//! Background work for the GUI: listing, sorting and filtering run on worker
//! threads; results come back to the GTK main loop over a channel.
//!
//! The main thread never touches the filesystem (AGENTS.md rule 1); it sends
//! a [`Job`] and later receives a [`Reply`].

use std::path::PathBuf;
use std::sync::Arc;

use rustix::fs::{AtFlags, StatxFlags};
use tudi_core::listing::meta::{MetaBatch, MetaTable};
use tudi_core::listing::sort::{NameRank, SortSpec, order};
use tudi_core::listing::{Kind, ListError, Lister, Listing};
use tudi_core::tree::{ListFailure, NodeId};

/// Number of listing threads. Listing is mostly kernel time; two threads let
/// a tree expansion proceed while a large directory is being read.
const WORKERS: usize = 2;

pub enum Job {
    /// List a directory for the details pane.
    ListPane {
        path: PathBuf,
        generation: u64,
        spec: SortSpec,
        show_hidden: bool,
        /// Names of entries whose positions should be reported (to keep or
        /// set the selection).
        select: Vec<Vec<u8>>,
    },
    /// Recompute the pane order (sort change, hidden toggle, metadata).
    Sort {
        generation: u64,
        listing: Arc<Listing>,
        rank: Arc<NameRank>,
        metas: MetaTable,
        spec: SortSpec,
        show_hidden: bool,
    },
    /// List the sub-directories of a tree node. With `refresh`, the node
    /// was listed before and this is a live update.
    ListTree {
        node: NodeId,
        path: PathBuf,
        show_hidden: bool,
        refresh: bool,
    },
}

/// A display order: entry indices, and each entry's position (`u32::MAX`
/// when filtered out).
pub struct Order {
    pub order: Vec<u32>,
    pub pos_of: Vec<u32>,
    /// Shown folders and other entries (for the status bar).
    pub dirs: u32,
    pub files: u32,
}

pub struct PaneListing {
    pub generation: u64,
    pub listing: Arc<Listing>,
    pub rank: Arc<NameRank>,
    pub order: Order,
    /// Positions of the requested `select` entries that are visible.
    pub select: Vec<u32>,
}

pub enum Reply {
    Pane(PaneListing),
    PaneError {
        generation: u64,
        error: ListError,
    },
    Sorted {
        generation: u64,
        order: Order,
    },
    Tree {
        node: NodeId,
        refresh: bool,
        result: Result<Vec<Box<[u8]>>, ListFailure>,
    },
    Meta(MetaBatch),
    /// Total size below the shown folder (`None`: took over a second).
    FolderSize {
        generation: u64,
        size: Option<u64>,
        /// Free space for unprivileged users on the folder's filesystem.
        available: Option<u64>,
    },
    /// Watched directories changed.
    Changed(Vec<WatchKey>),
    /// Results from the index daemon (search overlay).
    Search {
        id: u64,
        items: Vec<tudi_proto::ResultItem>,
        partial: bool,
        error: Option<String>,
    },
    /// The folder history (for the "Frequent" view).
    History(crate::history::Snapshot),
}

/// What a directory watch belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WatchKey {
    /// The folder shown in the details list.
    Pane,
    /// An expanded folder in the tree.
    Tree(NodeId),
}

#[derive(Clone)]
pub struct Worker {
    jobs: async_channel::Sender<Job>,
    replies: async_channel::Sender<Reply>,
}

impl Worker {
    /// Start the worker threads. Replies arrive on the returned receiver.
    pub fn start() -> (Self, async_channel::Receiver<Reply>) {
        let (jobs, job_rx) = async_channel::unbounded::<Job>();
        let (replies, reply_rx) = async_channel::unbounded::<Reply>();
        for i in 0..WORKERS {
            let (rx, tx) = (job_rx.clone(), replies.clone());
            std::thread::Builder::new()
                .name(format!("tudi-list-{i}"))
                .spawn(move || run(rx, tx))
                .expect("spawn listing thread");
        }
        (Self { jobs, replies }, reply_rx)
    }

    pub fn send(&self, job: Job) {
        // The workers live as long as the process; a send cannot fail.
        let _ = self.jobs.send_blocking(job);
    }

    /// A sender for replies produced elsewhere (e.g. the directory watcher).
    pub fn reply_sender(&self) -> async_channel::Sender<Reply> {
        self.replies.clone()
    }

