//! Haven's composition root: it wires `haven-pty`, `haven-shell` and
//! `haven-core` together. The GPU and TUI frontends (`docs/PLAN.md` §12-13)
//! plug in behind the `Frontend` trait once M0.5 lands; until then this crate
//! exposes the headless pump, which is what the app-compatibility harness
//! (§16, M2.7) drives.

pub mod attach;
pub mod keys;
pub mod pump;
pub mod shot;
pub mod solo;

// Re-exported so the integration tests — and later the frontends — have one
// crate to depend on.
pub use haven_core;
pub use haven_pty;
pub use haven_shell;
pub use haven_vtwrite;
