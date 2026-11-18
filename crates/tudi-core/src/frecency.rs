//! Folder history: visit counts and zoxide-style frecency (PLAN §5.5,
//! ADR-006, ADR-016), with exclusions.
//!
//! One SQLite database in WAL mode, so the GUI and a shell hook can both
//! write. Synchronous API: callers run it off the GTK main thread.
//!
//! Ranking: each visit adds 1 to `rank`; when the sum of ranks exceeds
//! [`MAX_TOTAL_RANK`] all ranks are scaled down and tiny ones dropped
//! (aging). The score multiplies `rank` by a recency factor of the last
//! visit: ×4 within an hour, ×2 within a day, ×0.5 within a week, ×0.25
//! older. `count` is the raw number of visits, never aged, for display.

use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use rusqlite::{Connection, OptionalExtension, params};

/// Aging threshold for the sum of all ranks (zoxide's default).
pub const MAX_TOTAL_RANK: f64 = 10_000.0;

#[derive(Debug, thiserror::Error)]
pub enum FrecencyError {
    #[error("history database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("invalid history exclude pattern {pattern:?}: {source}")]
    Pattern {
        pattern: String,
        #[source]
        source: globset::Error,
    },
}

/// One folder in the history.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    /// Visits ever recorded (never aged).
    pub count: u64,
    /// Last visit, seconds since the Unix epoch.
    pub last: i64,
    pub score: f64,
}

/// An exclusion added from the GUI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exclusion {
    pub path: PathBuf,
    /// Also excludes everything below `path`.
    pub recursive: bool,
}

/// Glob rules from `[history] exclude`, matched against absolute paths.
#[derive(Clone, Debug)]
pub struct ExcludeRules(GlobSet);

impl Default for ExcludeRules {
    fn default() -> Self {
        Self(GlobSet::empty())
    }
}

impl ExcludeRules {
    pub fn new(patterns: &[String]) -> Result<Self, FrecencyError> {
        let mut b = GlobSetBuilder::new();
        for p in patterns {
            let g = Glob::new(p).map_err(|source| FrecencyError::Pattern {
                pattern: p.clone(),
                source,
            })?;
            b.add(g);
        }
        b.build()
            .map(Self)
            .map_err(|source| FrecencyError::Pattern {
                pattern: patterns.join(", "),
                source,
            })
    }

    pub fn matches(&self, path: &Path) -> bool {
        self.0.is_match(path)
    }
}

/// Recency factor for a visit `age` seconds ago.
pub fn recency_factor(age: i64) -> f64 {
    if age <= 3_600 {
        4.0
    } else if age <= 86_400 {
        2.0
    } else if age <= 604_800 {
        0.5
    } else {
        0.25
    }
}

fn bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_bytes()
}

fn path(b: Vec<u8>) -> PathBuf {
    PathBuf::from(std::ffi::OsString::from_vec(b))
}

/// `prefix` followed by `/`, for "is below" range queries on byte paths.
fn below_bounds(dir: &Path) -> (Vec<u8>, Vec<u8>) {
    let mut lo = bytes(dir).to_vec();
    if lo.last() != Some(&b'/') {
        lo.push(b'/');
    }
    let mut hi = lo.clone();
    *hi.last_mut().unwrap() = b'/' + 1;
    (lo, hi)
}

pub struct Frecency {
    db: Connection,
    rules: ExcludeRules,
    max_total: f64,
}