    /// Compute the total size below `path` on its own thread, giving up
    /// after one second or when `cancel` is set. Replies `FolderSize`.
    pub fn folder_size(
        &self,
        path: PathBuf,
        generation: u64,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) {
        let tx = self.replies.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            let available = rustix::fs::statvfs(&path)
                .ok()
                .map(|v| v.f_bavail.saturating_mul(v.f_frsize));
            let _ = tx.send_blocking(Reply::FolderSize {
                generation,
                size: None,
                available,
            });
            let size = tudi_core::listing::meta::tree_size(&path, deadline, &cancel);
            let _ = tx.send_blocking(Reply::FolderSize {
                generation,
                size,
                available,
            });
        });
    }

    /// A sink for metadata batches, forwarding them as [`Reply::Meta`].
    pub fn meta_sink(&self) -> tudi_core::listing::meta::Sink {
        let tx = self.replies.clone();
        Arc::new(move |b| {
            let _ = tx.send_blocking(Reply::Meta(b));
        })
    }
}

fn run(jobs: async_channel::Receiver<Job>, replies: async_channel::Sender<Reply>) {
    let mut lister = Lister::new();
    while let Ok(job) = jobs.recv_blocking() {
        let reply = match job {
            Job::ListPane {
                path,
                generation,
                spec,
                show_hidden,
                select,
            } => match lister.list(&path, generation) {
                Ok(listing) => {
                    let rank = NameRank::new(&listing);
                    let metas = MetaTable::new(listing.len());
                    let order = filtered_order(&listing, &rank, &metas, spec, show_hidden);
                    let select = positions_of(&listing, &order, &select);
                    Reply::Pane(PaneListing {
                        generation,
                        listing: Arc::new(listing),
                        rank: Arc::new(rank),
                        order,
                        select,
                    })
                }
                Err(error) => Reply::PaneError { generation, error },
            },
            Job::Sort {
                generation,
                listing,
                rank,
                metas,
                spec,
                show_hidden,
            } => Reply::Sorted {
                generation,
                order: filtered_order(&listing, &rank, &metas, spec, show_hidden),
            },
            Job::ListTree {
                node,
                path,
                show_hidden,
                refresh,
            } => Reply::Tree {
                node,
                refresh,
                result: list_subdirs(&mut lister, &path, show_hidden),
            },
        };
        if replies.send_blocking(reply).is_err() {
            return;
        }
    }
}

/// Visible positions of the entries named in `names`.
fn positions_of(listing: &Listing, order: &Order, names: &[Vec<u8>]) -> Vec<u32> {
    if names.is_empty() {
        return Vec::new();
    }
    let wanted: std::collections::HashSet<&[u8]> = names.iter().map(|n| n.as_slice()).collect();
    (0..listing.len())
        .filter(|&i| wanted.contains(listing.name(i)))
        .map(|i| order.pos_of[i])
        .filter(|&p| p != u32::MAX)
        .collect()
}

pub fn is_hidden(name: &[u8]) -> bool {
    name.first() == Some(&b'.')
}

fn filtered_order(
    listing: &Listing,
    rank: &NameRank,
    metas: &MetaTable,
    spec: SortSpec,
    show_hidden: bool,
) -> Order {
    let mut order = order(listing, rank, metas, spec);
    if !show_hidden {
        order.retain(|&i| !is_hidden(listing.name(i as usize)));
    }
    let mut pos_of = vec![u32::MAX; listing.len()];
    let mut dirs = 0;
    for (p, &i) in order.iter().enumerate() {
        pos_of[i as usize] = p as u32;
        if metas.kind(listing, i as usize) == Kind::Dir {
            dirs += 1;
        }
    }
    let files = order.len() as u32 - dirs;
    Order {
        order,
        pos_of,
        dirs,
        files,
    }
}

/// Sub-directories of `path` in natural order, for the folder tree.
/// Entries of unknown type and symlinks are resolved with `statx`
/// (following symlinks, so a symlink to a directory is shown as one).
fn list_subdirs(
    lister: &mut Lister,
    path: &std::path::Path,
    show_hidden: bool,
) -> Result<Vec<Box<[u8]>>, ListFailure> {
    let listing = lister.list(path, 0).map_err(|e| ListFailure {
        kind: e.kind(),
        message: e.source.to_string(),
    })?;
    let is_dir = |i: usize| match listing.kind(i) {
        Kind::Dir => true,
        Kind::Unknown | Kind::Symlink => rustix::fs::statx(
            listing.dir(),
            listing.name_cstr(i),
            AtFlags::STATX_DONT_SYNC,
            StatxFlags::TYPE,
        )
        .is_ok_and(|st| u32::from(st.stx_mode) & 0o170000 == 0o040000),
        _ => false,
    };
    let rank = NameRank::new(&listing);
    Ok(rank
        .sorted()
        .iter()
        .map(|&i| i as usize)
        .filter(|&i| show_hidden || !is_hidden(listing.name(i)))
        .filter(|&i| is_dir(i))
        .map(|i| listing.name(i).into())
        .collect())
}
