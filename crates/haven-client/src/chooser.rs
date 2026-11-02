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

use haven_core::{Color, Style};
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
                    "{} ({} windows, {} panes{})",
                    info.name,
                    info.windows,
                    info.panes,
                    if info.clients > 0 { ", attached" } else { "" }
                ),
                choice: Choice::Session(info.id),
                current: here && status.is_none(),
            });
            if !here {
                continue;
            }
            let Some(status) = status else { continue };
            for (index, window) in status.windows.iter().enumerate() {
                let active_window = window.id == status.active_window;
                rows.push(Row {
                    indent: 1,
                    text: format!(
                        "{}: {}{}",
                        window.id,
                        window.name,
                        if window.zoomed { " (zoomed)" } else { "" }
                    ),
                    choice: Choice::Window(index as u16),
                    current: active_window && window.panes.len() <= 1,
                });
                for pane in &window.panes {
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
    pub fn widget(&self, size: CellSize) -> Option<Widget> {
        let visible = self.visible();
        if size.rows < 6 || size.cols < 24 {
            return None;
        }
        let title = if self.filter.is_empty() {
            " sessions  (enter to pick, esc to close, type to filter) ".to_string()
        } else {
            format!(" sessions  filter: {} ", self.filter)
        };

        let widest =
            visible.iter().map(|(_, r)| r.text.chars().count() + r.indent as usize * 2).max();
        let body_width = widest.unwrap_or(0).max(title.chars().count()) + 4;
        let cols = body_width.min(size.cols as usize - 2).max(24);

        // Leave the status line visible: it is where the mode is shown.
        let max_rows = (size.rows as usize).saturating_sub(4);
        let shown = visible.len().min(max_rows).max(1);
        let rows = shown + 2;

        let col = ((size.cols as usize - cols) / 2) as u16;
        let row = ((size.rows as usize - rows) / 2) as u16;

        // Scroll so the selection stays on screen.
        let at = visible.iter().position(|(i, _)| *i == self.selected).unwrap_or(0);
        let first = at.saturating_sub(shown.saturating_sub(1));

        let mut out = Vec::with_capacity(rows);
        out.push(line(&centre(&title, cols, '─'), frame_style()));
        for (index, (row_index, entry)) in visible.iter().skip(first).take(shown).enumerate() {
            let _ = index;
            let text = format!(
                "{}{}",
                "  ".repeat(entry.indent as usize),
                if entry.current { format!("* {}", entry.text) } else { entry.text.clone() }
            );
            let style = if *row_index == self.selected { selected_style() } else { entry_style() };
            out.push(line(&pad(&format!(" {text}"), cols), style));
        }
        if visible.is_empty() {
            out.push(line(&pad(" (nothing matches)", cols), entry_style()));
        }
        out.push(line(&centre("", cols, '─'), frame_style()));

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

fn line(text: &str, style: Style) -> FrameRow {
    FrameRow { spans: vec![Span { col: 0, text: text.to_string(), style }], rev: 0 }
}

fn pad(text: &str, cols: usize) -> String {
    let mut out: String = text.chars().take(cols).collect();
    let len = out.chars().count();
    if len < cols {
        out.push_str(&" ".repeat(cols - len));
    }
    out
}

fn centre(text: &str, cols: usize, fill: char) -> String {
    let len = text.chars().count();
    if len >= cols {
        return text.chars().take(cols).collect();
    }
    let left = (cols - len) / 2;
    let right = cols - len - left;
    format!("{}{}{}", fill.to_string().repeat(left), text, fill.to_string().repeat(right))
}

fn frame_style() -> Style {
    Style { fg: Color::Indexed(12), bg: Color::Indexed(0), ..Style::default() }
}

fn entry_style() -> Style {
    Style { fg: Color::Indexed(7), bg: Color::Indexed(0), ..Style::default() }
}

fn selected_style() -> Style {
    Style { fg: Color::Indexed(0), bg: Color::Indexed(12), ..Style::default() }
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
        for (cols, rows) in [(40u16, 10u16), (200, 60), (26, 8)] {
            let w = c.widget(size(cols, rows)).expect("widget");
            assert!(w.rect.col + w.rect.cols <= cols, "too wide at {cols}x{rows}");
            assert!(w.rect.row + w.rect.rows <= rows, "too tall at {cols}x{rows}");
            for row in &w.rows {
                assert_eq!(
                    row.spans[0].text.chars().count(),
                    w.rect.cols as usize,
                    "ragged row at {cols}x{rows}"
                );
            }
        }
    }

    #[test]
    fn a_screen_too_small_gets_no_overlay_rather_than_a_broken_one() {
        let c = Chooser::new(&[info(1, "work", 1)], Some(&status()));
        assert!(c.widget(size(20, 4)).is_none());
    }
}