impl Frecency {
    /// Open (creating if needed) the database at `path`.
    pub fn open(path: &Path, rules: ExcludeRules) -> Result<Self, FrecencyError> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?, rules)
    }

    pub fn open_in_memory(rules: ExcludeRules) -> Result<Self, FrecencyError> {
        Self::init(Connection::open_in_memory()?, rules)
    }

    fn init(db: Connection, rules: ExcludeRules) -> Result<Self, FrecencyError> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.busy_timeout(std::time::Duration::from_millis(500))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS dirs (
                 path  BLOB PRIMARY KEY,
                 rank  REAL NOT NULL,
                 count INTEGER NOT NULL,
                 last  INTEGER NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS excluded (
                 path      BLOB PRIMARY KEY,
                 recursive INTEGER NOT NULL
             ) WITHOUT ROWID;",
        )?;
        Ok(Self {
            db,
            rules,
            max_total: MAX_TOTAL_RANK,
        })
    }

    pub fn set_rules(&mut self, rules: ExcludeRules) {
        self.rules = rules;
    }

    /// Whether `dir` is excluded by a rule or a stored exclusion (itself,
    /// or a recursive exclusion of an ancestor).
    pub fn is_excluded(&self, dir: &Path) -> Result<bool, FrecencyError> {
        if self.rules.matches(dir) {
            return Ok(true);
        }
        let mut stmt = self
            .db
            .prepare_cached("SELECT recursive FROM excluded WHERE path = ?1")?;
        if stmt
            .query_row(params![bytes(dir)], |r| r.get::<_, bool>(0))
            .optional()?
            .is_some()
        {
            return Ok(true);
        }
        let mut cur = dir.parent();
        while let Some(a) = cur {
            if stmt
                .query_row(params![bytes(a)], |r| r.get::<_, bool>(0))
                .optional()?
                == Some(true)
            {
                return Ok(true);
            }
            cur = a.parent();
        }
        Ok(false)
    }

    /// Record a deliberate visit of `dir` at `now`. Returns false if the
    /// folder is excluded (nothing recorded).
    pub fn visit(&mut self, dir: &Path, now: i64) -> Result<bool, FrecencyError> {
        if self.is_excluded(dir)? {
            return Ok(false);
        }
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO dirs (path, rank, count, last) VALUES (?1, 1, 1, ?2)
             ON CONFLICT(path) DO UPDATE SET
                 rank = rank + 1, count = count + 1, last = excluded.last",
            params![bytes(dir), now],
        )?;
        let total: f64 =
            tx.query_row("SELECT COALESCE(SUM(rank), 0) FROM dirs", [], |r| r.get(0))?;
        if total > self.max_total {
            let factor = 0.9 * self.max_total / total;
            tx.execute("UPDATE dirs SET rank = rank * ?1", params![factor])?;
            tx.execute("DELETE FROM dirs WHERE rank < 1", [])?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// The `limit` best folders by score at `now`, best first; ties by
    /// count, then path.
    pub fn top(&self, limit: usize, now: i64) -> Result<Vec<Entry>, FrecencyError> {
        let mut stmt = self
            .db
            .prepare_cached("SELECT path, rank, count, last FROM dirs")?;
        let mut all: Vec<Entry> = stmt
            .query_map([], |r| {
                let rank: f64 = r.get(1)?;
                let last: i64 = r.get(3)?;
                Ok(Entry {
                    path: path(r.get(0)?),
                    count: r.get::<_, i64>(2)? as u64,
                    last,
                    score: rank * recency_factor(now - last),
                })
            })?
            .collect::<Result<_, _>>()?;
        // Rules may have changed since the visits were recorded.
        all.retain(|e| !self.rules.matches(&e.path));
        all.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(b.count.cmp(&a.count))
                .then_with(|| a.path.cmp(&b.path))
        });
        all.truncate(limit);
        Ok(all)
    }

    /// Exclude `dir` (and, if `recursive`, everything below it) and delete
    /// the matching records.
    pub fn exclude(&mut self, dir: &Path, recursive: bool) -> Result<(), FrecencyError> {
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO excluded (path, recursive) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET recursive = excluded.recursive",
            params![bytes(dir), recursive],
        )?;
        tx.execute("DELETE FROM dirs WHERE path = ?1", params![bytes(dir)])?;
        if recursive {
            let (lo, hi) = below_bounds(dir);
            tx.execute(
                "DELETE FROM dirs WHERE path >= ?1 AND path < ?2",
                params![lo, hi],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove a stored exclusion. Returns whether one existed.
    pub fn unexclude(&mut self, dir: &Path) -> Result<bool, FrecencyError> {
        Ok(self
            .db
            .execute("DELETE FROM excluded WHERE path = ?1", params![bytes(dir)])?
            > 0)
    }

    pub fn exclusions(&self) -> Result<Vec<Exclusion>, FrecencyError> {
        let mut stmt = self
            .db
            .prepare_cached("SELECT path, recursive FROM excluded ORDER BY path")?;
        let v = stmt
            .query_map([], |r| {
                Ok(Exclusion {
                    path: path(r.get(0)?),
                    recursive: r.get(1)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Forget all visits (exclusions are kept).
    pub fn clear(&mut self) -> Result<(), FrecencyError> {
        self.db.execute("DELETE FROM dirs", [])?;
        Ok(())
    }

    /// Forget one folder's history (it may be recorded again later).
    pub fn forget(&mut self, dir: &Path) -> Result<(), FrecencyError> {
        self.db
            .execute("DELETE FROM dirs WHERE path = ?1", params![bytes(dir)])?;
        Ok(())
    }
}

/// `$XDG_DATA_HOME/tudi/frecency.sqlite`, falling back to
/// `~/.local/share/tudi/frecency.sqlite`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("tudi/frecency.sqlite"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn store() -> Frecency {
        Frecency::open_in_memory(ExcludeRules::default()).unwrap()
    }

    fn paths(v: &[Entry]) -> Vec<&str> {
        v.iter().map(|e| e.path.to_str().unwrap()).collect()
    }

    #[test]
    fn counts_and_orders_by_frecency() {
        let mut f = store();
        for _ in 0..5 {
            f.visit(Path::new("/w/old"), NOW - 30 * DAY).unwrap();
        }
        for _ in 0..2 {
            f.visit(Path::new("/w/new"), NOW - 60).unwrap();
        }
        f.visit(Path::new("/w/once"), NOW - 2 * DAY).unwrap();
        let top = f.top(10, NOW).unwrap();
        // new: 2×4 = 8; old: 5×0.25 = 1.25; once: 1×0.5.
        assert_eq!(paths(&top), ["/w/new", "/w/old", "/w/once"]);
        assert_eq!(top[1].count, 5);
        assert_eq!(top[1].last, NOW - 30 * DAY);
        assert_eq!(f.top(1, NOW).unwrap().len(), 1);
    }

    #[test]
    fn recursive_exclusion_deletes_and_blocks() {
        let mut f = store();
        for p in ["/w/a", "/w/a/b", "/w/ab", "/w/c"] {
            f.visit(Path::new(p), NOW).unwrap();
        }
        f.exclude(Path::new("/w/a"), true).unwrap();
        // "/w/ab" is a sibling, not below "/w/a".
        assert_eq!(paths(&f.top(10, NOW).unwrap()), ["/w/ab", "/w/c"]);
        assert!(!f.visit(Path::new("/w/a/b/c"), NOW).unwrap());
        assert!(!f.visit(Path::new("/w/a"), NOW).unwrap());
        assert!(f.visit(Path::new("/w/ab"), NOW).unwrap());
        assert_eq!(
            f.exclusions().unwrap(),
            [Exclusion {
                path: "/w/a".into(),
                recursive: true
            }]
        );
        assert!(f.unexclude(Path::new("/w/a")).unwrap());
        f.clear().unwrap();
        assert!(f.top(10, NOW).unwrap().is_empty());
        f.exclude(Path::new("/w/x"), false).unwrap();
        f.clear().unwrap();
        assert_eq!(f.exclusions().unwrap().len(), 1, "clear keeps exclusions");
        assert!(f.visit(Path::new("/w/a/b"), NOW).unwrap());
    }

    #[test]
    fn non_recursive_exclusion_keeps_children() {
        let mut f = store();
        f.exclude(Path::new("/w/a"), false).unwrap();
        assert!(!f.visit(Path::new("/w/a"), NOW).unwrap());
        assert!(f.visit(Path::new("/w/a/b"), NOW).unwrap());
    }

    #[test]
    fn glob_rules() {
        let rules = ExcludeRules::new(&["/tmp/**".into(), "**/node_modules/**".into()]).unwrap();
        let mut f = Frecency::open_in_memory(rules).unwrap();
        assert!(!f.visit(Path::new("/tmp/x"), NOW).unwrap());
        assert!(!f.visit(Path::new("/w/p/node_modules/lib"), NOW).unwrap());
        assert!(f.visit(Path::new("/w/p"), NOW).unwrap());
        assert!(ExcludeRules::new(&["[".into()]).is_err());
    }

    #[test]
    fn rules_added_later_hide_existing_records() {
        let mut f = store();
        f.visit(Path::new("/tmp/x"), NOW).unwrap();
        f.set_rules(ExcludeRules::new(&["/tmp/**".into()]).unwrap());
        assert!(f.top(10, NOW).unwrap().is_empty());
    }

    #[test]
    fn aging_keeps_the_total_bounded() {
        let mut f = store();
        f.max_total = 100.0;
        for i in 0..20 {
            for _ in 0..12 {
                f.visit(Path::new(&format!("/d{i}")), NOW).unwrap();
            }
        }
        let total: f64 = f
            .top(1000, NOW)
            .unwrap()
            .iter()
            .map(|e| e.score / 4.0)
            .sum();
        assert!(total <= 100.0, "{total}");
        // Ranks are aged, counts are not: the last folder still shows all
        // 12 visits although its rank was scaled down. Folders whose rank
        // fell below 1 were dropped (their count restarts if revisited).
        let top = f.top(1000, NOW).unwrap();
        let last = top.iter().find(|e| e.path == Path::new("/d19")).unwrap();
        assert_eq!(last.count, 12);
        assert!(last.score / 4.0 < 12.0);

        // A folder visited once disappears after enough aging.
        let mut f = store();
        f.max_total = 10.0;
        f.visit(Path::new("/rare"), NOW).unwrap();
        for _ in 0..50 {
            f.visit(Path::new("/busy"), NOW).unwrap();
        }
        assert_eq!(paths(&f.top(10, NOW).unwrap()), ["/busy"]);
    }

    #[test]
    fn non_utf8_paths_and_file_db() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("sub/frecency.sqlite");
        let p = path(b"/w/caf\xe9".to_vec());
        {
            let mut f = Frecency::open(&db, ExcludeRules::default()).unwrap();
            f.visit(&p, NOW).unwrap();
        }
        let f = Frecency::open(&db, ExcludeRules::default()).unwrap();
        assert_eq!(f.top(10, NOW).unwrap()[0].path, p);
    }
}
