//! The status line: session, windows, panes and how long it has been open.
//!
//! It is built here rather than in either frontend because both composite the
//! same [`FrameModel`], and a status line that differed between the window and
//! the terminal would be two implementations of one thing.
//!
//! It is drawn as a [`Widget`], which is the mechanism a plugin will use for
//! the same job (`docs/PLAN.md` §15). Nothing about it is privileged; it sits
//! on the bottom row because the client never offers that row to the daemon,
//! so no pane is ever laid out underneath it.

use haven_core::{Color, Style};
use haven_proto::{CellSize, PaneRect, SessionStatus};

use crate::frame::{FrameRow, Span, Widget, WidgetKind};

/// How many rows the status line takes.
pub const ROWS: u16 = 1;

/// The size to tell the daemon about: everything except our row.
///
/// A one-row screen keeps its row — a status line that left no room for a
/// terminal would be a strange thing to insist on.
pub fn reserve(size: CellSize) -> CellSize {
    if size.rows <= ROWS {
        return size;
    }
    CellSize { rows: size.rows - ROWS, ..size }
}

/// What the client is currently doing, shown on the right of the bar.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Normal,
    /// The prefix key was pressed and the next key is a command.
    Prefix,
    /// A command is being typed: the text so far, and what it could become.
    ///
    /// The suggestions are carried rather than computed because the command
    /// table lives in the composition root, above this crate — the same
    /// reason the key table does.
    Command { text: String, suggestions: Vec<String> },
}

impl Mode {
    pub fn command(text: String) -> Self {
        Self::Command { text, suggestions: Vec::new() }
    }

    pub fn command_text(&self) -> Option<&str> {
        match self {
            Self::Command { text, .. } => Some(text),
            _ => None,
        }
    }
}

/// The completion box, drawn just above the bar while a command is being
/// typed. `None` when there is nothing to suggest, so an exact command does
/// not leave a box with one obvious entry in it.
pub fn suggestions(size: CellSize, mode: &Mode) -> Option<Widget> {
    let Mode::Command { text, suggestions } = mode else { return None };
    if suggestions.is_empty() || (suggestions.len() == 1 && suggestions[0] == *text) {
        return None;
    }
    let rows = suggestions.len().min(size.rows.saturating_sub(2) as usize);
    if rows == 0 {
        return None;
    }
    let width = suggestions.iter().map(|s| s.chars().count()).max().unwrap_or(0) + 2;
    let cols = width.min(size.cols as usize).max(4);

    let style = Style { fg: Color::Indexed(0), bg: Color::Indexed(7), ..Style::default() };
    let first = Style { fg: Color::Indexed(0), bg: Color::Indexed(11), ..Style::default() };
    let out: Vec<FrameRow> = suggestions
        .iter()
        .take(rows)
        .enumerate()
        .map(|(i, name)| FrameRow {
            spans: vec![Span {
                col: 0,
                text: pad(&format!(" {name}"), cols),
                // The first is what Tab would complete to, so it is marked.
                style: if i == 0 { first } else { style },
            }],
            rev: 0,
        })
        .collect();

    Some(Widget {
        kind: WidgetKind::Overlay,
        rect: PaneRect {
            pane: haven_proto::PaneId(u32::MAX - 2),
            col: 0,
            row: size.rows - ROWS - out.len() as u16,
            cols: cols as u16,
            rows: out.len() as u16,
        },
        rows: out,
    })
}

/// Build the bar. `now_unix` is passed in rather than read here so the whole
/// thing stays a pure function and can be tested at a fixed instant.
pub fn build(
    size: CellSize,
    status: Option<&SessionStatus>,
    mode: &Mode,
    now_unix: u64,
) -> Option<Widget> {
    if size.rows <= ROWS || size.cols == 0 {
        return None;
    }
    let cols = size.cols as usize;

    // A command being typed replaces the bar: what you are typing matters
    // more than what the session is called.
    if let Mode::Command { text, .. } = mode {
        let line = format!(":{text}");
        return Some(widget(size, pad(&line, cols), command_style()));
    }

    let left = match status {
        Some(s) => format!(" [{}] ", s.name),
        None => " [detached] ".to_string(),
    };
    let middle = status.map(windows_text).unwrap_or_default();
    let right = right_text(status, mode, now_unix);

    let mut line = String::new();
    line.push_str(&left);
    line.push_str(&middle);
    // Right-align the clock, or just append it when the bar is too narrow to
    // align anything.
    let used = line.chars().count() + right.chars().count();
    if used < cols {
        line.push_str(&" ".repeat(cols - used));
    }
    line.push_str(&right);

    Some(widget(size, pad(&line, cols), normal_style()))
}

