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

use haven_core::Style;
use haven_proto::{CellSize, PaneRect, SessionStatus};

use crate::frame::{FrameRow, Span, Widget, WidgetKind};
use crate::theme::Theme;

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

/// What else the bar has to say, beyond the session itself.
///
/// A struct rather than three more arguments: the list was already at the
/// point where the call site said nothing about which `None` was which.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct BarState {
    /// How far the focused pane is scrolled back, when it is.
    pub scroll: Option<(u32, u32)>,
    /// Copy mode's label, when it is up.
    pub copy: Option<String>,
    /// Something to tell the user, until the next keystroke. It outranks
    /// everything else on the right of the bar: it is the answer to a thing
    /// that just happened, and the rest is standing information.
    pub notice: Option<String>,
}

/// What the client is currently doing, shown on the right of the bar.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Normal,
    /// A key table is armed and the next key is looked up in it. `prefix`
    /// is the one-shot case that has always existed; a table of your own
    /// stays armed, which is what makes the key handling modal.
    Table(String),
    /// A command is being typed: the text so far, what it could become, and
    /// which of those is highlighted.
    ///
    /// The suggestions are carried rather than computed because the command
    /// table lives in the composition root, above this crate — the same
    /// reason the key table does.
    Command { text: String, suggestions: Vec<Suggestion>, selected: usize },
}

/// A completion, and how to press it instead.
///
/// The key is kept apart from the command rather than written into it: the
/// arrows put the highlighted entry on the command line, so a label reading
/// `split-v   C-a %` would have typed the key onto the line as well, and
/// Enter would have run a command nobody wrote.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Suggestion {
    pub command: String,
    /// How to press it, as a person would write it. Empty when nothing is
    /// bound to it, which is most of the vocabulary.
    pub keys: String,
    /// A symbol for the family it belongs to, or empty.
    pub icon: String,
}

impl Suggestion {
    pub fn new(command: impl Into<String>) -> Self {
        Self { command: command.into(), keys: String::new(), icon: String::new() }
    }

    pub fn with_keys(mut self, keys: impl Into<String>) -> Self {
        self.keys = keys.into();
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    fn width(&self) -> usize {
        self.command.chars().count()
            + if self.keys.is_empty() { 0 } else { self.keys.chars().count() + 2 }
    }
}

impl Mode {
    pub fn command(text: String) -> Self {
        Self::Command { text, suggestions: Vec::new(), selected: 0 }
    }

    pub fn command_text(&self) -> Option<&str> {
        match self {
            Self::Command { text, .. } => Some(text),
            _ => None,
        }
    }

    /// The armed table, if one is.
    pub fn table(&self) -> Option<&str> {
        match self {
            Self::Table(name) => Some(name),
            _ => None,
        }
    }

