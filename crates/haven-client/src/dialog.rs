//! The one modal box.
//!
//! The menu and the session tree drew their own, and the two disagreed
//! about everything that was not the border: where the filter went, whether
//! there was a hint, and — worst — how big to be. Both sized themselves to
//! the rows the filter happened to admit, so the window grew and shrank on
//! every keystroke and the thing you were reading moved while you read it.
//!
//! So: one renderer, three rules.
//!
//! **The box holds still.** Its size comes from what the dialog *has*, not
//! from what the filter admits. The list inside shrinks; the window does
//! not. Every fuzzy finder works this way and the reason is the same.
//!
//! **The title says where you are and how to search.** `history` becomes
//! `history · / to search`, and once you are searching it becomes
//! `history / needle`, so the one line answers both questions.
//!
//! **A panel is optional and on the right.** Help beside a command, a
//! preview beside a pane — different content, same furniture.

use haven_core::Style;
use haven_proto::{CellSize, PaneRect};

use crate::frame::{FrameRow, Span, Widget, WidgetKind};
use crate::status_bar::{border_row, pad};
use crate::theme::Theme;

/// A run of text in one style. A whole panel is rows of these, which is
/// what lets a preview keep a pane's colours and a help panel not bother.
pub type Run = (String, Style);

/// One line of the list.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DialogRow {
    /// Drawn before the text in its own colours: a tree's guides, a status
    /// mark. Taken over by the highlight when the row is selected, because
    /// a half-highlighted row reads as two things.
    pub lead: Vec<Run>,
    pub text: String,
    /// Overrides the ordinary row colour when not selected.
    pub style: Option<Style>,
}

impl DialogRow {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }

    pub fn with_lead(mut self, lead: Vec<Run>) -> Self {
        self.lead = lead;
        self
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = Some(style);
        self
    }

    fn width(&self) -> usize {
        self.lead.iter().map(|(t, _)| t.chars().count()).sum::<usize>() + self.text.chars().count()
    }
}

/// What to draw, before anything has been decided about how big it is.
#[derive(Debug)]
pub struct Dialog<'a> {
    pub title: &'a str,
    /// What has been typed to narrow the list, if anything.
    pub filter: &'a str,
    /// Every row the dialog holds, which is what it is sized from.
    pub total: usize,
    /// The rows the filter admits, in order.
    pub rows: Vec<DialogRow>,
    /// Which of `rows` is selected.
    pub selected: usize,
    /// The panel beside the list, already wrapped to whatever width it was
    /// given — see [`panel_width`].
    pub panel: Vec<Vec<Run>>,
    /// Whether typing narrows the list. A short menu is picked from by
    /// number instead, and saying "type to search" there would be a lie.
    ///
    /// It also decides how the box is sized — see [`Dialog::widget`].
    pub searchable: bool,
    /// Whether a panel belongs beside the list, even when `panel` is empty
    /// right now.
    ///
    /// A help lookup is not instant and a preview arrives a round trip
    /// late, so a dialog that sizes itself around the panel it *has* opens
    /// narrow and widens a moment later, under the cursor. This reserves
    /// the room from the first frame.
    pub panel_slot: bool,
    /// Shown under the list. A modal nobody can leave is a trap.
    pub hint: &'a str,
    /// Distinguishes this overlay from the others in the frame.
    pub id: u32,
}

/// How wide a panel beside a list would be on this screen, or zero when the
/// screen is too narrow to be worth splitting.
///
/// Public because a caller has to wrap its content to this width *before*
/// building the dialog: the number of lines decides the height.
pub fn panel_width(size: CellSize) -> usize {
    if size.cols < 72 {
        return 0;
    }
    // Of the frame, not of the screen: half the screen beside a list that
    // only got what the frame had left over left the list a quarter as wide
    // as the help next to it, and the lines you were there to read were the
    // ones being cut. Slightly under half, so the list — the thing being
    // chosen from — is the wider column.
    let inner = (size.cols as usize * 4 / 5).saturating_sub(2);
    (inner * 45 / 100).clamp(24, 56)
}

/// How many rows a searchable dialog's list and panel are tall.
///
/// Public because a preview has to be *asked for* at this height: a caller
/// that guesses high gets a snapshot whose bottom rows are then dropped,
/// and the bottom is the half worth seeing.
pub fn panel_rows(size: CellSize) -> usize {
    let room = (size.rows as usize).saturating_sub(5);
    (size.rows as usize * 2 / 3).clamp(1, room.max(1))
}

const NOTHING: &str = "  nothing matches";

