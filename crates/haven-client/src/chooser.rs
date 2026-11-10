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

use crate::theme::Theme;
use haven_proto::{CellSize, SessionId, SessionInfo, SessionStatus};

use crate::frame::Widget;

/// What picking a row does.
///
/// Every row that is somewhere says which session that somewhere is in. A
/// window index and a pane id are both only meaningful inside one session —
/// the ids restart per session, so two sessions commonly have a pane 1 each
/// — and the tree lists every session's tabs. Without the session, picking
/// a row under another session sent a window index or a pane id that the
/// daemon resolved against the session the client was already in: either
/// the wrong tab, or nothing at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    Session(SessionId),
    Window {
        session: SessionId,
        index: u16,
    },
    Pane {
        session: SessionId,
        id: u32,
    },
    /// Make one, and go there. The chooser is where you are already
    /// deciding which session you want, so it is where "none of these"
    /// belongs.
    NewSession,
}

impl Choice {
    /// Which session picking this lands you in, if it is a session at all.
    pub fn session(&self) -> Option<SessionId> {
        match *self {
            Choice::Session(id) => Some(id),
            Choice::Window { session, .. } | Choice::Pane { session, .. } => Some(session),
            Choice::NewSession => None,
        }
    }
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
    /// What the selected pane is showing, and which pane that was. Kept
    /// with its id so an answer that arrives after the selection moved on
    /// is dropped rather than shown against the wrong row.
    pub preview: Option<(u32, Vec<haven_proto::PreviewRow>)>,
}

impl Chooser {
    /// Which pane the selection is about, for the preview.
    ///
    /// A pane row means itself; a tab means its active pane; a session
    /// means the active pane of its active tab. Every row is *somewhere*,
    /// and that somewhere is what to show.
    pub fn previewed(&self, sessions: &[SessionInfo]) -> Option<u32> {
        let row = self.rows.get(self.selected)?;
        match row.choice {
            // Nothing exists to preview yet.
            Choice::NewSession => None,
            Choice::Pane { id, .. } => Some(id),
            Choice::Window { session, index } => {
                let info = sessions.iter().find(|s| s.id == session)?;
                info.tabs.get(index as usize).map(|w| w.active_pane)
            }
            Choice::Session(id) => {
                let info = sessions.iter().find(|s| s.id == id)?;
                info.tabs.first().map(|w| w.active_pane)
            }
        }
    }
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
                    "{} ({} {}, {} {}{})",
                    info.name,
                    info.windows,
                    if info.windows == 1 { "tab" } else { "tabs" },
                    info.panes,
                    if info.panes == 1 { "pane" } else { "panes" },
                    if info.clients > 0 { ", attached" } else { "" }
                ),
                choice: Choice::Session(info.id),
                current: here && status.is_none(),
                last: false,
                parent_last: false,
            });
            // Every session's tabs, not just the attached one's. Answering
            // "what is in that other session" is the only reason to open a
            // tree rather than press `last`.
            let active_window = status
                .filter(|s| s.session == info.id)
                .map(|s| s.active_window)
                .or_else(|| info.tabs.first().map(|w| w.id));
            for (index, window) in info.tabs.iter().enumerate() {
                let active_window = Some(window.id) == active_window && here;
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
                    choice: Choice::Window { session: info.id, index: index as u16 },
                    current: active_window && window.panes.len() <= 1,
                    last: index + 1 == info.tabs.len(),
                    parent_last: false,
                });
                let pane_count = window.panes.len();
                let tab_is_last = index + 1 == info.tabs.len();
                for (pane_index, pane) in window.panes.iter().enumerate() {
                    let active = active_window && pane.id == window.active_pane;
                    // Whose pane it is, when that is not whose Haven is.
                    // The tree is where you go looking for a pane you left
                    // somewhere, and "the one I sudo'd in" is exactly the
                    // kind of thing you are looking for.
                    let user =
                        pane.user.as_deref().map(|user| format!("  [{user}]")).unwrap_or_default();
                    rows.push(Row {
                        indent: 2,
                        // What it is doing, if the shell says so; what it
                        // calls itself otherwise. A bare id names nothing.
                        text: match (pane.command.as_str(), pane.title.as_str()) {
                            ("", "") => format!("pane {}{user}", pane.id),
                            ("", title) => format!("pane {}  {title}{user}", pane.id),
                            (command, _) => format!(
                                "pane {}  {command}{}{user}",
                                pane.id,
                                if pane.running { " …" } else { "" }
                            ),
                        },
                        choice: Choice::Pane { session: info.id, id: pane.id },
                        current: active,
                        last: pane_index + 1 == pane_count,
                        parent_last: tab_is_last,
                    });
                }
            }
        }

        // "None of these." Last, because it is the answer you want least
        // often, and never marked current, so opening the tree still lands
        // on where you are.
        //
        // Only once there is a listing to be none of, though: an empty
        // `rows` is how the client knows the listing has not arrived yet,
        // and it settles the selection onto the active pane on the first
        // rebuild that has rows. A row that is always present would make
        // that rebuild look like a user who had already chosen, and the
        // tree would open on this row instead of where you are.
        if !rows.is_empty() {
            rows.push(Row {
                indent: 0,
                text: "+ new session".into(),
                choice: Choice::NewSession,
                current: false,
                last: true,
                parent_last: false,
            });
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
        Self { rows, selected, filter: String::new(), preview: None }
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
    ///
    /// Through [`crate::dialog`], which the menus use too: one box, one
    /// rule about its size, one place the filter is shown.
    pub fn widget(&self, size: CellSize, theme: &Theme) -> Option<Widget> {
        let g = &theme.glyphs;
        let visible = self.visible();
        let rows: Vec<crate::dialog::DialogRow> = visible
            .iter()
            .map(|(_, entry)| {
                let guides = guide(g, entry.indent, entry.last, entry.parent_last);
                let text = format!(
                    "{}{}",
                    entry.text,
                    if entry.current { format!("  {}", g.here) } else { String::new() }
                );
                let style = if entry.indent == 0 {
                    theme.panel_title.style()
                } else if entry.current {
                    theme.panel.style()
                } else {
                    theme.panel_dim.style()
                };
                crate::dialog::DialogRow::new(text)
                    .with_lead(vec![(format!(" {guides}"), theme.panel_dim.style())])
                    .with_style(style)
            })
            .collect();
        let selected = visible.iter().position(|(i, _)| *i == self.selected).unwrap_or(0);

        crate::dialog::Dialog {
            title: "sessions",
            filter: &self.filter,
            total: self.rows.len(),
            rows,
            selected,
            panel: self.panel(size, theme),
            searchable: true,
            // The preview is a round trip away; the room for it is not.
            panel_slot: crate::dialog::panel_width(size) > 0,
            hint: HINT,
            id: 1,
        }
        .widget(size, theme)
    }

    /// The preview: what the selected pane is showing, in its own colours.
    fn panel(&self, size: CellSize, theme: &Theme) -> Vec<Vec<crate::dialog::Run>> {
        let width = crate::dialog::panel_width(size);
        if width == 0 {
            return Vec::new();
        }
        let Some((_, rows)) = &self.preview else {
            // Said rather than left blank: an empty panel looks like a
            // preview of an empty pane.
            return vec![vec![("  …".to_string(), theme.panel_dim.style())]];
        };
        rows.iter()
            .map(|row| {
                row.runs
                    .iter()
                    .flat_map(|(count, text, style)| {
                        std::iter::repeat_n(text.clone(), *count as usize).map(move |t| (t, *style))
                    })
                    .collect()
            })
            .collect()
    }
}

