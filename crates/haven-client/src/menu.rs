//! A modal list of things to pick, each of which runs a command.
//!
//! The chooser next door knows about sessions; this one knows about nothing.
//! It holds labels paired with command strings, which the composition root
//! parses exactly as it parses a typed line — so a menu cannot offer anything
//! the command line could not do, and a new menu is data rather than code.

use haven_proto::{CellSize, PaneRect};

use crate::frame::{FrameRow, Span, Widget, WidgetKind};
use crate::status_bar::{border_row, pad};
use crate::theme::Theme;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Menu {
    pub title: String,
    /// Label and the command it stands for.
    pub entries: Vec<(String, String)>,
    pub selected: usize,
}

impl Menu {
    pub fn new(title: impl Into<String>, entries: Vec<(String, String)>) -> Self {
        Self { title: title.into(), entries, selected: 0 }
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len() as isize;
        self.selected = (((self.selected as isize + delta) % n + n) % n) as usize;
    }

    pub fn command(&self) -> Option<&str> {
        self.entries.get(self.selected).map(|(_, cmd)| cmd.as_str())
    }

    /// A digit picks an entry directly, as the numbers beside them promise.
    pub fn pick_digit(&mut self, c: char) -> bool {
        let Some(n) = c.to_digit(10).filter(|n| *n > 0) else { return false };
        let index = n as usize - 1;
        if index >= self.entries.len() {
            return false;
        }
        self.selected = index;
        true
    }

    /// The panel, centred over the screen.
    pub fn widget(&self, size: CellSize, theme: &Theme) -> Option<Widget> {
        if self.entries.is_empty() || size.rows < 4 || size.cols < 10 {
            return None;
        }
        let g = &theme.glyphs;
        // Two rows of border, and never more than the screen can hold.
        let room = (size.rows as usize).saturating_sub(4);
        let shown = self.entries.len().min(room.max(1));
        let first = self.selected.saturating_sub(shown - 1);

        let widest = self
            .entries
            .iter()
            .skip(first)
            .take(shown)
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        // Room for " 1 " in front of the widest label.
        let inner = (widest + 4).max(self.title.chars().count() + 2);
        let cols = (inner + 2).min(size.cols as usize);
        let inner = cols.saturating_sub(2);

        let mut rows = Vec::with_capacity(shown + 2);
        rows.push(border_row(g, cols, &format!(" {} ", self.title), theme, true));
        for (i, (label, _)) in self.entries.iter().enumerate().skip(first).take(shown) {
            let style =
                if i == self.selected { theme.selection.style() } else { theme.panel.style() };
            let text = format!(" {} {label}", i + 1);
            rows.push(FrameRow {
                spans: vec![
                    Span { col: 0, text: g.vertical.clone(), style: theme.panel_border.style() },
                    Span { col: 1, text: pad(&text, inner), style },
                    Span {
                        col: (inner + 1) as u16,
                        text: g.vertical.clone(),
                        style: theme.panel_border.style(),
                    },
                ],
                rev: 0,
            });
        }
        rows.push(border_row(g, cols, "", theme, false));

        let height = rows.len() as u16;
        Some(Widget {
            kind: WidgetKind::Overlay,
            rect: PaneRect {
                pane: haven_proto::PaneId(u32::MAX - 4),
                col: (size.cols.saturating_sub(cols as u16)) / 2,
                row: (size.rows.saturating_sub(height)) / 2,
                cols: cols as u16,
                rows: height,
            },
            rows,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Menu {
        Menu::new(
            "shell",
            vec![
                ("fish".into(), "split-v /usr/bin/fish".into()),
                ("bash".into(), "split-v /bin/bash".into()),
                ("sh".into(), "split-v /bin/sh".into()),
            ],
        )
    }

    fn size(cols: u16, rows: u16) -> CellSize {
        CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
    }

    #[test]
    fn moving_wraps_in_both_directions() {
        let mut m = menu();
        m.move_by(-1);
        assert_eq!(m.selected, 2, "up from the top should wrap to the bottom");
        m.move_by(1);
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn a_digit_picks_the_entry_beside_it() {
        let mut m = menu();
        assert!(m.pick_digit('2'));
        assert_eq!(m.command(), Some("split-v /bin/bash"));
        // Zero and anything past the end are not entries.
        assert!(!m.pick_digit('0'));
        assert!(!m.pick_digit('9'));
        assert_eq!(m.selected, 1, "a refused digit must not move the selection");
    }

    #[test]
    fn the_panel_is_centred_and_holds_every_entry() {
        let w = menu().widget(size(60, 20), &Theme::haven()).expect("a panel");
        assert_eq!(w.rows.len(), 5, "three entries plus two borders");
        let right = w.rect.col + w.rect.cols;
        assert_eq!(w.rect.col, 60 - right, "not centred horizontally");
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_selection_visible() {
        let entries: Vec<(String, String)> =
            (0..20).map(|i| (format!("shell{i}"), format!("run {i}"))).collect();
        let mut m = Menu::new("shell", entries);
        m.selected = 19;
        let w = m.widget(size(40, 8), &Theme::haven()).expect("a panel");
        let text: String =
            w.rows.iter().flat_map(|r| r.spans.iter()).map(|s| s.text.clone()).collect();
        assert!(text.contains("shell19"), "the selection scrolled off: {text:?}");
    }

    #[test]
    fn an_empty_menu_draws_nothing() {
        assert!(Menu::new("shell", Vec::new()).widget(size(60, 20), &Theme::haven()).is_none());
    }

    #[test]
    fn a_screen_too_short_for_a_panel_gets_none_rather_than_a_broken_one() {
        assert!(menu().widget(size(60, 3), &Theme::haven()).is_none());
    }
}
