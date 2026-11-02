//! Sessions, windows, pane trees and the layout solver. See `docs/PLAN.md` §10.
//!
//! This crate knows nothing about either frontend — it cannot link `wgpu` or
//! `haven-tui`, and CI enforces that (§4.2). It owns the model; presentation
//! is somebody else's problem.

pub mod layout;
pub mod runtime;
pub mod session;
pub mod window;

pub use layout::{neighbour, Child, Direction, Edge, PaneNode, SEPARATOR};
pub use runtime::{MuxRuntime, PaneRuntime, RuntimeEvent};
pub use session::{Session, SessionEvent};
pub use window::{NamedLayout, Window};
