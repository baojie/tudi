//! The in-memory path index: data layout, snapshot persistence, and applying
//! change events. Used by `tudi-indexd`; testable without it.
//!
//! This crate must not depend on GTK.
