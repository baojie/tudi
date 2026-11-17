# Flattened tree model (M2/M3)

Code: `crates/tudi-core/src/tree.rs`. Plan: `docs/PLAN.md` §5.2.

The left pane holds directories only. Nodes live in a slab (`Vec<Node>`,
`NodeId = u32`); nodes are never freed in M2 (refresh and removal arrive
with inotify in M3). The visible rows are `Vec<Row>` with
`Row { node, depth, kind }`, 8 bytes each, in display order.

## Node state

`Node { name, parent, depth, expanded, children }` where `children` is
`Unknown | Loading | Loaded(Vec<NodeId>) | Failed(ListFailure)`. A root's
name is its absolute path; other names are single components (bytes).
`Tree::path` joins them.

## Operations and splices

Every row change returns `Splice { position, removed, added }`, which the
GTK model forwards unchanged to `items-changed`.

- `expand(row)`: marks the node expanded. If its children are loaded, the
  visible subtree (children, and recursively the children of descendants that
  are still marked expanded) is inserted in one splice. If unknown or
  failed, the node becomes `Loading` and the caller gets `NeedsListing(id)`.
- `collapse(row)`: removes the contiguous range of deeper rows below it.
  Descendants keep `expanded`, so re-expanding restores them.
- `set_children(id, result)`: accepted only while the node is `Loading`
  (later results are stale and ignored). If the node is expanded and visible,
  its rows are inserted; otherwise the result is only stored.
- A failed listing shows one `RowKind::Error` row below the node (its `node`
  is the failed directory). Collapsing removes it; expanding again retries.

`row_of(id)` first checks that all ancestors are expanded, then scans the
row array. It runs once per completed listing; the scan touches 8 bytes per
row with no per-row work.

## Testing

`rows_match_reference_dfs` (proptest, 512 cases) runs random sequences of
expand, collapse and listing delivery (including failures) and checks after
every step that (1) the rows equal a naive depth-first walk of the node
state and (2) replaying every returned splice on a shadow array, as GTK
would, reproduces the rows exactly.