    pub fn prefix() -> Self {
        Self::Table(crate::bindings::PREFIX.to_string())
    }
}

/// The completion box, drawn just above the bar while a command is being
/// typed. `None` when there is nothing to suggest, so an exact command does
/// not leave a box with one obvious entry in it.
pub fn suggestions(size: CellSize, mode: &Mode, theme: &Theme) -> Option<Widget> {
    let Mode::Command { text, suggestions, selected } = mode else { return None };
    if suggestions.is_empty() || (suggestions.len() == 1 && suggestions[0].command == *text) {
        return None;
    }
    // Two rows go to the border, and the bar keeps its own.
    let room = (size.rows as usize).saturating_sub(ROWS as usize + 2);
    let shown = suggestions.len().min(room);
    if shown == 0 {
        return None;
    }
    // Keep the highlighted entry on screen when the list is longer than the
    // room for it: an arrow that moves a selection you cannot see is worse
    // than no arrow at all.
    let first = selected.saturating_sub(shown - 1);
    // One column for the symbols or none at all, rather than every row
    // indented past an empty one.
    let icons = theme.glyphs.icons
        && suggestions.iter().skip(first).take(shown).any(|s| !s.icon.is_empty());
    // A space, the symbol in two columns, a space — the width the row
    // format below actually spends, so a long command and its key cannot
    // meet in the middle.
    let lead = if icons { 4 } else { 0 };
    let widest =
        suggestions.iter().skip(first).take(shown).map(Suggestion::width).max().unwrap_or(0) + lead;
    let inner = widest.max("completions".len());
    let cols = (inner + 4).min(size.cols as usize);
    let inner = cols.saturating_sub(4);

    let g = &theme.glyphs;
    let mut out = Vec::with_capacity(shown + 2);
    out.push(border_row(g, cols, " completions ", theme, true));
    for (i, entry) in suggestions.iter().enumerate().skip(first).take(shown) {
        // The highlighted one is what the arrows chose and what Enter runs.
        let selected = i == *selected;
        let style = if selected { theme.selection.style() } else { theme.panel.style() };
        let mut spans = vec![
            Span { col: 0, text: g.vertical.clone(), style: theme.panel_border.style() },
            Span {
                col: 1,
                text: pad(
                    &if icons {
                        format!(" {:<2} {}", entry.icon, entry.command)
                    } else {
                        format!(" {}", entry.command)
                    },
                    inner + 2,
                ),
                style,
            },
            Span {
                col: (inner + 3) as u16,
                text: g.vertical.clone(),
                style: theme.panel_border.style(),
            },
        ];
        // The key sits at the right edge, dim, so the column of commands
        // stays a column and the keys read as an annotation on it. On the
        // highlighted row it takes the highlight with everything else: a
        // selected line in two colours reads as two lines.
        let keys = entry.keys.chars().count();
        if keys > 0 && keys + 2 < inner + 2 {
            spans.insert(
                2,
                Span {
                    col: (inner + 2 - keys) as u16,
                    text: format!("{} ", entry.keys),
                    style: if selected { style } else { theme.panel_dim.style() },
                },
            );
        }
        out.push(FrameRow { spans, rev: 0 });
    }
    out.push(border_row(g, cols, "", theme, false));

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

/// A number over each pane, as `display-panes` does.
///
/// One widget per pane rather than one big one, so each sits over its own
/// pane wherever the layout put it.
pub fn pane_numbers(
    status: Option<&SessionStatus>,
    focus: Option<haven_proto::PaneId>,
    theme: &Theme,
) -> Vec<Widget> {
    let Some(status) = status else { return Vec::new() };
    let Some(window) = status.windows.iter().find(|w| w.id == status.active_window) else {
        return Vec::new();
    };
    window
        .panes
        .iter()
        .enumerate()
        .filter(|(_, p)| p.rect.cols > 0 && p.rect.rows > 0)
        .map(|(i, pane)| {
            let label = format!(" {} ", i + 1);
            let style = if focus == Some(haven_proto::PaneId(pane.id)) {
                theme.selection.style()
            } else {
                theme.panel_title.style()
            };
            Widget {
                kind: WidgetKind::Overlay,
                rect: PaneRect {
                    pane: haven_proto::PaneId(u32::MAX - 3),
                    // Top-left of the pane, which is the one corner every
                    // layout has free.
                    col: pane.rect.col,
                    row: pane.rect.row,
                    cols: (label.chars().count() as u16).min(pane.rect.cols),
                    rows: 1,
                },
                rows: vec![FrameRow { spans: vec![Span { col: 0, text: label, style }], rev: 0 }],
            }
        })
        .collect()
}

/// A top or bottom border, with an optional title set into it.
pub fn border_row(
    g: &crate::theme::Glyphs,
    cols: usize,
    title: &str,
    theme: &Theme,
    top: bool,
) -> FrameRow {
    let (left, right) =
        if top { (&g.top_left, &g.top_right) } else { (&g.bottom_left, &g.bottom_right) };
    let inner = cols.saturating_sub(2);
    let title: String = title.chars().take(inner).collect();
    let bars = inner - title.chars().count();
    let before = if title.is_empty() { bars } else { 1.min(bars) };
    let after = bars - before;

    let mut spans = vec![Span {
        col: 0,
        text: format!("{left}{}", g.horizontal.repeat(before)),
        style: theme.panel_border.style(),
    }];
    let mut at = 1 + before;
    if !title.is_empty() {
        let width = title.chars().count();
        spans.push(Span { col: at as u16, text: title, style: theme.panel_title.style() });
        at += width;
    }
    spans.push(Span {
        col: at as u16,
        text: format!("{}{right}", g.horizontal.repeat(after)),
        style: theme.panel_border.style(),
    });
    FrameRow { spans, rev: 0 }
}

/// Build the bar. `now_unix` is passed in rather than read here so the whole
/// thing stays a pure function and can be tested at a fixed instant.
pub fn build(
    size: CellSize,
    status: Option<&SessionStatus>,
    mode: &Mode,
    now_unix: u64,
    theme: &Theme,
    state: &BarState,
) -> Option<Widget> {
    if size.rows <= ROWS || size.cols == 0 {
        return None;
    }
    let cols = size.cols as usize;

    // A command being typed replaces the bar: what you are typing matters
    // more than what the session is called.
    if let Mode::Command { text, .. } = mode {
        let mut spans = Spans::new(cols);
        spans.push(" : ", theme.bar_session.style());
        // A block stands in for the cursor, which the pane still owns.
        spans.push(&format!("{text}\u{2588}"), theme.command.style());
        spans.fill(theme.command.style());
        return Some(widget(size, spans.finish()));
    }

    let mut spans = Spans::new(cols);
    match status {
        Some(s) => spans.push(&format!(" {} ", s.name), theme.bar_session.style()),
        None => spans.push(" detached ", theme.bar_alert.style()),
    }

    // Tabs, each its own span so the current one can be painted rather than
    // marked with a character the eye has to hunt for.
    if let Some(s) = status {
        spans.push(" ", theme.bar.style());
        for w in &s.windows {
            let active = w.id == s.active_window;
            let zoom = if w.zoomed { " \u{25a3}" } else { "" };
            let sync = if w.synchronized { " \u{2261}" } else { "" };
            // A bell outranks activity: it was asked for, output was not.
            let bell = match (w.bell, w.activity) {
                (true, _) => " \u{25cf}",
                (false, true) => " \u{25cb}",
                _ => "",
            };
            let label = format!(" {}:{}{}{}{} ", w.id, w.name, zoom, sync, bell);
            let style = match (active, w.bell) {
                (true, _) => theme.tab_active.style(),
                // A rung tab stands out, or the mark is no better than none.
                // Activity gets a mark but not a colour: it is information,
                // not a summons, and painting every busy tab is noise.
                (false, true) => theme.bar_alert.style(),
                (false, false) => theme.tab.style(),
            };
            spans.push(&label, style);
            spans.push(" ", theme.bar.style());
        }
    }

    let right = right_text(status, now_unix, theme);
    // Reading history is a mode, and an interface that does not say so leaves
    // you wondering why the screen stopped moving.
    // Most urgent first: an answer to what just happened, then what you are
    // typing, then what key is armed, then where in the history you are.
    if let Some(notice) = &state.notice {
        let mut spans = Spans::new(cols);
        spans.push(" ", theme.bar.style());
        spans.push(&format!(" {notice} "), theme.bar_alert.style());
        spans.fill(theme.bar.style());
        return Some(widget(size, spans.finish()));
    }
    let alert = match (state.copy.as_ref(), mode.table(), state.scroll) {
        (Some(label), _, _) => Some(format!(" {label} ")),
        // The table's own name, so a mode of your own says which one it is
        // rather than all of them saying `PREFIX`.
        (_, Some(name), _) => Some(format!(" {} ", name.to_uppercase())),
        (_, _, Some((offset, total))) => Some(format!(" SCROLL {offset}/{total} ")),
        _ => None,
    };
    // Whose pane you are typing into, when it is not yours. Standing
    // information rather than an answer to something that just happened, so
    // it keeps its own place beside the mode alert instead of taking turns
    // with it: a root pane is root whether or not you are also in copy mode,
    // and that is the one you would least like to be wrong about.
    let user = elevated(status).map(|user| format!(" {user} "));
    let tail = alert.as_ref().map(|a| a.chars().count()).unwrap_or(0)
        + user.as_ref().map(|u| u.chars().count()).unwrap_or(0)
        + right.chars().count();
    spans.fill_to(cols.saturating_sub(tail), theme.bar.style());
    if let Some(user) = user {
        spans.push(&user, theme.bar_alert.style());
    }
    if let Some(alert) = alert {
        spans.push(&alert, theme.bar_alert.style());
    }
    spans.push(&right, theme.bar_dim.style());
    spans.fill(theme.bar.style());

    Some(widget(size, spans.finish()))
}

/// Builds one row out of styled runs, tracking how wide it has become so the
/// right-hand end can be placed exactly.
struct Spans {
    cols: usize,
    used: usize,
    spans: Vec<Span>,
}

impl Spans {
    fn new(cols: usize) -> Self {
        Self { cols, used: 0, spans: Vec::new() }
    }

    fn push(&mut self, text: &str, style: Style) {
        if self.used >= self.cols || text.is_empty() {
            return;
        }
        let room = self.cols - self.used;
        let text: String = text.chars().take(room).collect();
        let width = text.chars().count();
        self.spans.push(Span { col: self.used as u16, text, style });
        self.used += width;
    }

    /// Pad with blanks until the row is `target` wide.
    fn fill_to(&mut self, target: usize, style: Style) {
        if target > self.used {
            let n = target - self.used;
            self.push(&" ".repeat(n), style);
        }
    }

    fn fill(&mut self, style: Style) {
        self.fill_to(self.cols, style);
    }

    fn finish(self) -> Vec<Span> {
        self.spans
    }
}

/// The user the focused pane is running as, when it is not Haven's own.
fn elevated(status: Option<&SessionStatus>) -> Option<&str> {
    let s = status?;
    let window = s.windows.iter().find(|w| w.id == s.active_window)?;
    let pane = window.panes.iter().find(|p| p.id == window.active_pane)?;
    pane.user.as_deref()
}

fn right_text(status: Option<&SessionStatus>, now_unix: u64, theme: &Theme) -> String {
    let Some(s) = status else { return String::new() };
    let panes =
        s.windows.iter().find(|w| w.id == s.active_window).map(|w| w.panes.len()).unwrap_or(0);
    let sep = &theme.glyphs.separator;
    let mut parts = vec![format!("{panes} panes")];
    if s.clients > 1 {
        parts.push(format!("{} clients", s.clients));
    }
    parts.push(uptime(now_unix.saturating_sub(s.started_unix)));
    format!(" {} ", parts.join(&format!(" {sep} ")))
}

fn widget(size: CellSize, spans: Vec<Span>) -> Widget {
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
        rows: vec![FrameRow { spans, rev: 0 }],
    }
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

/// Exactly `cols` columns, so a panel row never leaves a pane showing
/// through it or spills onto the next one.
pub fn pad(text: &str, cols: usize) -> String {
    let mut out: String = text.chars().take(cols).collect();
    let len = out.chars().count();
    if len < cols {
        out.push_str(&" ".repeat(cols - len));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_proto::{PaneInfo, SessionId, WindowInfo};

    fn pane(id: u32) -> PaneInfo {
        PaneInfo {
            id,
            rect: Default::default(),
            title: format!("pane{id}"),
            command: String::new(),
            running: false,
            user: None,
        }
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
                    synchronized: false,
                    bell: false,
                    activity: false,
                },
                WindowInfo {
                    id: 2,
                    name: "vim".into(),
                    panes: vec![pane(3)],
                    active_pane: 3,
                    zoomed: false,
                    synchronized: false,
                    bell: false,
                    activity: false,
                },
            ],
            active_window: 1,
            started_unix: 1000,
            clients: 1,
        }
    }