impl Dialog<'_> {
    pub fn widget(&self, size: CellSize, theme: &Theme) -> Option<Widget> {
        // Small enough to be useless is four rows — border, a row, the
        // hint, border — and a width that cannot hold a word. Anything
        // above that gets a box, because a modal that silently declines to
        // appear on a small screen looks like a key that does nothing.
        if size.rows < 5 || size.cols < 12 {
            return None;
        }
        let g = &theme.glyphs;
        let panel_w = if self.panel_slot || !self.panel.is_empty() { panel_width(size) } else { 0 };

        // Border, hint, border, and the status line keeps its own row.
        let room = (size.rows as usize).saturating_sub(5);

        // Where you are, and how to narrow it — and once you are
        // narrowing, what by. One line answers both questions.
        let title = match (self.filter.is_empty(), self.searchable) {
            (false, _) => format!(" {} / {} ", self.title, self.filter),
            (true, true) => format!(" {} · type to search ", self.title),
            (true, false) => format!(" {} ", self.title),
        };
        // Two ways to be a box, and which one depends on whether you type
        // into it.
        //
        // A **searchable** dialog takes a fixed frame off the screen, the
        // way telescope does: two thirds of the rows, four fifths of the
        // columns. Nothing it holds can move it — not the rows the filter
        // admits, not how long the selected command's man page turned out
        // to be. Sizing it from its contents was the bug, and sizing it
        // from its *unfiltered* contents only moved the bug, because the
        // help panel beside the list still changed length on every arrow
        // press.
        //
        // A **menu** — four shells, picked by digit — is sized to fit,
        // because a short list in a window five times its size reads as a
        // mistake, and nothing about it changes while it is open.
        let (body, list_w) = if self.searchable {
            let body = panel_rows(size);
            let cols = (size.cols as usize * 4 / 5).clamp(12, size.cols as usize);
            let inner = cols.saturating_sub(2).max(1);
            let list_w =
                if panel_w > 0 && inner > panel_w + 4 { inner - panel_w - 3 } else { inner };
            (body, list_w)
        } else {
            let widest = self
                .rows
                .iter()
                .map(DialogRow::width)
                .chain(std::iter::once(NOTHING.len()))
                .max()
                .unwrap_or(0)
                // One line nobody can read should not decide the shape of
                // the window.
                .min(size.cols as usize * 3 / 4);
            let list_w = (widest + 2)
                // The box is sized from the title, which on a menu never
                // changes: there is nothing to type into one.
                .max(self.title.chars().count() + 2)
                .max(self.hint.chars().count())
                .max(24.min(size.cols as usize - 2))
                .min((size.cols as usize).saturating_sub(panel_w + 5).max(1));
            (self.total.max(self.panel.len()).max(1).min(room.max(1)), list_w)
        };
        let inner = if panel_w > 0 { list_w + panel_w + 3 } else { list_w };
        let cols = (inner + 2).min(size.cols as usize);
        let inner = cols - 2;
        let list_w = if panel_w > 0 { inner.saturating_sub(panel_w + 3) } else { inner };

        // Keep the selection in view, and show as much above it as below.
        let at = self.selected.min(self.rows.len().saturating_sub(1));
        let first = at.saturating_sub(body.saturating_sub(1).min(at));

        // A query longer than the box is cut rather than allowed to widen
        // it: the window holding still matters more than seeing every
        // letter of what you typed, which is on the screen anyway.
        let title = if title.chars().count() > inner {
            let keep = inner.saturating_sub(2);
            format!("{}… ", title.chars().take(keep).collect::<String>())
        } else {
            title
        };

        let mut out = Vec::with_capacity(body + 3);
        out.push(border_row(g, cols, &title, theme, true));
        for n in 0..body {
            let row = self.rows.get(first + n);
            let panel = self.panel.get(n);
            let selected = row.is_some() && first + n == self.selected;
            let style = match (selected, row.and_then(|r| r.style)) {
                (true, _) => theme.selection.style(),
                (false, Some(style)) => style,
                (false, None) => theme.panel.style(),
            };
            let mut spans: Vec<Span> =
                vec![Span { col: 0, text: g.vertical.clone(), style: theme.panel_border.style() }];
            let mut col = 1usize;
            let mut used = 0usize;
            // A row of nothing but a hint, when the filter admits nothing.
            if self.rows.is_empty() && n == 0 {
                spans.push(Span {
                    col: col as u16,
                    text: pad(NOTHING, list_w),
                    style: theme.panel_dim.style(),
                });
                used = list_w;
                col += list_w;
            } else if let Some(row) = row {
                for (text, lead_style) in &row.lead {
                    let width = text.chars().count().min(list_w.saturating_sub(used));
                    if width == 0 {
                        break;
                    }
                    let clipped: String = text.chars().take(width).collect();
                    spans.push(Span {
                        col: col as u16,
                        text: clipped,
                        // The highlight takes the whole row: a selected line
                        // in two colours reads as two lines.
                        style: if selected { style } else { *lead_style },
                    });
                    col += width;
                    used += width;
                }
                let rest = list_w.saturating_sub(used);
                let text: String = row.text.chars().take(rest).collect();
                spans.push(Span { col: col as u16, text: pad(&text, rest), style });
                col += rest;
                used = list_w;
            } else {
                // Past the end of the list. Drawn blank rather than skipped,
                // so the box keeps its shape as the filter narrows.
                spans.push(Span {
                    col: col as u16,
                    text: pad("", list_w),
                    style: theme.panel.style(),
                });
                used = list_w;
                col += list_w;
            }
            let _ = used;

            if panel_w > 0 {
                spans.push(Span {
                    col: col as u16,
                    text: format!(" {} ", g.vertical),
                    style: theme.panel_border.style(),
                });
                col += 3;
                let mut taken = 0usize;
                for (text, run_style) in panel.into_iter().flatten() {
                    let width = text.chars().count().min(panel_w.saturating_sub(taken));
                    if width == 0 {
                        break;
                    }
                    let clipped: String = text.chars().take(width).collect();
                    spans.push(Span { col: col as u16, text: clipped, style: *run_style });
                    col += width;
                    taken += width;
                }
                if taken < panel_w {
                    spans.push(Span {
                        col: col as u16,
                        text: pad("", panel_w - taken),
                        style: theme.panel.style(),
                    });
                    col += panel_w - taken;
                }
            }
            spans.push(Span {
                col: col as u16,
                text: g.vertical.clone(),
                style: theme.panel_border.style(),
            });
            out.push(FrameRow { spans, rev: 0 });
        }

        let hidden = self.rows.len().saturating_sub(body);
        let hint = if hidden > 0 {
            format!("{} · +{hidden} more", self.hint)
        } else {
            self.hint.to_string()
        };
        out.push(FrameRow {
            spans: vec![
                Span { col: 0, text: g.vertical.clone(), style: theme.panel_border.style() },
                Span { col: 1, text: pad(&hint, inner), style: theme.panel_dim.style() },
                Span {
                    col: (inner + 1) as u16,
                    text: g.vertical.clone(),
                    style: theme.panel_border.style(),
                },
            ],
            rev: 0,
        });
        out.push(border_row(g, cols, "", theme, false));

        let height = out.len() as u16;
        Some(Widget {
            kind: WidgetKind::Overlay,
            rect: PaneRect {
                pane: haven_proto::PaneId(u32::MAX - self.id),
                col: (size.cols.saturating_sub(cols as u16)) / 2,
                row: (size.rows.saturating_sub(height)) / 2,
                cols: cols as u16,
                rows: height,
            },
            rows: out,
        })
    }
}