const HINT: &str = " enter · esc";

/// How wide a guide is at a given depth, so widths can be worked out before
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
            // The listing carries the tabs now: the tree shows them for
            // every session, not only the attached one.
            tabs: status().windows,
        }
    }

    /// A listing entry whose tabs are the ones in this status, which is
    /// what the daemon sends: the tree reads tabs from the listing so it
    /// can show them for sessions nobody is attached to.
    fn info_of(id: u32, name: &str, clients: u16, st: &SessionStatus) -> SessionInfo {
        SessionInfo { tabs: st.windows.clone(), ..info(id, name, clients) }
    }

    fn status() -> SessionStatus {
        SessionStatus {
            session: SessionId(1),
            name: "work".into(),
            windows: vec![WindowInfo {
                id: 1,
                name: "shell".into(),
                panes: vec![
                    PaneInfo {
                        id: 10,
                        rect: Default::default(),
                        title: "fish".into(),
                        command: "cargo test".into(),
                        running: true,
                        user: None,
                    },
                    PaneInfo {
                        id: 11,
                        rect: Default::default(),
                        title: "vim".into(),
                        command: String::new(),
                        running: false,
                        user: None,
                    },
                ],
                active_pane: 11,
                zoomed: false,
                synchronized: false,
                bell: false,
                activity: false,
            }],
            active_window: 1,
            started_unix: 0,
            clients: 1,
            plugins: Vec::new(),
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
                Choice::Window { session: SessionId(1), index: 0 },
                Choice::Pane { session: SessionId(1), id: 10 },
                Choice::Pane { session: SessionId(1), id: 11 },
                // And the session nobody is attached to, expanded too: the
                // listing carries its tabs, which is what the tree is for.
                // Its rows name their own session — note that both sessions
                // have a pane 10 and a pane 11, because the ids restart, so
                // the id alone cannot say which pane a row means.
                Choice::Session(SessionId(2)),
                Choice::Window { session: SessionId(2), index: 0 },
                Choice::Pane { session: SessionId(2), id: 10 },
                Choice::Pane { session: SessionId(2), id: 11 },
                // Last, and always offered: the tree is where you decide
                // which session you want, so "none of these" lives here.
                Choice::NewSession,
            ]
        );
    }

    #[test]
    fn it_opens_on_the_pane_you_are_in() {
        let c = Chooser::new(&[info(1, "work", 1)], Some(&status()));
        assert_eq!(
            c.choice(),
            Some(Choice::Pane { session: SessionId(1), id: 11 }),
            "did not start on the active pane"
        );
    }

    #[test]
    fn a_single_pane_window_still_opens_on_the_pane() {
        // The active window and its only pane are both "current"; the pane is
        // the more specific of the two and is where the cursor belongs.
        let mut st = status();
        st.windows[0].panes.truncate(1);
        st.windows[0].active_pane = 10;
        let c = Chooser::new(&[info_of(1, "work", 1, &st)], Some(&st));
        assert_eq!(c.choice(), Some(Choice::Pane { session: SessionId(1), id: 10 }));
    }

    #[test]
    fn a_single_pane_tab_still_opens_on_the_pane() {
        let mut st = status();
        st.windows[0].panes.truncate(1);
        st.windows[0].active_pane = 10;
        let c = Chooser::new(&[info_of(1, "work", 1, &st)], Some(&st));
        assert_eq!(c.choice(), Some(Choice::Pane { session: SessionId(1), id: 10 }));
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
        // One per session, now that every session's panes are listed.
        assert_eq!(c.visible().len(), 2);
        assert_eq!(c.choice(), Some(Choice::Pane { session: SessionId(1), id: 11 }));

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