    /// The whole row, as one string: the bar is several styled runs now, so
    /// a test that looked at only the first span would miss most of it.
    fn text(w: &Widget) -> String {
        w.rows[0].spans.iter().map(|s| s.text.as_str()).collect()
    }

    fn theme() -> Theme {
        Theme::haven()
    }

    #[test]
    fn a_root_pane_says_so_on_the_bar() {
        // A pane you ran `sudo -i` in an hour ago looks exactly like one
        // you did not, and the shell's own prompt is the only thing saying
        // otherwise — in whichever window you are not looking at.
        let plain = status();
        let mut elevated = plain.clone();
        elevated.windows[0].panes[0].user = Some("root".into());
        let bar = |s: &SessionStatus| {
            text(
                &build(size(80, 24), Some(s), &Mode::Normal, 1000, &theme(), &Default::default())
                    .unwrap(),
            )
        };
        assert!(bar(&elevated).contains("root"), "{:?}", bar(&elevated));
        // And an ordinary pane does not, or the mark means nothing.
        assert!(!bar(&plain).contains("root"), "{:?}", bar(&plain));
    }

    #[test]
    fn the_root_mark_does_not_take_turns_with_the_mode() {
        // It is standing information, not an answer to something that just
        // happened: a root pane is root whether or not you are also in copy
        // mode, and that is the one you would least like to be wrong about.
        let mut status = status();
        status.windows[0].panes[0].user = Some("root".into());
        let state = BarState { copy: Some("COPY".into()), ..Default::default() };
        let line = text(
            &build(size(80, 24), Some(&status), &Mode::Normal, 1000, &theme(), &state).unwrap(),
        );
        assert!(line.contains("root"), "{line:?}");
        assert!(line.contains("COPY"), "{line:?}");
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
        let w = build(
            size(60, 24),
            Some(&status()),
            &Mode::Normal,
            1000 + 125,
            &theme(),
            &BarState::default(),
        )
        .unwrap();
        let line = text(&w);
        assert!(line.contains("work"), "no session name: {line:?}");
        assert!(line.contains("1:shell"), "no window list: {line:?}");
        assert!(line.contains("2 panes"), "no pane count: {line:?}");
        assert!(line.contains("02:05"), "no uptime: {line:?}");
    }

