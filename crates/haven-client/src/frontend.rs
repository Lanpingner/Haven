//! The frontend seam, plus the two test frontends.
//!
//! `docs/PLAN.md` M0.5: these exist *before* either real frontend, so the whole
//! client loop is drivable with no window and no terminal. Three days here
//! instead of a two-week refactor at M6.

use std::collections::VecDeque;

use haven_proto::{CellSize, ClientCaps, FrontendKind, KeyTier};

use crate::frame::FrameModel;
use crate::input::InputEvent;

/// What a zoom key asks of a frontend.
///
/// Not the pane zoom the prefix key does — that rearranges a layout. This
/// is the size of the text, which only a frontend that owns its own font
/// can change at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zoom {
    In,
    Out,
    /// Back to the size the config asked for.
    Reset,
}

pub trait Frontend: std::fmt::Debug {
    fn caps(&self) -> ClientCaps;
    /// Non-blocking: `None` means no input is pending.
    fn next_input(&mut self) -> Option<InputEvent>;
    fn present(&mut self, frame: &FrameModel) -> Result<(), PresentError>;
    fn size(&self) -> CellSize {
        self.caps().size
    }
    /// Put text on whatever the host calls a clipboard. A frontend with no
    /// clipboard does nothing: the text is still kept in a paste buffer, so
    /// copy mode works either way.
    fn set_clipboard(&mut self, _text: &str) {}
    /// Make the text bigger or smaller, returning the new size in pixels.
    ///
    /// `None` from a frontend whose font is not its own: a client drawing
    /// into somebody else's terminal cannot resize that terminal's font,
    /// and the caller says so rather than letting the key do nothing.
    fn zoom(&mut self, _zoom: Zoom) -> Option<f32> {
        None
    }

    /// Stop or resume asking the host for mouse reports.
    ///
    /// Ignoring the events is not enough for a terminal client: the host
    /// stays in reporting mode, and its own selection goes on being broken
    /// by a feature the config just turned off.
    fn set_mouse(&mut self, _on: bool) {}
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

    /// Add an event after construction, for a test that has to let the
    /// client reach a particular state before the keystroke lands.
    pub fn feed(&mut self, event: InputEvent) {
        self.script.push_back(event);
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
