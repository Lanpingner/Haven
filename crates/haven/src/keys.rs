//! A minimal key table: the seed of the action registry in `docs/PLAN.md` §11.
//!
//! One prefix table, pushed by a prefix key and popped after one keystroke.
//! The full modal stack, the configurable bindings and the command palette are
//! M4; this exists so the mux is reachable from the keyboard today, and it is
//! deliberately shaped like the thing that replaces it: keys map to a
//! declarative [`Action`], never to inline behaviour.

use haven_mux::layout::{Direction, Edge};
use haven_mux::NamedLayout;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    SplitColumns,
    SplitRows,
    Focus(Edge),
    Resize(Edge),
    ClosePane,
    ToggleZoom,
    NewWindow,
    /// Make a session and move to it.
    NewSession,
    LastWindow,
    LastPane,
    SwapPane(Option<Edge>),
    RotateWindow {
        backwards: bool,
    },
    BreakPane,
    JoinPane {
        window: u32,
        columns: bool,
    },
    CloseWindow,
    RespawnPane,
    NextLayout,
    /// Rename the active window, or the session.
    Rename {
        window: bool,
        name: String,
    },
    NextWindow,
    PreviousWindow,
    SelectWindow(usize),
    ApplyLayout(NamedLayout),
    Detach,
    /// Send the prefix key itself to the pane.
    SendPrefix,
    /// Open the command line, with this text already on it. `command-prompt
    /// rename-window ` is how a key gets a prompt rather than acting at once.
    EnterCommand(String),
    /// Open the session tree.
    Choose,
    /// Move the view through the pane's history, in rows. Negative goes back
    /// towards the live screen.
    Scroll(i32),
    /// Enter copy mode.
    Copy,
    /// Send the newest paste buffer to the pane.
    Paste,
}

/// How far `PageUp` moves. Not a pane height, which the key table has no way
/// to know; the daemon clamps it to the history that exists.
pub const PAGE_ROWS: i32 = 20;

/// Every command name, for completion and for the help overlay.
///
/// The aliases are included deliberately: someone who types `vsplit` should
/// see it offered rather than be told it does not exist after the fact.
pub const COMMANDS: &[&str] = &[
    "choose",
    "command-prompt",
    "focus-down",
    "focus-left",
    "focus-right",
    "focus-up",
    "resize-down",
    "resize-left",
    "resize-right",
    "resize-up",
    "send-prefix",
    "new-tab",
    "new-session",
    "rename-session",
    "respawn-pane",
    "rotate",
    "swap-pane",
    "rename-window",
    "detach",
    "copy-mode",
    "paste-buffer",
    "scroll-up",
    "scroll-down",
    "scroll-end",
    "scroll-top",
    "even-horizontal",
    "even-vertical",
    "break-pane",
    "join-pane",
    "kill-pane",
    "kill-window",
    "next-layout",
    "last",
    "last-pane",
    "new-window",
    "next",
    "prev",
    "select-window",
    "split-h",
    "split-v",
    "tiled",
    "zoom",
];

/// Commands starting with `prefix`. An empty prefix offers everything, which
/// is what makes the box a menu as well as a completion.
pub fn complete(prefix: &str) -> Vec<&'static str> {
    let word = prefix.split_whitespace().next().unwrap_or("");
    if prefix.contains(char::is_whitespace) {
        // Past the verb: nothing to complete, and offering verbs again would
        // be actively misleading.
        return Vec::new();
    }
    COMMANDS.iter().copied().filter(|c| c.starts_with(word)).collect()
}

/// The longest prefix every candidate shares, which is how far Tab can go
/// without guessing.
pub fn common_prefix(candidates: &[&str]) -> String {
    let Some(first) = candidates.first() else { return String::new() };
    let mut end = first.len();
    for other in &candidates[1..] {
        end = end.min(
            first
                .chars()
                .zip(other.chars())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a.len_utf8())
                .sum(),
        );
    }
    first[..end].to_string()
}