    #[test]
    fn the_active_tab_is_painted_not_merely_marked() {
        // It used to be a trailing asterisk the eye had to hunt for. The
        // current tab now gets its own style, so the test looks at the style
        // rather than at a character.
        let mut s = status();
        s.active_window = 2;
        let theme = theme();
        let w = build(size(60, 24), Some(&s), &Mode::Normal, 1000, &theme, &BarState::default())
            .unwrap();
        let active = w.rows[0]
            .spans
            .iter()
            .find(|sp| sp.text.contains("2:vim"))
            .expect("no span for the active tab");
        let inactive = w.rows[0]
            .spans
            .iter()
            .find(|sp| sp.text.contains("1:shell"))
            .expect("no span for the other tab");
        assert_eq!(active.style, theme.tab_active.style());
        assert_eq!(inactive.style, theme.tab.style());
        assert_ne!(active.style, inactive.style);
    }

    #[test]
    fn the_bar_is_exactly_as_wide_as_the_screen() {
        // Too narrow to fit everything, and wide enough to spare: both must
        // come out exactly `cols`, or it draws over a pane or leaves a gap.
        for cols in [10u16, 40, 200] {
            let w = build(
                size(cols, 24),
                Some(&status()),
                &Mode::Normal,
                9_999_999,
                &theme(),
                &BarState::default(),
            )
            .unwrap();
            assert_eq!(text(&w).chars().count(), cols as usize, "width wrong at {cols}");
        }
    }

