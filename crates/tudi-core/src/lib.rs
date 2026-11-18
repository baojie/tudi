//! Core of tudi: directory listing, the flattened tree, the query matcher,
//! transliteration, frecency, and configuration.
//!
//! This crate must not depend on GTK.

pub mod bookmarks;
pub mod config;
pub mod frecency;
pub mod listing;
pub mod state;
pub mod tree;
pub mod watch;