/// Parse a typed command line.
///
/// The names are the long forms of the prefix keys rather than a new
/// vocabulary: anything a binding can do, a typed command can do, because
/// both resolve to the same [`Action`] (`docs/PLAN.md` §11.2).
fn wire_edge(edge: Edge) -> haven_proto::Edge {
    match edge {
        Edge::Left => haven_proto::Edge::Left,
        Edge::Right => haven_proto::Edge::Right,
        Edge::Up => haven_proto::Edge::Up,
        Edge::Down => haven_proto::Edge::Down,
    }
}

/// Everything after the verb, kept whole: a name is not a list of words.
fn rest_of(line: &str, verb: &str) -> String {
    line.trim().strip_prefix(verb).map(|rest| rest.trim().to_string()).unwrap_or_default()
}

pub fn parse_command(line: &str) -> Result<Action, String> {
    let mut parts = line.split_whitespace();
    let Some(verb) = parts.next() else {
        return Err(String::new());
    };
    let arg = parts.next();
    Ok(match verb {
        "split-h" | "splitw" | "sp" => Action::SplitRows,
        "split-v" | "vsplit" | "vs" => Action::SplitColumns,
        // tmux's names, plus the ones someone who calls them tabs will try.
        "new-window" | "neww" | "new-tab" => Action::NewWindow,
        "new-session" | "new" => Action::NewSession,
        "next" | "next-window" | "next-tab" => Action::NextWindow,
        "last" | "last-window" | "last-tab" => Action::LastWindow,
        "last-pane" => Action::LastPane,
        "swap-pane" => Action::SwapPane(match arg {
            Some("-L") | Some("left") => Some(Edge::Left),
            Some("-R") | Some("right") => Some(Edge::Right),
            Some("-U") | Some("up") => Some(Edge::Up),
            Some("-D") | Some("down") => Some(Edge::Down),
            _ => None,
        }),
        "rotate" | "rotate-window" => {
            Action::RotateWindow { backwards: matches!(arg, Some("-D") | Some("back")) }
        }
        "break-pane" | "breakp" => Action::BreakPane,
        "join-pane" | "joinp" => {
            let window: u32 = arg
                .ok_or_else(|| "join-pane needs a tab number".to_string())?
                .parse()
                .map_err(|_| "join-pane needs a tab number".to_string())?;
            // The third word, when there is one, picks the axis.
            let columns = !line.split_whitespace().any(|w| w == "-h" || w == "rows");
            Action::JoinPane { window, columns }
        }
        "kill-window" | "killw" | "kill-tab" => Action::CloseWindow,
        "respawn-pane" | "respawnp" => Action::RespawnPane,
        "next-layout" | "layout" => Action::NextLayout,
        "rename-window" | "rename-tab" => {
            Action::Rename { window: true, name: rest_of(line, verb) }
        }
        "rename-session" | "rename" => Action::Rename { window: false, name: rest_of(line, verb) },
        "prev" | "previous-window" | "prev-tab" => Action::PreviousWindow,
        "kill-pane" | "killp" => Action::ClosePane,
        "zoom" => Action::ToggleZoom,
        "choose" | "choose-tree" => Action::Choose,
        // Every bound key must have a name, or a binding table could not
        // express what the built-in one already does.
        "focus-left" | "select-pane-left" => Action::Focus(Edge::Left),
        "focus-right" | "select-pane-right" => Action::Focus(Edge::Right),
        "focus-up" | "select-pane-up" => Action::Focus(Edge::Up),
        "focus-down" | "select-pane-down" => Action::Focus(Edge::Down),
        "resize-left" => Action::Resize(Edge::Left),
        "resize-right" => Action::Resize(Edge::Right),
        "resize-up" => Action::Resize(Edge::Up),
        "resize-down" => Action::Resize(Edge::Down),
        "send-prefix" => Action::SendPrefix,
        "command-prompt" => Action::EnterCommand(rest_of(line, verb)),
        "copy-mode" | "copy" => Action::Copy,
        "paste-buffer" | "paste" => Action::Paste,
        "scroll-up" | "page-up" => Action::Scroll(PAGE_ROWS),
        "scroll-down" | "page-down" => Action::Scroll(-PAGE_ROWS),
        // The live screen again, whatever the history.
        "scroll-end" | "bottom" => Action::Scroll(i32::MIN),
        "scroll-top" | "top" => Action::Scroll(i32::MAX),
        "detach" | "det" => Action::Detach,
        "even-horizontal" => Action::ApplyLayout(NamedLayout::EvenHorizontal),
        "even-vertical" => Action::ApplyLayout(NamedLayout::EvenVertical),
        "tiled" => Action::ApplyLayout(NamedLayout::Tiled),
        "select-window" | "selectw" | "select-tab" => {
            let n: usize = arg
                .ok_or_else(|| "select-window needs a number".to_string())?
                .parse()
                .map_err(|_| "select-window needs a number".to_string())?;
            if n == 0 {
                return Err("windows are numbered from 1".into());
            }
            Action::SelectWindow(n - 1)
        }
        other => return Err(format!("unknown command: {other}")),
    })
}

