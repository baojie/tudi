# Query syntax

The memory search (`Ctrl+P`, `Ctrl+Shift+F`, `tudi q`) takes
whitespace-separated tokens in any order; an item is a result when **all**
of them hold. Code: `crates/tudi-index/src/query.rs` (parser) and
`matcher.rs` (`search_query`). Examples:

```text
合同 .pdf 上月          PDFs changed last month with 合同 in the name or a folder above
in:acme report         "report" below a folder matching "acme"
.md <7d                Markdown files changed in the last seven days
size>1G                files over 1 GB
dir: tudi              folders matching "tudi"
"annual plan" date:2025-06
```

## Fragments

| Form | Meaning |
|---|---|
| `report` | fuzzy fragment of the item's name or of a folder above it (pinyin initials, full pinyin and small typos work, ADR-013) |
| `"annual plan"` | one fragment containing a space |
| `in:acme`, `/acme/` | a folder **above** the item matches `acme` (not the item itself) |

At least one plain fragment must match the item's own name; the others may
match its folders. A query with filters only (`.pdf 上月`) lists every item
that passes them.

## Filters

| Form | Meaning |
|---|---|
| `.pdf` | extension `pdf` (any case); also names starting with `.pdf` (so `.bashrc` finds the dotfile) |
| `ext:pdf,docx` | any of these extensions |
| `dir:` / `folder:` / `文件夹:` | folders only; `dir:acme` also adds the fragment `acme` |
| `file:` / `文件:` | files only |
| `size>10M`, `size<500k`, `size:>1.5G` | size in decimal units B, K, M, G, T (files only) |

## Time (modification date)

| Form | Meaning |
|---|---|
| `<7d`, `<12h` | changed within the last 7 days / 12 hours |
| `>2w` | changed more than two weeks ago |
| units | `h` hours, `d` days, `w` weeks, `mo` months (30 days), `y` years (365 days) |
| `>2025-06`, `>2025-06-15` | changed on or after that month / day |
| `<2025`, `<2025-06-15` | changed before that year / day |
| `date:2025`, `date:2025-06`, `date:2025-06-15` | changed in that year / month / day |
| `today` `今天` | since midnight |
| `yesterday` `昨天` | the day before today |
| `week` `本周` | since Monday |
| `lastweek` `上周` | the previous Monday to Sunday |
| `month` `本月` | since the 1st |
| `lastmonth` `上月` | the previous calendar month |
| `year` `今年` / `lastyear` `去年` | this / the previous calendar year |

Calendar words use the local time zone (including daylight saving time).
Several time filters intersect: `<30d >1w` is "between one week and thirty
days ago".

## Mistakes

A filter that cannot be read does not fail the search: it is ignored and a
hint appears under the search field (on stderr for `tudi q`), e.g.
`“<7m”: months are “mo” (<3mo); for sizes use size>10M`. Words that only
start like a filter (`sizes`, `sizeof.h`) are ordinary fragments.

## Scope

`Ctrl+Shift+F` searches below the selected folder (or the shown one); the
folder appears as a chip above the field, and `×` or `Backspace` in the
empty field searches everywhere again. On the command line:
`tudi q --in DIR QUERY…`.

## Shell

`tudi init bash|zsh|fish [--cmd NAME] [--hook]` prints a function `tj`
(or `NAME`) that changes to the best folder match: `tj acme rep`. With
`--hook`, every `cd` is recorded in the folder history (`tudi visit`), like
opening the folder in tudi; the folder a shell starts in is not counted.

```sh
eval "$(tudi init bash --hook)"     # ~/.bashrc
eval "$(tudi init zsh --hook)"      # ~/.zshrc
tudi init fish --hook | source      # ~/.config/fish/config.fish
```
