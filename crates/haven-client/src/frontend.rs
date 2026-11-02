//! The frontend seam, plus the two test frontends.
//!
//! `docs/PLAN.md` M0.5: these exist *before* either real frontend, so the whole
//! client loop is drivable with no window and no terminal. Three days here
//! instead of a two-week refactor at M6.

use std::collections::VecDeque;

use haven_proto::{CellSize, ClientCaps, FrontendKind, KeyTier};

use crate::frame::FrameModel;
use crate::input::InputEvent;

pub trait Frontend: std::fmt::Debug {
    fn caps(&self) -> ClientCaps;
    /// Non-blocking: `None` means no input is pending.
    fn next_input(&mut self) -> Option<InputEvent>;
    fn present(&mut self, frame: &FrameModel) -> Result<(), PresentError>;
    fn size(&self) -> CellSize {
        self.caps().size
    }
}

#[derive(Debug)]
pub struct PresentError(pub String);

impl std::fmt::Display for PresentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "present failed: {}", self.0)
    }
}

impl std::error::Error for PresentError {}

/// Records every frame it is asked to present and never produces input.
#[derive(Debug)]
pub struct NullFrontend {
    caps: ClientCaps,
    pub presented: Vec<FrameModel>,
}

impl NullFrontend {
    pub fn new(size: CellSize) -> Self {
        Self { caps: ClientCaps::null(size), presented: Vec::new() }
    }

    pub fn last(&self) -> Option<&FrameModel> {
        self.presented.last()
    }
}

impl Frontend for NullFrontend {
    fn caps(&self) -> ClientCaps {
        self.caps
    }

    fn next_input(&mut self) -> Option<InputEvent> {
        None
    }

    fn present(&mut self, frame: &FrameModel) -> Result<(), PresentError> {
        self.presented.push(frame.clone());
        Ok(())
    }
}

/// Replays a canned input script, and records frames like [`NullFrontend`].
#[derive(Debug)]
pub struct ScriptedFrontend {
    caps: ClientCaps,
    script: VecDeque<InputEvent>,
    pub presented: Vec<FrameModel>,
}

impl ScriptedFrontend {
    pub fn new(size: CellSize, script: impl IntoIterator<Item = InputEvent>) -> Self {
        Self {
            caps: ClientCaps::null(size),
            script: script.into_iter().collect(),
            presented: Vec::new(),
        }
    }

    /// Pretend to be a TUI client at a given fidelity, to exercise the
    /// degradation paths without a terminal.
    pub fn as_tui(mut self, tier: KeyTier) -> Self {
        self.caps.frontend = FrontendKind::Tui;
        self.caps.key_tier = tier;
        self
    }

    pub fn last(&self) -> Option<&FrameModel> {
        self.presented.last()
    }
}

impl Frontend for ScriptedFrontend {
    fn caps(&self) -> ClientCaps {
        self.caps
    }

    fn next_input(&mut self) -> Option<InputEvent> {
        self.script.pop_front()
    }

    fn present(&mut self, frame: &FrameModel) -> Result<(), PresentError> {
        self.presented.push(frame.clone());
        Ok(())
    }
}
