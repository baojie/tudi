//! Core of tudi: directory listing, the flattened tree, the query matcher,
//! transliteration, frecency, and configuration.
//!
//! This crate must not depend on GTK.

pub mod listing;
pub mod config;
pub mod tree;
