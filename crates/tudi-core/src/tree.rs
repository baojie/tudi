//! The folder tree as a flattened row array (PLAN §5.2).
//!
//! The left pane shows directories only. Nodes live in a slab keyed by
//! [`NodeId`]; the visible rows are a `Vec<Row>` in display order. Every
//! change to the rows returns a [`Splice`] that maps 1:1 onto
//! `GListModel::items-changed`.
//!
//! Listing is asynchronous and lives outside this module: [`Tree::expand`]
//! asks for a listing ([`Expand::NeedsListing`]) and the result is delivered
//! with [`Tree::set_children`], which splices only if the node is still
//! expanded and visible. Expanded state of descendants is remembered while a
//! parent is collapsed and restored when it is expanded again.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

pub type NodeId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// The row shows the node itself.
    Node,
    /// The row shows why the node's children could not be listed (e.g.
    /// permission denied). It is the node's only child row.
    Error,
}

/// One visible row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub node: NodeId,
    pub depth: u16,
    pub kind: RowKind,
}

/// A change to the row array: at `position`, `removed` rows were replaced by
/// `added` rows (now at `rows()[position..position + added]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Splice {
    pub position: u32,
    pub removed: u32,
    pub added: u32,
}

/// Why a directory could not be listed (kept small and cloneable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListFailure {
    pub kind: std::io::ErrorKind,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Children {
    /// Never listed.
    Unknown,
    /// A listing is in flight.
    Loading,
    /// Sub-directories in display order.
    Loaded(Vec<NodeId>),
    Failed(ListFailure),
}

#[derive(Clone, Debug)]
pub struct Node {
    /// For a root: its absolute path. Otherwise: one name component.
    name: Box<[u8]>,
    parent: Option<NodeId>,
    depth: u16,
    expanded: bool,
    children: Children,
}

impl Node {
    pub fn name(&self) -> &OsStr {
        OsStr::from_bytes(&self.name)
    }

    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    pub fn depth(&self) -> u16 {
        self.depth
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub fn children(&self) -> &Children {
        &self.children
    }
}

/// Result of [`Tree::expand`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expand {
    /// Children were known; rows were inserted.
    Spliced(Splice),
    /// The caller must list this node's path and call [`Tree::set_children`].
    NeedsListing(NodeId),
    /// Nothing to do (already expanded, already loading, or an error row).
    Nothing,
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    nodes: Vec<Node>,
    rows: Vec<Row>,
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// Append a top-level root (e.g. Home, File System, a volume).
    pub fn add_root(&mut self, path: impl Into<OsString>) -> (NodeId, Splice) {
        let id = self.new_node(path.into().into_vec().into(), None, 0);
        let position = self.rows.len() as u32;
        self.rows.push(Row {
            node: id,
            depth: 0,
            kind: RowKind::Node,
        });
        (
            id,
            Splice {
                position,
                removed: 0,
                added: 1,
            },
        )
    }

    fn new_node(&mut self, name: Box<[u8]>, parent: Option<NodeId>, depth: u16) -> NodeId {
        let id = NodeId::try_from(self.nodes.len()).expect("fewer than 2^32 nodes");
        self.nodes.push(Node {
            name,
            parent,
            depth,
            expanded: false,
            children: Children::Unknown,
        });
        id
    }

