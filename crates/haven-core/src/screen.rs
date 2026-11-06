//! A composited screen: resolved cells, ready for a frontend to present.
//!
//! This is the hand-off point between "what the mux decided" and "how it gets
//! drawn". It lives in `haven-core` because it is presentation-*neutral* data
//! — resolved text and styles, no geometry beyond cells — which is what lets
//! the serializer, the TUI frontend and the GPU frontend all consume one type
//! instead of each inventing its own (`docs/PLAN.md` §3).

use crate::style::Style;

/// One cell with its text and style already resolved: no interning tables
/// needed to read it, so diffing compares content rather than ids.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScreenCell {
    pub text: String,
    pub style: Style,
    /// Occupies two columns.
    pub wide: bool,
    /// The placeholder column owned by the wide cell to its left.
    pub spacer: bool,
    /// What this cell points at, from `OSC 8`, already resolved.
    ///
    /// Resolved like the text and the style, for the same reason: a
    /// frontend should not need a pane's interning tables to know that the
    /// thing under the pointer is a URL. Shared rather than copied, since
    /// a run of cells is one link and the screen is rebuilt every frame.
    pub link: Option<std::sync::Arc<str>>,
}

impl Default for ScreenCell {
    fn default() -> Self {
        Self::blank()
    }
}

impl ScreenCell {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self { text: text.into(), style, wide: false, spacer: false, link: None }
    }

    /// The same cell, pointing somewhere.
    pub fn linked(mut self, link: Option<std::sync::Arc<str>>) -> Self {
        self.link = link;
        self
    }

    pub fn blank() -> Self {
        Self { text: " ".into(), style: Style::default(), wide: false, spacer: false, link: None }
    }

    pub fn is_blank(&self) -> bool {
        self.text == " " && self.style == Style::default() && self.link.is_none()
    }

    /// Columns this cell occupies.
    pub fn width(&self) -> usize {
        if self.wide {
            2
        } else {
            1
        }
    }
}

/// Rows of cells, plus where the cursor sits.
pub type Screen = Vec<Vec<ScreenCell>>;

/// Fill a screen of the given size with blanks.
pub fn blank_screen(cols: usize, rows: usize) -> Screen {
    vec![vec![ScreenCell::blank(); cols]; rows]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_cell_is_recognised_as_blank() {
        assert!(ScreenCell::blank().is_blank());
        assert!(!ScreenCell::new("x", Style::default()).is_blank());
    }

    #[test]
    fn wide_cells_claim_two_columns() {
        let mut cell = ScreenCell::new("漢", Style::default());
        assert_eq!(cell.width(), 1);
        cell.wide = true;
        assert_eq!(cell.width(), 2);
    }
}