/// Map a local action onto the wire enum. `None` means the action is handled
/// client-side (detaching, say) rather than by the daemon.
pub fn to_proto(action: Action) -> Option<haven_proto::Action> {
    use haven_proto::Action as P;
    Some(match action {
        Action::SplitColumns => P::SplitColumns,
        Action::SplitRows => P::SplitRows,
        Action::Focus(Edge::Left) => P::FocusLeft,
        Action::Focus(Edge::Right) => P::FocusRight,
        Action::Focus(Edge::Up) => P::FocusUp,
        Action::Focus(Edge::Down) => P::FocusDown,
        Action::Resize(Edge::Left) => P::ResizeLeft,
        Action::Resize(Edge::Right) => P::ResizeRight,
        Action::Resize(Edge::Up) => P::ResizeUp,
        Action::Resize(Edge::Down) => P::ResizeDown,
        Action::ClosePane => P::ClosePane,
        Action::ToggleZoom => P::ToggleZoom,
        Action::NewWindow => P::NewWindow,
        Action::NextWindow => P::NextWindow,
        Action::PreviousWindow => P::PreviousWindow,
        Action::SelectWindow(n) => P::SelectWindow(n as u16),
        Action::LastWindow => P::LastWindow,
        Action::LastPane => P::LastPane,
        Action::SwapPane(edge) => P::SwapPane(edge.map(wire_edge)),
        Action::RotateWindow { backwards } => P::RotateWindow { backwards },
        Action::BreakPane => P::BreakPane,
        Action::JoinPane { window, columns } => P::JoinPane { window, columns },
        Action::CloseWindow => P::CloseWindow,
        Action::RespawnPane => P::RespawnPane,
        Action::NextLayout => P::NextLayout,
        Action::Rename { window, name } => P::Rename { window, name },
        Action::ApplyLayout(NamedLayout::EvenHorizontal) => P::EvenHorizontal,
        Action::ApplyLayout(NamedLayout::EvenVertical) => P::EvenVertical,
        Action::ApplyLayout(NamedLayout::Tiled) => P::Tiled,
        // Client-side: these need the frontend's size, or the client's own
        // state, neither of which the daemon's action table has.
        Action::Detach
        | Action::SendPrefix
        | Action::EnterCommand(_)
        | Action::Choose
        | Action::NewSession
        | Action::Scroll(_)
        | Action::Copy
        | Action::Paste => return None,
    })
}

