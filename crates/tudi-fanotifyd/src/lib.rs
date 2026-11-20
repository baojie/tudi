//! `tudi-fanotifyd`: optional root helper that forwards filesystem change
//! events to user daemons (PLAN §5.6, milestone M7). Subscribe-only by
//! design; the security model is in `docs/design/fanotify.md`.
//!
//! The library holds the protocol (also used by `tudi-indexd`) and the
//! pieces of the helper that can be unit-tested without root.
//! Dependencies: `libc` only; no async runtime.

pub mod access;
pub mod event;
pub mod mounts;
pub mod privs;
pub mod proto;
pub mod sys;