    /// Absolute path of a node.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(n) = cur {
            parts.push(n);
            cur = self.nodes[n as usize].parent;
        }
        let mut p = PathBuf::new();
        for &n in parts.iter().rev() {
            p.push(self.node(n).name());
        }
        p
    }

    /// Expand the node shown at `row`.
    pub fn expand(&mut self, row: usize) -> Expand {
        let r = self.rows[row];
        if r.kind == RowKind::Error {
            return Expand::Nothing;
        }
        let node = &mut self.nodes[r.node as usize];
        if node.expanded {
            return Expand::Nothing;
        }
        node.expanded = true;
        match node.children {
            Children::Loading => Expand::Nothing,
            Children::Unknown | Children::Failed(_) => {
                // A failed listing is retried on the next expand.
                node.children = Children::Loading;
                Expand::NeedsListing(r.node)
            }
            Children::Loaded(_) => {
                let mut sub = Vec::new();
                self.push_visible_children(r.node, &mut sub);
                Expand::Spliced(self.insert(row + 1, sub))
            }
        }
    }

    /// Collapse the node shown at `row`. Descendants keep their expanded
    /// state. Returns the removal, if any rows were removed.
    pub fn collapse(&mut self, row: usize) -> Option<Splice> {
        let r = self.rows[row];
        let node = &mut self.nodes[r.node as usize];
        if r.kind == RowKind::Error || !node.expanded {
            return None;
        }
        node.expanded = false;
        let end = self.subtree_end(row);
        let removed = end - row - 1;
        if removed == 0 {
            return None;
        }
        self.rows.drain(row + 1..end);
        Some(Splice {
            position: row as u32 + 1,
            removed: removed as u32,
            added: 0,
        })
    }

    /// Deliver a listing result for `node`: its sub-directory names in
    /// display order, or the failure. Ignored unless the node is loading.
    /// Returns the inserted rows if the node is expanded and visible.
    pub fn set_children<N: AsRef<[u8]>>(
        &mut self,
        node: NodeId,
        result: Result<impl IntoIterator<Item = N>, ListFailure>,
    ) -> Option<Splice> {
        if self.nodes[node as usize].children != Children::Loading {
            return None;
        }
        let depth = self.nodes[node as usize].depth + 1;
        let children = match result {
            Ok(names) => Children::Loaded(
                names
                    .into_iter()
                    .map(|n| self.new_node(n.as_ref().into(), Some(node), depth))
                    .collect(),
            ),
            Err(f) => Children::Failed(f),
        };
        let n = &mut self.nodes[node as usize];
        n.children = children;
        if !n.expanded {
            return None;
        }
        let row = self.row_of(node)?;
        let mut sub = Vec::new();
        self.push_visible_children(node, &mut sub);
        Some(self.insert(row + 1, sub))
    }

    /// Replace the children of an already listed node with a fresh listing
    /// (live update). Children whose name is unchanged keep their node id
    /// and state (expanded, loaded descendants). A loading node is handled
    /// like [`Tree::set_children`]; a never-listed node is left alone.
    ///
    /// If the node is visible and expanded, the rows that differ are
    /// replaced in one splice spanning only the changed middle (common
    /// leading and trailing rows are kept, and with them their selection).
    pub fn refresh_children<N: AsRef<[u8]>>(
        &mut self,
        node: NodeId,
        result: Result<impl IntoIterator<Item = N>, ListFailure>,
    ) -> Option<Splice> {
        match self.nodes[node as usize].children {
            Children::Unknown => return None,
            Children::Loading => return self.set_children(node, result),
            Children::Loaded(_) | Children::Failed(_) => {}
        }
        let visible = if self.nodes[node as usize].expanded {
            self.row_of(node)
        } else {
            None
        };
        let old_range = visible.map(|row| row + 1..self.subtree_end(row));

        let depth = self.nodes[node as usize].depth + 1;
        let children = match result {
            Ok(names) => {
                let old: std::collections::HashMap<Box<[u8]>, NodeId> =
                    match &self.nodes[node as usize].children {
                        Children::Loaded(kids) => kids
                            .iter()
                            .map(|&k| (self.nodes[k as usize].name.clone(), k))
                            .collect(),
                        _ => Default::default(),
                    };
                let mut kids = Vec::new();
                for n in names {
                    let n = n.as_ref();
                    let id = match old.get(n) {
                        Some(&id) => id,
                        None => self.new_node(n.into(), Some(node), depth),
                    };
                    kids.push(id);
                }
                Children::Loaded(kids)
            }
            Err(f) => Children::Failed(f),
        };
        self.nodes[node as usize].children = children;

        let range = old_range?;
        let mut new_rows = Vec::new();
        self.push_visible_children(node, &mut new_rows);
        let old_rows = &self.rows[range.clone()];
        let prefix = old_rows
            .iter()
            .zip(&new_rows)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = old_rows[prefix..]
            .iter()
            .rev()
            .zip(new_rows[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let removed = old_rows.len() - prefix - suffix;
        let added = new_rows.len() - prefix - suffix;
        if removed == 0 && added == 0 {
            return None;
        }
        let at = range.start + prefix;
        self.rows.splice(
            at..at + removed,
            new_rows[prefix..prefix + added].iter().copied(),
        );
        Some(Splice {
            position: at as u32,
            removed: removed as u32,
            added: added as u32,
        })
    }

    /// Row index of a node, if it is visible.
    ///
    /// Checks that every ancestor is expanded first, then scans the row
    /// array: a linear scan of 8-byte rows (memory-bandwidth bound, no
    /// per-row work), used once per completed listing.
    pub fn row_of(&self, node: NodeId) -> Option<usize> {
        let mut cur = self.nodes[node as usize].parent;
        while let Some(p) = cur {
            let n = &self.nodes[p as usize];
            if !n.expanded {
                return None;
            }
            cur = n.parent;
        }
        self.rows
            .iter()
            .position(|r| r.node == node && r.kind == RowKind::Node)
    }

    /// End (exclusive) of the rows below `row` that belong to its subtree.
    fn subtree_end(&self, row: usize) -> usize {
        let depth = self.rows[row].depth;
        row + 1
            + self.rows[row + 1..]
                .iter()
                .take_while(|r| r.depth > depth)
                .count()
    }

    fn insert(&mut self, at: usize, rows: Vec<Row>) -> Splice {
        let added = rows.len() as u32;
        self.rows.splice(at..at, rows);
        Splice {
            position: at as u32,
            removed: 0,
            added,
        }
    }

    /// Rows below an expanded `id`: its children, recursively for children
    /// that are themselves expanded, or a single error row.
    fn push_visible_children(&self, id: NodeId, out: &mut Vec<Row>) {
        let node = &self.nodes[id as usize];
        match &node.children {
            Children::Loaded(kids) => {
                for &k in kids {
                    out.push(Row {
                        node: k,
                        depth: node.depth + 1,
                        kind: RowKind::Node,
                    });
                    if self.nodes[k as usize].expanded {
                        self.push_visible_children(k, out);
                    }
                }
            }
            Children::Failed(_) => out.push(Row {
                node: id,
                depth: node.depth + 1,
                kind: RowKind::Error,
            }),
            Children::Unknown | Children::Loading => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ok(names: &[&str]) -> Result<Vec<Vec<u8>>, ListFailure> {
        Ok(names.iter().map(|n| n.as_bytes().to_vec()).collect())
    }

    fn denied() -> ListFailure {
        ListFailure {
            kind: std::io::ErrorKind::PermissionDenied,
            message: "Permission denied".into(),
        }
    }

    fn labels(t: &Tree) -> Vec<String> {
        t.rows()
            .iter()
            .map(|r| {
                let name = t.node(r.node).name().to_string_lossy();
                let pad = "  ".repeat(r.depth as usize);
                match r.kind {
                    RowKind::Node => format!("{pad}{name}"),
                    RowKind::Error => format!("{pad}!{name}"),
                }
            })
            .collect()
    }

    #[test]
    fn expand_lists_then_splices() {
        let mut t = Tree::new();
        let (root, s) = t.add_root("/r");
        assert_eq!(
            s,
            Splice {
                position: 0,
                removed: 0,
                added: 1
            }
        );
        assert_eq!(t.expand(0), Expand::NeedsListing(root));
        assert_eq!(t.expand(0), Expand::Nothing, "already expanded");
        let s = t.set_children(root, ok(&["a", "b"])).unwrap();
        assert_eq!(
            s,
            Splice {
                position: 1,
                removed: 0,
                added: 2
            }
        );
        assert_eq!(labels(&t), ["/r", "  a", "  b"]);
        assert_eq!(t.path(t.rows()[2].node), PathBuf::from("/r/b"));
    }

    #[test]
    fn collapse_remembers_descendants() {
        let mut t = Tree::new();
        let (root, _) = t.add_root("/r");
        t.expand(0);
        t.set_children(root, ok(&["a", "b"]));
        let a = t.rows()[1].node;
        t.expand(1);
        t.set_children(a, ok(&["x", "y"]));
        assert_eq!(labels(&t), ["/r", "  a", "    x", "    y", "  b"]);
        let s = t.collapse(0).unwrap();
        assert_eq!(
            s,
            Splice {
                position: 1,
                removed: 4,
                added: 0
            }
        );
        assert_eq!(labels(&t), ["/r"]);
        let Expand::Spliced(s) = t.expand(0) else {
            panic!()
        };
        assert_eq!(
            s,
            Splice {
                position: 1,
                removed: 0,
                added: 4
            }
        );
        assert_eq!(labels(&t), ["/r", "  a", "    x", "    y", "  b"]);
    }

    #[test]
    fn failure_shows_an_error_row_and_retries() {
        let mut t = Tree::new();
        let (root, _) = t.add_root("/locked");
        t.expand(0);
        let s = t
            .set_children(root, Err::<Vec<Vec<u8>>, _>(denied()))
            .unwrap();
        assert_eq!(s.added, 1);
        assert_eq!(labels(&t), ["/locked", "  !/locked"]);
        assert_eq!(t.expand(1), Expand::Nothing, "error rows do not expand");
        assert!(
            matches!(t.node(root).children(), Children::Failed(f) if f.kind == std::io::ErrorKind::PermissionDenied)
        );
        t.collapse(0);
        assert_eq!(t.expand(0), Expand::NeedsListing(root));
    }

    #[test]
    fn result_for_collapsed_or_hidden_node_does_not_splice() {
        let mut t = Tree::new();
        let (root, _) = t.add_root("/r");
        t.expand(0);
        t.set_children(root, ok(&["a"]));
        let a = t.rows()[1].node;
        t.expand(1); // a: loading
        t.collapse(0); // a hidden, still expanded
        assert_eq!(t.set_children(a, ok(&["x"])), None);
        let Expand::Spliced(s) = t.expand(0) else {
            panic!()
        };
        assert_eq!(s.added, 2);
        assert_eq!(labels(&t), ["/r", "  a", "    x"]);
        // A second, stale result is ignored.
        assert_eq!(t.set_children(a, ok(&["z"])), None);
    }

    #[test]
    fn refresh_keeps_state_and_splices_only_the_change() {
        let mut t = Tree::new();
        let (root, _) = t.add_root("/r");
        t.expand(0);
        t.set_children(root, ok(&["a", "b", "d"]));
        let b = t.rows()[2].node;
        t.expand(2);
        t.set_children(b, ok(&["x"]));
        assert_eq!(labels(&t), ["/r", "  a", "  b", "    x", "  d"]);
        // "c" created between b and d: one row inserted, b keeps its state.
        let s = t.refresh_children(root, ok(&["a", "b", "c", "d"])).unwrap();
        assert_eq!(
            s,
            Splice {
                position: 4,
                removed: 0,
                added: 1
            }
        );
        assert_eq!(labels(&t), ["/r", "  a", "  b", "    x", "  c", "  d"]);
        assert_eq!(t.rows()[2].node, b);
        // "a" deleted.
        let s = t.refresh_children(root, ok(&["b", "c", "d"])).unwrap();
        assert_eq!(
            s,
            Splice {
                position: 1,
                removed: 1,
                added: 0
            }
        );
        // No change: no splice.
        assert_eq!(t.refresh_children(root, ok(&["b", "c", "d"])), None);
        // Collapsed: state updates, no splice.
        t.collapse(0);
        assert_eq!(t.refresh_children(root, ok(&["b"])), None);
        t.expand(0);
        assert_eq!(labels(&t), ["/r", "  b", "    x"]);
    }

    #[test]
    fn non_utf8_names_and_paths() {
        let mut t = Tree::new();
        let (root, _) = t.add_root("/r");
        t.expand(0);
        t.set_children(root, Ok::<_, ListFailure>(vec![b"caf\xe9".to_vec()]));
        let p = t.path(t.rows()[1].node);
        assert_eq!(p.as_os_str().as_bytes(), b"/r/caf\xe9");
    }

    /// Naive reference: depth-first walk of what should be visible.
    fn reference(t: &Tree, roots: &[NodeId]) -> Vec<Row> {
        fn walk(t: &Tree, id: NodeId, out: &mut Vec<Row>) {
            let n = t.node(id);
            out.push(Row {
                node: id,
                depth: n.depth(),
                kind: RowKind::Node,
            });
            if !n.is_expanded() {
                return;
            }
            match n.children() {
                Children::Loaded(kids) => kids.iter().for_each(|&k| walk(t, k, out)),
                Children::Failed(_) => out.push(Row {
                    node: id,
                    depth: n.depth() + 1,
                    kind: RowKind::Error,
                }),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for &r in roots {
            walk(t, r, &mut out);
        }
        out
    }

    #[derive(Clone, Debug)]
    enum Op {
        Expand(usize),
        Collapse(usize),
        /// Deliver a listing to one pending node: `None` = failure.
        Deliver(usize, Option<u8>),
        /// Live update of a listed node: keep each old child with the
        /// given probability mask, add `new` children; `None` = failure.
        Refresh(usize, Option<(u64, u8)>),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => any::<usize>().prop_map(Op::Expand),
            1 => any::<usize>().prop_map(Op::Collapse),
            3 => (any::<usize>(), prop::option::weighted(0.85, 0u8..6)).prop_map(|(i, n)| Op::Deliver(i, n)),
            2 => (any::<usize>(), prop::option::weighted(0.9, (any::<u64>(), 0u8..3))).prop_map(|(i, r)| Op::Refresh(i, r)),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Random expand/collapse/listing sequences keep the row array equal
        /// to a reference DFS, and replaying the splice triples on a shadow
        /// array (as GTK would) reproduces it exactly.
        #[test]
        fn rows_match_reference_dfs(n_roots in 1usize..4, ops in prop::collection::vec(op(), 1..80)) {
            let mut t = Tree::new();
            let mut shadow: Vec<Row> = Vec::new();
            let mut roots = Vec::new();
            for i in 0..n_roots {
                let (id, s) = t.add_root(format!("/root{i}"));
                roots.push(id);
                shadow.splice(s.position as usize..s.position as usize, t.rows()[s.position as usize..][..s.added as usize].iter().copied());
            }
            let mut pending: Vec<NodeId> = Vec::new();
            for op in ops {
                let splice = match op {
                    Op::Expand(i) => {
                        let row = i % t.rows().len();
                        match t.expand(row) {
                            Expand::Spliced(s) => Some(s),
                            Expand::NeedsListing(id) => { pending.push(id); None }
                            Expand::Nothing => None,
                        }
                    }
                    Op::Collapse(i) => t.collapse(i % t.rows().len()),
                    Op::Refresh(i, r) => {
                        let listed: Vec<NodeId> = (0..t.nodes.len() as NodeId)
                            .filter(|&id| matches!(t.node(id).children(), Children::Loaded(_) | Children::Failed(_)))
                            .collect();
                        if listed.is_empty() { None } else {
                            let id = listed[i % listed.len()];
                            let res = match r {
                                Some((keep, new)) => {
                                    let mut names: Vec<Vec<u8>> = match t.node(id).children() {
                                        Children::Loaded(kids) => kids
                                            .iter()
                                            .enumerate()
                                            .filter(|(k, _)| keep >> (k % 64) & 1 == 1)
                                            .map(|(_, &k)| t.node(k).name().as_bytes().to_vec())
                                            .collect(),
                                        _ => Vec::new(),
                                    };
                                    let serial = t.nodes.len();
                                    names.extend((0..new).map(|k| format!("n{serial}-{k}").into_bytes()));
                                    names.sort();
                                    Ok(names)
                                }
                                None => Err(denied()),
                            };
                            t.refresh_children(id, res)
                        }
                    }
                    Op::Deliver(i, n) => {
                        if pending.is_empty() { None } else {
                            let id = pending.swap_remove(i % pending.len());
                            let res = match n {
                                Some(n) => Ok((0..n).map(|k| format!("d{k}").into_bytes()).collect::<Vec<_>>()),
                                None => Err(denied()),
                            };
                            t.set_children(id, res)
                        }
                    }
                };
                if let Some(s) = splice {
                    let p = s.position as usize;
                    let added = t.rows()[p..p + s.added as usize].to_vec();
                    shadow.splice(p..p + s.removed as usize, added);
                }
                prop_assert_eq!(&shadow, &t.rows().to_vec());
                prop_assert_eq!(t.rows().to_vec(), reference(&t, &roots));
            }
        }
    }
}
