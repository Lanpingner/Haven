//! The session tree: sessions, their windows, their panes, pick one.
//!
//! This lives in the client rather than behind the key filter because it is
//! the only place that holds both halves of what it shows — the session list
//! from `ListSessions` and the attached session's windows and panes from
//! `SessionStatus`. A filter that owned the state would have to be handed
//! both, and would then be a second place that knows what a session is.
//!
//! Navigation is a flat list of rows with an indent level, not a real tree.
//! A tree would need expand and collapse state for no gain: the whole point
//! is to see everything at once and jump to one of it.

use haven_core::Style;

use crate::theme::Theme;
use haven_proto::{CellSize, PaneRect, SessionId, SessionInfo, SessionStatus};

use crate::frame::{FrameRow, Span, Widget, WidgetKind};

/// What picking a row does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    Session(SessionId),
    Window(u16),
    Pane(u32),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub indent: u8,
    pub text: String,
    pub choice: Choice,
    /// Where you already are, so the chooser opens on it.
    pub current: bool,
    /// Last among its siblings, so its branch closes instead of running on
    /// into whitespace below.
    pub last: bool,
    /// Whether the row's parent was itself a last child. Without this the
    /// trunk keeps being drawn beneath a tab that has nothing after it.
    pub parent_last: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Chooser {
    pub rows: Vec<Row>,
    pub selected: usize,
    /// Typed text narrows the list, as it does in every other chooser.
    pub filter: String,
}

impl Chooser {
    /// Build from what the client knows. `status` is the attached session, so
    /// only it can show windows and panes; the others are named and counted
    /// from the listing, which is all the daemon reports about them.
    pub fn new(sessions: &[SessionInfo], status: Option<&SessionStatus>) -> Self {
        let mut rows = Vec::new();
        let attached = status.map(|s| s.session);

        for info in sessions {
            let here = Some(info.id) == attached;
            rows.push(Row {
                indent: 0,
                text: format!(
                    "{} ({} tabs, {} panes{})",
                    info.name,
                    info.windows,
                    info.panes,
                    if info.clients > 0 { ", attached" } else { "" }
                ),
                choice: Choice::Session(info.id),
                current: here && status.is_none(),
                last: false,
                parent_last: false,
            });
            if !here {
                continue;
            }
            let Some(status) = status else { continue };
            for (index, window) in status.windows.iter().enumerate() {
                let active_window = window.id == status.active_window;
                rows.push(Row {
                    indent: 1,
                    // Called a tab in the interface, though the protocol and
                    // the commands keep tmux's name for it. "Window" is
                    // already taken here by the thing on the desktop — this
                    // program has one of those, which tmux never did — and
                    // one word for two levels of the same tree is a trap.
                    text: format!(
                        "tab {}: {}{}",
                        window.id,
                        window.name,
                        if window.zoomed { " (zoomed)" } else { "" }
                    ),
                    choice: Choice::Window(index as u16),
                    current: active_window && window.panes.len() <= 1,
                    last: index + 1 == status.windows.len(),
                    parent_last: false,
                });
                let pane_count = window.panes.len();
                let tab_is_last = index + 1 == status.windows.len();
                for (pane_index, pane) in window.panes.iter().enumerate() {
                    let active = active_window && pane.id == window.active_pane;
                    rows.push(Row {
                        indent: 2,
                        text: if pane.title.is_empty() {
                            format!("pane {}", pane.id)
                        } else {
                            format!("pane {} — {}", pane.id, pane.title)
                        },
                        choice: Choice::Pane(pane.id),
                        current: active,
                        last: pane_index + 1 == pane_count,
                        parent_last: tab_is_last,
                    });
                }
            }
        }

        // Several rows can be current at once — the active window and the
        // active pane inside it — so start on the deepest, which is the most
        // specific thing the user is actually looking at.
        let selected = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.current)
            .max_by_key(|(_, r)| r.indent)
            .map(|(i, _)| i)
            .unwrap_or(0);
        Self { rows, selected, filter: String::new() }
    }

    /// Rows matching the filter, with their index in `rows`.
    pub fn visible(&self) -> Vec<(usize, &Row)> {
        if self.filter.is_empty() {
            return self.rows.iter().enumerate().collect();
        }
        let needle = self.filter.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.text.to_lowercase().contains(&needle))
            .collect()
    }

    pub fn move_by(&mut self, delta: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let at = visible.iter().position(|(i, _)| *i == self.selected).unwrap_or(0);
        let next = (at as isize + delta).rem_euclid(visible.len() as isize) as usize;
        self.selected = visible[next].0;
    }

    /// Keep the selection on something the filter still shows.
    pub fn resettle(&mut self) {
        let visible = self.visible();
        if visible.iter().any(|(i, _)| *i == self.selected) {
            return;
        }
        self.selected = visible.first().map(|(i, _)| *i).unwrap_or(0);
    }

    pub fn choice(&self) -> Option<Choice> {
        self.rows.get(self.selected).map(|r| r.choice)
    }

    /// Draw it centred, as an overlay over the panes.
    pub fn widget(&self, size: CellSize, theme: &Theme) -> Option<Widget> {
        let visible = self.visible();
        if size.rows < 7 || size.cols < 28 {
            return None;
        }
        let g = &theme.glyphs;
        let title = if self.filter.is_empty() {
            " session / tab / pane ".to_string()
        } else {
            format!(" filter: {} ", self.filter)
        };

        let widest = visible
            .iter()
            .map(|(_, r)| r.text.chars().count() + guide_width(r.indent) + 6)
            .max()
            .unwrap_or(0);
        let cols = widest
            .max(title.chars().count() + 4)
            .max(HINT.chars().count() + 4)
            .min(size.cols as usize - 2)
            .max(28);
        let inner = cols - 2;

        // Two rows of border, one of hint, and the status bar keeps its own.
        let room = (size.rows as usize).saturating_sub(5);
        let shown = visible.len().min(room).max(1);
        let at = visible.iter().position(|(i, _)| *i == self.selected).unwrap_or(0);
        let first = at.saturating_sub(shown.saturating_sub(1));

        let mut out = Vec::with_capacity(shown + 3);
        out.push(crate::status_bar::border_row(g, cols, &title, theme, true));

        if visible.is_empty() {
            out.push(panel_line(g, inner, "  nothing matches", theme.panel_dim.style(), theme));
        }
        for (row_index, entry) in visible.iter().skip(first).take(shown) {
            let selected = *row_index == self.selected;
            let text = format!(
                "{}{}{}",
                guide(g, entry.indent, entry.last, entry.parent_last),
                entry.text,
                if entry.current { format!("  {}", g.here) } else { String::new() }
            );
            let style = if selected {
                theme.selection.style()
            } else if entry.indent == 0 {
                theme.panel_title.style()
            } else if entry.current {
                theme.panel.style()
            } else {
                theme.panel_dim.style()
            };
            out.push(panel_line(g, inner, &format!(" {text}"), style, theme));
        }

        // A way out, stated: a modal nobody can leave is a trap.
        let more = visible.len().saturating_sub(shown);
        let hint = if more > 0 { format!("{HINT} · +{more} more") } else { HINT.to_string() };
        out.push(panel_line(g, inner, &hint, theme.panel_dim.style(), theme));
        out.push(crate::status_bar::border_row(g, cols, "", theme, false));

        let col = ((size.cols as usize - cols) / 2) as u16;
        let row = ((size.rows as usize).saturating_sub(out.len()) / 2) as u16;
        Some(Widget {
            kind: WidgetKind::Overlay,
            rect: PaneRect {
                pane: haven_proto::PaneId(u32::MAX - 1),
                col,
                row,
                cols: cols as u16,
                rows: out.len() as u16,
            },
            rows: out,
        })
    }
}