/// Wrap plain text into panel lines of one run each.
pub fn wrap_plain(lines: &[String], width: usize, style: Style) -> Vec<Vec<Run>> {
    let mut out: Vec<Vec<Run>> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            out.push(Vec::new());
            continue;
        }
        let mut current = String::new();
        for word in line.split_whitespace() {
            if current.is_empty() {
                current = word.to_string();
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                out.push(vec![(std::mem::take(&mut current), style)]);
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            out.push(vec![(current, style)]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<DialogRow> {
        (0..n).map(|i| DialogRow::new(format!("entry {i}"))).collect()
    }

    fn drawn(d: &Dialog, size: CellSize) -> Vec<String> {
        let theme = Theme::default();
        d.widget(size, &theme)
            .expect("a widget")
            .rows
            .iter()
            .map(|row| {
                let mut line = String::new();
                for span in &row.spans {
                    while line.chars().count() < span.col as usize {
                        line.push(' ');
                    }
                    line.push_str(&span.text);
                }
                line
            })
            .collect()
    }

    fn dialog<'a>(all: usize, shown: Vec<DialogRow>, filter: &'a str) -> Dialog<'a> {
        Dialog {
            title: "history",
            filter,
            total: all,
            rows: shown,
            selected: 0,
            panel: Vec::new(),
            searchable: true,
            panel_slot: false,
            hint: "enter · esc",
            id: 1,
        }
    }

    #[test]
    fn the_box_does_not_change_size_as_the_filter_narrows() {
        // What this module exists for: the window used to be sized from the
        // rows the filter admitted, so it shrank under the cursor on every
        // keystroke and the line you were reading moved.
        let size = CellSize { cols: 100, rows: 24, pixel_width: 0, pixel_height: 0 };
        let wide = drawn(&dialog(12, rows(12), ""), size);
        let narrow = drawn(&dialog(12, rows(2), "ent"), size);
        assert_eq!(wide.len(), narrow.len(), "the height moved");
        assert_eq!(
            wide[0].chars().count(),
            narrow[0].chars().count(),
            "the width moved: {:?} vs {:?}",
            wide[0],
            narrow[0]
        );
    }

    #[test]
    fn a_searchable_box_is_the_same_box_whatever_is_in_it() {
        // The jumping that survived the first fix: the height followed the
        // help panel, which is as long as the selected command's man page,
        // and the width followed the widest row the filter admitted. Both
        // changed on every keystroke in alt-H.
        let size = CellSize { cols: 120, rows: 40, pixel_width: 0, pixel_height: 0 };
        let shape = |d: &Dialog| {
            let lines = drawn(d, size);
            (lines.len(), lines[0].chars().count())
        };

        let mut short = dialog(60, rows(60), "");
        short.panel_slot = true;
        short.panel = wrap_plain(&["one line of help".to_string()], 40, Style::default());

        let mut long = dialog(60, rows(3), "ent");
        long.panel_slot = true;
        long.panel = wrap_plain(
            &(0..200).map(|i| format!("help line {i}")).collect::<Vec<_>>(),
            40,
            Style::default(),
        );

        // And the one in between: the lookup has not come back at all.
        let mut pending = dialog(60, rows(60), "");
        pending.panel_slot = true;

        assert_eq!(shape(&short), shape(&long), "the box moved with the help");
        assert_eq!(shape(&short), shape(&pending), "the box moved while help loaded");
    }

    #[test]
    fn a_menu_is_sized_to_fit_instead() {
        // The other half of the bargain: a menu of three shells must not
        // open as a screen-sized frame.
        let size = CellSize { cols: 120, rows: 40, pixel_width: 0, pixel_height: 0 };
        let mut d = dialog(3, rows(3), "");
        d.searchable = false;
        let lines = drawn(&d, size);
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert!(lines[0].chars().count() < 60, "{:?}", lines[0]);
    }

    #[test]
    fn the_title_says_where_you_are_and_how_to_search() {
        let size = CellSize { cols: 100, rows: 24, pixel_width: 0, pixel_height: 0 };
        let idle = drawn(&dialog(4, rows(4), ""), size);
        assert!(idle[0].contains("history"), "{:?}", idle[0]);
        assert!(idle[0].contains("type to search"), "{:?}", idle[0]);

        let searching = drawn(&dialog(4, rows(1), "ent"), size);
        assert!(searching[0].contains("history / ent"), "{:?}", searching[0]);
        assert!(!searching[0].contains("to search"), "the hint outstayed its welcome");
    }

    #[test]
    fn nothing_matching_says_so_rather_than_emptying_the_box() {
        let size = CellSize { cols: 100, rows: 24, pixel_width: 0, pixel_height: 0 };
        let empty = drawn(&dialog(12, Vec::new(), "zzz"), size);
        assert!(empty.iter().any(|l| l.contains("nothing matches")), "{empty:?}");
        assert_eq!(
            empty.len(),
            drawn(&dialog(12, rows(12), ""), size).len(),
            "and it kept its size"
        );
    }

    #[test]
    fn a_panel_sits_beside_the_list_and_keeps_its_colours() {
        let size = CellSize { cols: 100, rows: 24, pixel_width: 0, pixel_height: 0 };
        let mut d = dialog(4, rows(4), "");
        let red = Style { fg: haven_core::Color::Indexed(1), ..Default::default() };
        d.panel = vec![vec![("coloured".to_string(), red)], vec![("second".to_string(), red)]];
        let theme = Theme::default();
        let widget = d.widget(size, &theme).expect("widget");
        let styles: Vec<Style> = widget.rows[1].spans.iter().map(|s| s.style).collect();
        assert!(styles.contains(&red), "the panel lost its colour: {styles:?}");
        let text: String = widget.rows[1].spans.iter().map(|s| s.text.clone()).collect();
        assert!(text.contains("entry 0") && text.contains("coloured"), "{text:?}");
    }

    #[test]
    fn a_narrow_screen_drops_the_panel_rather_than_the_list() {
        let narrow = CellSize { cols: 50, rows: 20, pixel_width: 0, pixel_height: 0 };
        assert_eq!(panel_width(narrow), 0);
        let mut d = dialog(4, rows(4), "");
        d.panel = vec![vec![("help".to_string(), Style::default())]];
        let lines = drawn(&d, narrow);
        assert!(lines.iter().any(|l| l.contains("entry 0")), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("help")), "{lines:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= 50), "it overflowed the screen");
    }
}