fn windows_text(s: &SessionStatus) -> String {
    let mut out = String::new();
    for w in &s.windows {
        let marker = if w.id == s.active_window { "*" } else { " " };
        let zoom = if w.zoomed { "Z" } else { "" };
        out.push_str(&format!("{}:{}{}{} ", w.id, w.name, zoom, marker));
    }
    out
}

fn right_text(status: Option<&SessionStatus>, mode: &Mode, now_unix: u64) -> String {
    let mut parts = Vec::new();
    if *mode == Mode::Prefix {
        parts.push("PREFIX".to_string());
    }
    if let Some(s) = status {
        let panes =
            s.windows.iter().find(|w| w.id == s.active_window).map(|w| w.panes.len()).unwrap_or(0);
        parts.push(format!("{panes} panes"));
        if s.clients > 1 {
            parts.push(format!("{} clients", s.clients));
        }
        parts.push(uptime(now_unix.saturating_sub(s.started_unix)));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(" {} ", parts.join(" | "))
}

/// Elapsed time, in the largest unit that still says something useful.
pub fn uptime(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    if d > 0 {
        format!("{d}d {h:02}h")
    } else if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Exactly `cols` columns: a short line would leave the pane row showing
/// through, and a long one would wrap onto a pane.
fn pad(line: &str, cols: usize) -> String {
    let mut out: String = line.chars().take(cols).collect();
    let len = out.chars().count();
    if len < cols {
        out.push_str(&" ".repeat(cols - len));
    }
    out
}

fn widget(size: CellSize, text: String, style: Style) -> Widget {
    Widget {
        kind: WidgetKind::StatusLine,
        rect: PaneRect {
            pane: haven_proto::PaneId(u32::MAX),
            col: 0,
            row: size.rows - ROWS,
            cols: size.cols,
            rows: ROWS,
        },
        // The bar is rebuilt every frame — the clock moves on its own — so
        // there is no revision for a frontend to skip on.
        rows: vec![FrameRow { spans: vec![Span { col: 0, text, style }], rev: 0 }],
    }
}

fn normal_style() -> Style {
    // Bright white on blue. Black on blue is the obvious choice and is
    // genuinely hard to read: both are dark, and the bar is small text.
    Style { fg: Color::Indexed(15), bg: Color::Indexed(4), ..Style::default() }
}

fn command_style() -> Style {
    Style { fg: Color::Indexed(0), bg: Color::Indexed(3), ..Style::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_proto::{PaneInfo, SessionId, WindowInfo};

    fn pane(id: u32) -> PaneInfo {
        PaneInfo { id, title: format!("pane{id}") }
    }

    fn size(cols: u16, rows: u16) -> CellSize {
        CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
    }

    fn status() -> SessionStatus {
        SessionStatus {
            session: SessionId(1),
            name: "work".into(),
            windows: vec![
                WindowInfo {
                    id: 1,
                    name: "shell".into(),
                    panes: vec![pane(1), pane(2)],
                    active_pane: 1,
                    zoomed: false,
                },
                WindowInfo {
                    id: 2,
                    name: "vim".into(),
                    panes: vec![pane(3)],
                    active_pane: 3,
                    zoomed: false,
                },
            ],
            active_window: 1,
            started_unix: 1000,
            clients: 1,
        }
    }

    fn text(w: &Widget) -> String {
        w.rows[0].spans[0].text.clone()
    }

    #[test]
    fn the_daemon_is_never_offered_the_row_we_draw_on() {
        assert_eq!(reserve(size(80, 24)).rows, 23);
        // A screen with no room to spare keeps its only row: a status line
        // that left nowhere to run a shell would be useless.
        assert_eq!(reserve(size(80, 1)).rows, 1);
    }

    #[test]
    fn the_bar_names_the_session_the_windows_and_the_time() {
        let w = build(size(60, 24), Some(&status()), &Mode::Normal, 1000 + 125).unwrap();
        let line = text(&w);
        assert!(line.contains("[work]"), "no session name: {line:?}");
        assert!(line.contains("1:shell"), "no window list: {line:?}");
        assert!(line.contains("2 panes"), "no pane count: {line:?}");
        assert!(line.contains("02:05"), "no uptime: {line:?}");
    }

    #[test]
    fn the_active_window_is_the_marked_one() {
        let mut s = status();
        s.active_window = 2;
        let line = text(&build(size(60, 24), Some(&s), &Mode::Normal, 1000).unwrap());
        let shell = line.find("1:shell").unwrap();
        let vim = line.find("2:vim").unwrap();
        assert_eq!(&line[shell + "1:shell".len()..shell + "1:shell".len() + 1], " ");
        assert_eq!(&line[vim + "2:vim".len()..vim + "2:vim".len() + 1], "*");
    }

    #[test]
    fn the_bar_is_exactly_as_wide_as_the_screen() {
        // Too narrow to fit everything, and wide enough to spare: both must
        // come out exactly `cols`, or it draws over a pane or leaves a gap.
        for cols in [10u16, 40, 200] {
            let w = build(size(cols, 24), Some(&status()), &Mode::Normal, 9_999_999).unwrap();
            assert_eq!(text(&w).chars().count(), cols as usize, "width wrong at {cols}");
        }
    }

    #[test]
    fn typing_a_command_replaces_the_bar() {
        let mode = Mode::command("split-h".into());
        let line = text(&build(size(40, 24), Some(&status()), &mode, 1000).unwrap());
        assert!(line.starts_with(":split-h"), "{line:?}");
    }

    #[test]
    fn the_prefix_is_visible_while_it_is_armed() {
        let line = text(&build(size(60, 24), Some(&status()), &Mode::Prefix, 1000).unwrap());
        assert!(line.contains("PREFIX"), "{line:?}");
    }

    #[test]
    fn uptime_grows_through_its_units() {
        assert_eq!(uptime(45), "00:45");
        assert_eq!(uptime(125), "02:05");
        assert_eq!(uptime(3700), "1:01:40");
        assert_eq!(uptime(90_000), "1d 01h");
    }

    #[test]
    fn a_detached_bar_still_fills_the_row() {
        let w = build(size(30, 24), None, &Mode::Normal, 0).unwrap();
        assert_eq!(text(&w).chars().count(), 30);
    }
}

#[cfg(test)]
mod suggestion_tests {
    use super::*;

    fn size(cols: u16, rows: u16) -> CellSize {
        CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
    }

    fn mode(text: &str, names: &[&str]) -> Mode {
        Mode::Command {
            text: text.into(),
            suggestions: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_box_sits_directly_above_the_bar() {
        let m = mode("s", &["split-h", "split-v", "select-window"]);
        let w = suggestions(size(80, 24), &m).expect("box");
        assert_eq!(w.rect.row + w.rect.rows, 24 - ROWS, "not flush with the bar");
        assert_eq!(w.rows.len(), 3);
    }

    #[test]
    fn nothing_to_choose_means_no_box() {
        assert!(suggestions(size(80, 24), &mode("zoom", &[])).is_none());
        // One candidate that is already what was typed is not a suggestion.
        assert!(suggestions(size(80, 24), &mode("zoom", &["zoom"])).is_none());
        assert!(suggestions(size(80, 24), &Mode::Normal).is_none());
    }

    #[test]
    fn the_box_stays_inside_a_short_screen() {
        let names: Vec<&str> = vec!["a", "b", "c", "d", "e", "f"];
        let w = suggestions(size(20, 4), &mode("", &names)).expect("box");
        assert!(w.rect.row + w.rect.rows <= 4 - ROWS);
    }
}
