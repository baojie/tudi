//! Core of tudi: directory listing, the flattened tree, the query matcher,
//! transliteration, frecency, and configuration.
//!
//! This crate must not depend on GTK.

pub mod bookmarks;
pub mod config;
pub mod fileops;
pub mod frecency;
pub mod import;
pub mod launchers;
pub mod listing;
pub mod mounts;
pub mod state;
pub mod tree;
pub mod watch;