    #[test]
    fn typing_a_command_replaces_the_bar() {
        let mode = Mode::command("split-h".into());
        let line = text(
            &build(size(40, 24), Some(&status()), &mode, 1000, &theme(), &BarState::default())
                .unwrap(),
        );
        // A `:` badge, then what is being typed, then a block for the
        // cursor — the pane still owns the real one.
        assert!(line.contains(':'), "no prompt: {line:?}");
        assert!(line.contains("split-h\u{2588}"), "no text or cursor: {line:?}");
        assert_eq!(line.chars().count(), 40, "the command line must fill the row");
    }

    #[test]
    fn a_notice_takes_the_whole_bar_and_outranks_everything() {
        // It is the answer to something that just happened; the session name
        // and the clock are standing information and can wait a keystroke.
        let state = BarState {
            scroll: Some((7, 40)),
            copy: Some("COPY".into()),
            notice: Some("A-c is not bound in prefix".into()),
        };
        let line = text(
            &build(size(60, 24), Some(&status()), &Mode::Normal, 1000, &theme(), &state).unwrap(),
        );
        assert!(line.contains("A-c is not bound in prefix"), "{line:?}");
        assert!(!line.contains("SCROLL"), "the notice should have the row to itself: {line:?}");
    }

    #[test]
    fn reading_history_is_shown_in_the_bar() {
        // A screen that stopped moving with nothing saying why is the thing
        // this avoids.
        let line = text(
            &build(
                size(60, 24),
                Some(&status()),
                &Mode::Normal,
                1000,
                &theme(),
                &BarState { scroll: Some((7, 40)), ..Default::default() },
            )
            .unwrap(),
        );
        assert!(line.contains("SCROLL 7/40"), "{line:?}");
    }

