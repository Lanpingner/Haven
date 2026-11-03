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
    /// Typed text narrowing the list. A menu short enough to read whole does
    /// not need it; a palette over every command does, and one mechanism for
    /// both beats two.
    pub filter: String,
    /// Whether typing filters or picks. A menu of four shells is faster with
    /// digits; a list of forty commands is only usable by typing.
    pub filterable: bool,
}

impl Menu {
    pub fn new(title: impl Into<String>, entries: Vec<(String, String)>) -> Self {
        Self { title: title.into(), entries, selected: 0, filter: String::new(), filterable: false }
    }

    /// A menu you narrow by typing rather than one you pick from by number.
    pub fn filterable(mut self) -> Self {
        self.filterable = true;
        self
    }

    /// The entries the filter admits, as indices into `entries`.
    ///
    /// Indices rather than clones, so the selection can stay an index into
    /// the one list and never has to be translated between two.
    ///
    /// Entries that *start* with what was typed come first. A plain
    /// substring match puts `display-panes` above `split-v` for "spl",
    /// which is correct and useless: what you typed is nearly always the
    /// beginning of what you meant.
    pub fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.entries.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        let matching: Vec<usize> = (0..self.entries.len())
            .filter(|i| self.entries[*i].0.to_lowercase().contains(&needle))
            .collect();
        let (leading, rest): (Vec<usize>, Vec<usize>) = matching
            .into_iter()
            .partition(|i| self.entries[*i].0.to_lowercase().starts_with(&needle));
        leading.into_iter().chain(rest).collect()
    }

    pub fn move_by(&mut self, delta: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let at = visible.iter().position(|i| *i == self.selected).unwrap_or(0) as isize;
        let n = visible.len() as isize;
        self.selected = visible[(((at + delta) % n + n) % n) as usize];
    }

    /// Put the selection back on something the filter still shows.
    pub fn resettle(&mut self) {
        let visible = self.visible();
        if !visible.contains(&self.selected) {
            self.selected = visible.first().copied().unwrap_or(0);
        }
    }

    pub fn command(&self) -> Option<&str> {
        let visible = self.visible();
        if !visible.contains(&self.selected) {
            return None;
        }
        self.entries.get(self.selected).map(|(_, cmd)| cmd.as_str())
    }

    /// A digit picks an entry directly, as the numbers beside them promise.
    /// Only on a menu that is not filtered, where a digit is text instead.
    pub fn pick_digit(&mut self, c: char) -> bool {
        if self.filterable {
            return false;
        }
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
        let visible = self.visible();
        // Two rows of border, and never more than the screen can hold.
        let room = (size.rows as usize).saturating_sub(4);
        let shown = visible.len().min(room.max(1)).max(1);
        let at = visible.iter().position(|i| *i == self.selected).unwrap_or(0);
        let first = at.saturating_sub(shown - 1);

        // The title carries what has been typed, so the filter is visible
        // without a second row for it.
        let title = if self.filter.is_empty() {
            format!(" {} ", self.title)
        } else {
            format!(" {}: {} ", self.title, self.filter)
        };
        let widest = visible
            .iter()
            .skip(first)
            .take(shown)
            .map(|i| self.entries[*i].0.chars().count())
            .max()
            // With nothing matching there are no labels to measure, and a box
            // sized from them truncates the very text that explains why.
            .unwrap_or(NOTHING.len());
        // Room for " 1 " in front of the widest label, and never narrower
        // than the title — which grows as the filter is typed.
        let inner = (widest + 4).max(title.chars().count() + 2);
        let cols = (inner + 2).min(size.cols as usize);
        let inner = cols.saturating_sub(2);

        let mut rows = Vec::with_capacity(shown + 2);
        rows.push(border_row(g, cols, &title, theme, true));
        if visible.is_empty() {
            rows.push(panel_row(g, NOTHING, inner, theme.panel_dim.style(), theme));
        }
        for (row, index) in visible.iter().enumerate().skip(first).take(shown) {
            let (label, _) = &self.entries[*index];
            let style =
                if *index == self.selected { theme.selection.style() } else { theme.panel.style() };
            let text =
                if self.filterable { format!(" {label}") } else { format!(" {} {label}", row + 1) };
            rows.push(panel_row(g, &text, inner, style, theme));
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

/// Shown in place of the list when the filter admits nothing. A box that
/// simply emptied would look like a hang.
const NOTHING: &str = " nothing matches";

/// One row of a panel: border, padded content, border.
fn panel_row(
    g: &crate::theme::Glyphs,
    text: &str,
    inner: usize,
    style: haven_core::Style,
    theme: &Theme,
) -> FrameRow {
    FrameRow {
        spans: vec![
            Span { col: 0, text: g.vertical.clone(), style: theme.panel_border.style() },
            Span { col: 1, text: pad(text, inner), style },
            Span {
                col: (inner + 1) as u16,
                text: g.vertical.clone(),
                style: theme.panel_border.style(),
            },
        ],
        rev: 0,
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
        // An odd width cannot be centred exactly on an even screen, so the
        // margins are allowed to differ by the one cell that implies.
        let right = 60 - (w.rect.col + w.rect.cols);
        assert!(w.rect.col.abs_diff(right) <= 1, "not centred: {} vs {right}", w.rect.col);
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
    fn typing_narrows_a_filterable_menu() {
        let mut m = Menu::new(
            "command",
            vec![
                ("split-v".into(), "split-v".into()),
                ("split-h".into(), "split-h".into()),
                ("zoom".into(), "zoom".into()),
            ],
        )
        .filterable();
        m.filter.push_str("spl");
        assert_eq!(m.visible(), vec![0, 1]);
        m.filter.push('i');
        assert_eq!(m.visible(), vec![0, 1], "matching is on the label, anywhere in it");
        // A substring match still counts, but behind the ones that lead with
        // what was typed.
        m.entries.push(("display-panes".into(), "display-panes".into()));
        m.filter = "spl".into();
        assert_eq!(m.visible(), vec![0, 1, 3], "display-panes should rank last");
        m.filter = "zo".into();
        m.resettle();
        assert_eq!(m.command(), Some("zoom"), "the selection followed the filter");
    }

    #[test]
    fn a_selection_the_filter_hid_is_not_what_enter_runs() {
        let mut m =
            Menu::new("command", vec![("alpha".into(), "a".into()), ("beta".into(), "b".into())])
                .filterable();
        m.selected = 1;
        m.filter = "alph".into();
        // Before resettling, the selection points at something off the list.
        assert_eq!(m.command(), None, "Enter would have run an entry nobody can see");
        m.resettle();
        assert_eq!(m.command(), Some("a"));
    }

    #[test]
    fn moving_wraps_within_what_the_filter_shows() {
        let mut m = Menu::new(
            "command",
            vec![
                ("one".into(), "1".into()),
                ("skip".into(), "x".into()),
                ("two".into(), "2".into()),
            ],
        )
        .filterable();
        m.filter = "o".into();
        m.resettle();
        assert_eq!(m.visible(), vec![0, 2], "one and two, not skip");
        m.move_by(1);
        assert_eq!(m.command(), Some("2"));
        m.move_by(1);
        assert_eq!(m.command(), Some("1"), "it should wrap past the hidden entry");
    }

    #[test]
    fn digits_are_text_in_a_filterable_menu_and_a_shortcut_otherwise() {
        let mut picks = menu();
        assert!(picks.pick_digit('2'));
        let mut types = menu().filterable();
        assert!(!types.pick_digit('2'), "a digit must be typeable into a filter");
    }

    #[test]
    fn a_filter_that_matches_nothing_says_so_rather_than_drawing_an_empty_box() {
        let mut m = menu().filterable();
        m.filter = "nothing like this".into();
        let w = m.widget(size(60, 20), &Theme::haven()).expect("still a panel");
        let text: String =
            w.rows.iter().flat_map(|r| r.spans.iter()).map(|s| s.text.clone()).collect();
        assert!(text.contains("nothing matches"), "{text:?}");
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