/// The direction a split action implies.
pub fn split_direction(action: Action) -> Option<Direction> {
    match action {
        Action::SplitColumns => Some(Direction::Columns),
        Action::SplitRows => Some(Direction::Rows),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use haven_client::bindings::{Bindings, Table};
    use haven_client::input::{KeyEvent, Modifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::char(c)
    }

    /// Resolve a key through the default table exactly as the client does.
    fn bound(event: &KeyEvent) -> Option<Action> {
        Bindings::default().command(Table::Prefix, event).and_then(|c| parse_command(c).ok())
    }

    #[test]
    fn the_prefix_is_recognised_and_nothing_else_is() {
        let b = Bindings::default();
        assert!(b.is_prefix(&key('a').with(Modifiers::CTRL)));
        assert!(!b.is_prefix(&key('a')));
        assert!(!b.is_prefix(&key('b').with(Modifiers::CTRL)));
    }

    #[test]
    fn prefix_bindings_resolve_to_actions() {
        assert_eq!(bound(&key('%')), Some(Action::SplitColumns));
        assert_eq!(bound(&key('"')), Some(Action::SplitRows));
        assert_eq!(bound(&key('z')), Some(Action::ToggleZoom));
        assert_eq!(bound(&key('l')), Some(Action::Focus(Edge::Right)));
        assert_eq!(bound(&key('L')), Some(Action::Resize(Edge::Right)));
        assert_eq!(bound(&key('3')), Some(Action::SelectWindow(2)));
    }

    #[test]
    fn pressing_the_prefix_twice_sends_it_through() {
        assert_eq!(bound(&key('a').with(Modifiers::CTRL)), Some(Action::SendPrefix));
    }

    #[test]
    fn an_unbound_key_resolves_to_nothing_rather_than_a_default() {
        assert_eq!(bound(&key('Q')), None);
    }

    #[test]
    fn every_mux_action_has_a_wire_form() {
        // Anything a binding can do must be expressible to the daemon, or the
        // same keymap would behave differently attached and standalone.
        for action in [
            Action::SplitColumns,
            Action::SplitRows,
            Action::Focus(Edge::Left),
            Action::Resize(Edge::Down),
            Action::ClosePane,
            Action::ToggleZoom,
            Action::NewWindow,
            Action::NextWindow,
            Action::PreviousWindow,
            Action::SelectWindow(3),
            Action::ApplyLayout(NamedLayout::Tiled),
            Action::LastWindow,
            Action::LastPane,
            Action::Rename { window: true, name: "x".into() },
        ] {
            assert!(to_proto(action.clone()).is_some(), "{action:?} has no wire form");
        }
        // Client-side only.
        assert!(to_proto(Action::Detach).is_none());
        assert!(to_proto(Action::SendPrefix).is_none());
    }

    #[test]
    fn split_direction_only_answers_for_splits() {
        assert_eq!(split_direction(Action::SplitColumns), Some(Direction::Columns));
        assert_eq!(split_direction(Action::SplitRows), Some(Direction::Rows));
        assert_eq!(split_direction(Action::ToggleZoom), None);
    }

    #[test]
    fn completion_offers_what_starts_with_the_word() {
        assert_eq!(complete("spl"), vec!["split-h", "split-v"]);
        assert_eq!(complete("zo"), vec!["zoom"]);
        assert!(complete("qqq").is_empty());
        // An empty line offers the whole menu, which is what makes the box
        // useful before you know the names.
        assert_eq!(complete("").len(), COMMANDS.len());
    }

    #[test]
    fn completion_stops_at_the_verb() {
        // Past the first word there is nothing to complete, and offering
        // verbs again would suggest `select-window select-window`.
        assert!(complete("select-window ").is_empty());
        assert!(complete("select-window 2").is_empty());
    }

    #[test]
    fn tab_completes_only_as_far_as_the_candidates_agree() {
        assert_eq!(common_prefix(&complete("spl")), "split-");
        assert_eq!(common_prefix(&complete("zo")), "zoom");
        assert_eq!(common_prefix(&[]), "");
    }

    #[test]
    fn every_offered_command_actually_parses() {
        // A completion that suggests something `parse_command` rejects would
        // be worse than no completion at all.
        for name in COMMANDS {
            // Two take a number, and the rest stand alone.
            let line = match *name {
                "select-window" | "join-pane" => format!("{name} 1"),
                other => other.to_string(),
            };
            assert!(parse_command(&line).is_ok(), "{name} is offered but does not parse");
        }
    }
}