    #[test]
    fn the_prefix_is_visible_while_it_is_armed() {
        let line = text(
            &build(
                size(60, 24),
                Some(&status()),
                &Mode::prefix(),
                1000,
                &theme(),
                &BarState::default(),
            )
            .unwrap(),
        );
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
        let w =
            build(size(30, 24), None, &Mode::Normal, 0, &theme(), &BarState::default()).unwrap();
        assert_eq!(text(&w).chars().count(), 30);
    }
}

#[cfg(test)]
mod suggestion_tests {
    use super::*;
    use crate::theme::Theme;

    fn size(cols: u16, rows: u16) -> CellSize {
        CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
    }

    fn mode(text: &str, names: &[&str]) -> Mode {
        chosen(text, names, 0)
    }

    fn chosen(text: &str, names: &[&str], selected: usize) -> Mode {
        Mode::Command {
            text: text.into(),
            suggestions: names.iter().map(|s| Suggestion::new(*s)).collect(),
            selected,
        }
    }

    #[test]
    fn pane_numbers_sit_over_their_own_panes() {
        use haven_proto::{PaneInfo, PaneRect, SessionId, WindowInfo};
        let at = |id, col, row, cols, rows| PaneInfo {
            id,
            rect: PaneRect { pane: haven_proto::PaneId(id), col, row, cols, rows },
            title: String::new(),
            command: String::new(),
            running: false,
            user: None,
        };
        let status = SessionStatus {
            session: SessionId(1),
            name: "s".into(),
            windows: vec![WindowInfo {
                id: 1,
                synchronized: false,
                bell: false,
                activity: false,
                name: "w".into(),
                panes: vec![at(1, 0, 0, 10, 5), at(2, 11, 0, 10, 5)],
                active_pane: 2,
                zoomed: false,
            }],
            active_window: 1,
            started_unix: 0,
            clients: 1,
        };
        let widgets = pane_numbers(Some(&status), Some(haven_proto::PaneId(2)), &Theme::haven());
        assert_eq!(widgets.len(), 2);
        assert_eq!((widgets[0].rect.col, widgets[0].rect.row), (0, 0));
        assert_eq!((widgets[1].rect.col, widgets[1].rect.row), (11, 0));
        assert_eq!(widgets[0].rows[0].spans[0].text.trim(), "1");
        // The focused one is painted differently, or the numbers say nothing
        // about where you already are.
        assert_ne!(widgets[0].rows[0].spans[0].style, widgets[1].rows[0].spans[0].style);
    }

    #[test]
    fn a_pane_with_no_room_gets_no_number() {
        use haven_proto::{PaneInfo, PaneRect, SessionId, WindowInfo};
        let status = SessionStatus {
            session: SessionId(1),
            name: "s".into(),
            windows: vec![WindowInfo {
                id: 1,
                synchronized: false,
                bell: false,
                activity: false,
                name: "w".into(),
                panes: vec![PaneInfo {
                    id: 1,
                    rect: PaneRect {
                        pane: haven_proto::PaneId(1),
                        col: 0,
                        row: 0,
                        cols: 0,
                        rows: 0,
                    },
                    title: String::new(),
                    command: String::new(),
                    running: false,
                    user: None,
                }],
                active_pane: 1,
                zoomed: false,
            }],
            active_window: 1,
            started_unix: 0,
            clients: 1,
        };
        assert!(pane_numbers(Some(&status), None, &Theme::haven()).is_empty());
    }