const HINT: &str = " enter · esc · type to filter";

/// How wide a guide is at a given depth, so widths can be worked out before
/// the guide itself exists.
fn guide_width(indent: u8) -> usize {
    indent as usize * 2
}

/// The tree guide for a row. Drawing the shape beats indenting with spaces:
/// the eye follows a line, and three levels of plain indentation read as a
/// list that happens to be ragged.
fn guide(g: &crate::theme::Glyphs, indent: u8, last: bool, parent_last: bool) -> String {
    let arm = if last { &g.last_branch } else { &g.branch };
    match indent {
        0 => String::new(),
        1 => arm.clone(),
        // A trunk under a parent that has no siblings left would be a line
        // drawn to nothing.
        _ if parent_last => format!("{}{}", " ".repeat(g.trunk.chars().count()), arm),
        _ => format!("{}{}", g.trunk, arm),
    }
}

/// One row of a panel: border, content padded to the inner width, border.
fn panel_line(
    g: &crate::theme::Glyphs,
    inner: usize,
    text: &str,
    style: Style,
    theme: &Theme,
) -> FrameRow {
    FrameRow {
        spans: vec![
            Span { col: 0, text: g.vertical.clone(), style: theme.panel_border.style() },
            Span { col: 1, text: crate::status_bar::pad(text, inner), style },
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
    use haven_proto::{PaneInfo, WindowInfo};

    fn size(cols: u16, rows: u16) -> CellSize {
        CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
    }

    fn info(id: u32, name: &str, clients: u16) -> SessionInfo {
        SessionInfo {
            id: SessionId(id),
            name: name.into(),
            windows: 1,
            panes: 2,
            clients,
            size: size(80, 24),
        }
    }

    fn status() -> SessionStatus {
        SessionStatus {
            session: SessionId(1),
            name: "work".into(),
            windows: vec![WindowInfo {
                id: 1,
                name: "shell".into(),
                panes: vec![
                    PaneInfo { id: 10, title: "fish".into() },
                    PaneInfo { id: 11, title: "vim".into() },
                ],
                active_pane: 11,
                zoomed: false,
            }],
            active_window: 1,
            started_unix: 0,
            clients: 1,
        }
    }

    #[test]
    fn the_attached_session_shows_its_windows_and_panes() {
        let c = Chooser::new(&[info(1, "work", 1), info(2, "other", 0)], Some(&status()));
        let kinds: Vec<Choice> = c.rows.iter().map(|r| r.choice).collect();
        assert_eq!(
            kinds,
            vec![
                Choice::Session(SessionId(1)),
                Choice::Window(0),
                Choice::Pane(10),
                Choice::Pane(11),
                // The session we are not attached to cannot be expanded: the
                // daemon only reports counts for it.
                Choice::Session(SessionId(2)),
            ]
        );
    }

    #[test]
    fn it_opens_on_the_pane_you_are_in() {
        let c = Chooser::new(&[info(1, "work", 1)], Some(&status()));
        assert_eq!(c.choice(), Some(Choice::Pane(11)), "did not start on the active pane");
    }

    #[test]
    fn a_single_pane_window_still_opens_on_the_pane() {
        // The active window and its only pane are both "current"; the pane is
        // the more specific of the two and is where the cursor belongs.
        let mut st = status();
        st.windows[0].panes.truncate(1);
        st.windows[0].active_pane = 10;
        let c = Chooser::new(&[info(1, "work", 1)], Some(&st));
        assert_eq!(c.choice(), Some(Choice::Pane(10)));
    }

    #[test]
    fn a_single_pane_tab_still_opens_on_the_pane() {
        let mut st = status();
        st.windows[0].panes.truncate(1);
        st.windows[0].active_pane = 10;
        let c = Chooser::new(&[info(1, "work", 1)], Some(&st));
        assert_eq!(c.choice(), Some(Choice::Pane(10)));
    }

    #[test]
    fn no_trunk_is_drawn_under_the_last_tab() {
        let c = Chooser::new(&[info(1, "work", 1)], Some(&status()));
        let g = crate::theme::Glyphs::default();
        let pane = c.rows.iter().find(|r| r.indent == 2).expect("a pane row");
        assert!(pane.parent_last, "the only tab is the last one");
        let drawn = guide(&g, pane.indent, pane.last, pane.parent_last);
        assert!(!drawn.starts_with(&g.trunk), "a trunk was drawn to nothing: {drawn:?}");
        // The branch itself is still there.
        assert!(drawn.contains(&g.branch) || drawn.contains(&g.last_branch));
    }

    #[test]
    fn the_last_child_closes_its_branch() {
        let c = Chooser::new(&[info(1, "work", 1)], Some(&status()));
        let panes: Vec<&Row> = c.rows.iter().filter(|r| r.indent == 2).collect();
        assert!(!panes[0].last, "the first pane is not the last one");
        assert!(panes[1].last, "the last pane should close its branch");
    }

    #[test]
    fn moving_wraps_in_both_directions() {
        let mut c = Chooser::new(&[info(1, "work", 1)], Some(&status()));
        c.selected = 0;
        c.move_by(-1);
        assert_eq!(c.selected, c.rows.len() - 1, "up from the top should wrap");
        c.move_by(1);
        assert_eq!(c.selected, 0, "down from the bottom should wrap");
    }

    #[test]
    fn filtering_narrows_and_keeps_the_selection_valid() {
        let mut c = Chooser::new(&[info(1, "work", 1), info(2, "other", 0)], Some(&status()));
        c.filter = "vim".into();
        c.resettle();
        assert_eq!(c.visible().len(), 1);
        assert_eq!(c.choice(), Some(Choice::Pane(11)));

        // A filter matching nothing must not leave a selection pointing off
        // the end of the list.
        c.filter = "zzzz".into();
        c.resettle();
        assert!(c.visible().is_empty());
        assert!(c.choice().is_some() || c.rows.is_empty());
    }

    #[test]
    fn the_overlay_fits_inside_the_screen() {
        let c = Chooser::new(&[info(1, "work", 1)], Some(&status()));
        for (cols, rows) in [(40u16, 10u16), (200, 60), (30, 8)] {
            let w = c.widget(size(cols, rows), &Theme::haven()).expect("widget");
            assert!(w.rect.col + w.rect.cols <= cols, "too wide at {cols}x{rows}");
            assert!(w.rect.row + w.rect.rows <= rows, "too tall at {cols}x{rows}");
            for row in &w.rows {
                let width: usize = row.spans.iter().map(|s| s.text.chars().count()).sum();
                assert_eq!(width, w.rect.cols as usize, "ragged row at {cols}x{rows}");
            }
        }
    }

    #[test]
    fn a_screen_too_small_gets_no_overlay_rather_than_a_broken_one() {
        let c = Chooser::new(&[info(1, "work", 1)], Some(&status()));
        assert!(c.widget(size(20, 4), &Theme::haven()).is_none());
    }
}