    #[test]
    fn a_completion_shows_its_key_and_its_symbol() {
        let m = Mode::Command {
            text: "s".into(),
            suggestions: vec![
                Suggestion::new("split-v").with_icon("◫").with_keys("C-a %"),
                Suggestion::new("swap-pane").with_icon("⇄").with_keys("C-a o"),
                // Bound to nothing, which is most of the vocabulary.
                Suggestion::new("synchronize-panes").with_icon("❯"),
            ],
            selected: 0,
        };
        let w = suggestions(size(80, 24), &m, &Theme::haven()).expect("box");
        let line = |n: usize| -> String {
            let mut cells = vec![' '; w.rect.cols as usize + 4];
            for span in &w.rows[n].spans {
                for (i, c) in span.text.chars().enumerate() {
                    let at = span.col as usize + i;
                    if at < cells.len() {
                        cells[at] = c;
                    }
                }
            }
            cells.iter().collect()
        };
        assert!(line(1).contains("◫"), "no symbol: {:?}", line(1));
        assert!(line(1).contains("split-v"), "{:?}", line(1));
        // The key at the right edge, and clear of the command: they used to
        // be able to meet in the middle, because the width reserved for the
        // symbol column was a column short of what the row spends on it.
        assert!(line(1).trim_end().ends_with("C-a % │"), "{:?}", line(1));
        assert!(line(1).contains("split-v  "), "the key crowded the command: {:?}", line(1));
        // And the longest command, which is the one at risk.
        assert!(line(3).contains("synchronize-panes"), "{:?}", line(3));
    }

    #[test]
    fn the_ascii_theme_has_no_symbols_to_draw() {
        // The same question the box set answers: a terminal that cannot
        // manage `─` will make nothing of `◫`.
        let m = Mode::Command {
            text: "s".into(),
            suggestions: vec![
                Suggestion::new("split-v").with_icon("◫").with_keys("C-a %"),
                Suggestion::new("swap-pane").with_icon("⇄"),
            ],
            selected: 0,
        };
        let w = suggestions(size(80, 24), &m, &Theme::plain()).expect("box");
        let text: String =
            w.rows.iter().flat_map(|r| r.spans.iter()).map(|s| s.text.clone()).collect();
        assert!(!text.contains('◫'), "{text:?}");
        // The key is not a glyph and stays.
        assert!(text.contains("C-a %"), "{text:?}");
        // And no column is left standing empty where they were.
        assert!(text.contains(" split-v"), "{text:?}");
        assert!(!text.contains("   split-v"), "an empty symbol column: {text:?}");
    }

    #[test]
    fn the_box_sits_directly_above_the_bar() {
        let m = mode("s", &["split-h", "split-v", "select-window"]);
        let w = suggestions(size(80, 24), &m, &Theme::haven()).expect("box");
        assert_eq!(w.rect.row + w.rect.rows, 24 - ROWS, "not flush with the bar");
        // Three entries plus a border above and below.
        assert_eq!(w.rows.len(), 5);
    }

    #[test]
    fn nothing_to_choose_means_no_box() {
        let t = Theme::haven();
        assert!(suggestions(size(80, 24), &mode("zoom", &[]), &t).is_none());
        // One candidate that is already what was typed is not a suggestion.
        assert!(suggestions(size(80, 24), &mode("zoom", &["zoom"]), &t).is_none());
        assert!(suggestions(size(80, 24), &Mode::Normal, &t).is_none());
    }

    #[test]
    fn the_highlight_follows_the_selection() {
        let m = chosen("s", &["one", "two", "three"], 2);
        let w = suggestions(size(40, 24), &m, &Theme::haven()).expect("a box");
        // Rows: border, three entries, border. The painted one is the third.
        let painted: Vec<bool> = w.rows[1..4]
            .iter()
            .map(|r| r.spans[1].style == Theme::haven().selection.style())
            .collect();
        assert_eq!(painted, vec![false, false, true], "the wrong entry is highlighted");
    }

    #[test]
    fn a_selection_below_the_fold_is_still_shown() {
        // Two rows of room, a selection at the bottom of a longer list: the
        // window has to move, or the arrows would appear to do nothing.
        let names: Vec<String> = (0..8).map(|i| format!("cmd{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let m = chosen("c", &refs, 7);
        let w = suggestions(size(40, 5), &m, &Theme::haven()).expect("a box");
        let shown: Vec<String> = w.rows[1..w.rows.len() - 1]
            .iter()
            .map(|r| r.spans[1].text.trim().to_string())
            .collect();
        assert!(shown.contains(&"cmd7".to_string()), "the selection scrolled off: {shown:?}");
    }

    #[test]
    fn the_box_stays_inside_a_short_screen() {
        let names: Vec<&str> = vec!["a", "b", "c", "d", "e", "f"];
        let w = suggestions(size(20, 4), &mode("", &names), &Theme::haven()).expect("box");
        assert!(w.rect.row + w.rect.rows <= 4 - ROWS);
    }
}
