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
    /// A program to run instead of the session's usual shell, when one was
    /// named. `split-v fish` is the whole of it.
    SplitColumns {
        program: Option<String>,
    },
    SplitRows {
        program: Option<String>,
    },
    Focus(Edge),
    Resize(Edge),
    ClosePane,
    ToggleZoom,
    NewWindow {
        program: Option<String>,
    },
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
    RespawnPane {
        program: Option<String>,
    },
    /// Offer the daemon's shells, and run the one picked with this command.
    /// `new-window` by default, so the key that offers a shell does the same
    /// thing as the key that makes a tab. Containers are in the same menu:
    /// which shell and where it runs are one decision.
    ChooseShell(String),
    /// A container picked from that menu, with one question left: does what
    /// you install in it outlive the pane. The menu's second step, and a
    /// command of its own so the menu stays data rather than code.
    ContainerShell {
        verb: String,
        spec: String,
    },
    /// Which host directories container panes are given. Both `None` asks
    /// what they are. A command rather than a config key because a mount can
    /// only be chosen before the container starts, so it belongs to opening
    /// the pane — and because nothing of the machine is mounted by default,
    /// which is the right default and the wrong one to be stuck with.
    ContainerMounts {
        add: Option<String>,
        remove: Option<String>,
    },
    SaveSessions,
    RestoreSessions,
    /// Every command, as a list you narrow by typing.
    CommandPalette,
    /// Arm a named key table. The one modal primitive: everything else is a
    /// table binding `switch-table` to get into and out of it.
    SwitchTable(String),
    SendKeys {
        pane: Option<u32>,
        bytes: Vec<u8>,
    },
    SynchronizePanes {
        on: Option<bool>,
    },
    DetachOthers,
    /// Number every pane on screen until a key is pressed.
    DisplayPanes,
    /// Go to the tab whose name contains this.
    FindWindow(String),
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
    /// Bigger or smaller text. Not [`Action::ToggleZoom`], which is the
    /// pane zoom the prefix key does: one letter, two meanings, so these
    /// are spelled out as fonts rather than as zoom.
    Font(haven_client::Zoom),
    /// The focused pane's command history, with what each command is
    /// beside it.
    History,
    /// Every line matching this, in every pane of the session.
    Grep(String),
    /// Go to one of those lines. Written by the grep menu rather than by a
    /// person, which is why it takes three numbers and no names.
    GoTo {
        window: u16,
        pane: u32,
        row: u32,
    },
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
    "choose-shell",
    "container-mount",
    "container-mounts",
    "container-unmount",
    "command-palette",
    "switch-table",
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
    "restore-sessions",
    "save-sessions",
    "rotate",
    "send-keys",
    "swap-pane",
    "synchronize-panes",
    "rename-window",
    "detach",
    "detach-others",
    "display-panes",
    "find-window",
    "copy-mode",
    "paste-buffer",
    "grep",
    "history",
    "font-bigger",
    "font-smaller",
    "font-reset",
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
/// Decode the escapes a script may write in a `send-keys` argument.
pub fn unescape(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('e') => out.push(0x1b),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(byte) => out.push(byte),
                    // Not hex after all: keep what was written rather than
                    // silently dropping two characters.
                    Err(_) => {
                        out.push(b'\\');
                        out.push(b'x');
                        out.extend_from_slice(hex.as_bytes());
                    }
                }
            }
            Some(other) => {
                out.push(b'\\');
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

fn wire_edge(edge: Edge) -> haven_proto::Edge {
    match edge {
        Edge::Left => haven_proto::Edge::Left,
        Edge::Right => haven_proto::Edge::Right,
        Edge::Up => haven_proto::Edge::Up,
        Edge::Down => haven_proto::Edge::Down,
    }
}

/// What a palette entry should run for a command name.
///
/// Some commands are nothing without an argument — `select-window` needs a
/// number, `rename-window` with no name *clears* the name — so picking the
/// bare verb from a list would do nothing or the wrong thing. Those open the
/// command line with the verb already typed instead. The test is the parser
/// itself rather than a hand-kept list, so a new command cannot be forgotten.
pub fn palette_entry(name: &str) -> String {
    let wants_more = match parse_command(name) {
        Err(_) => true,
        Ok(action) => matches!(
            action,
            Action::Rename { .. }
                | Action::FindWindow(_)
                | Action::SendKeys { .. }
                | Action::SwitchTable(_)
                | Action::EnterCommand(_)
        ),
    };
    if wants_more {
        format!("command-prompt {name} ")
    } else {
        name.to_string()
    }
}

/// Everything after the verb, or `None` when there is nothing.
fn optional(line: &str, verb: &str) -> Option<String> {
    let rest = rest_of(line, verb);
    (!rest.is_empty()).then_some(rest)
}

/// What `command-prompt` should put on the line, spaces and all.
///
/// Only the leading space that separates it from the verb is dropped; a
/// trailing one belongs to the caller, who put it there so the cursor would
/// land after the verb rather than inside it.
fn prefill(line: &str, verb: &str) -> String {
    line.trim_start()
        .strip_prefix(verb)
        .map(|rest| rest.strip_prefix(' ').unwrap_or(rest).to_string())
        .unwrap_or_default()
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
        "split-h" | "splitw" | "sp" => Action::SplitRows { program: optional(line, verb) },
        "split-v" | "vsplit" | "vs" => Action::SplitColumns { program: optional(line, verb) },
        // tmux's names, plus the ones someone who calls them tabs will try.
        "new-window" | "neww" | "new-tab" => Action::NewWindow { program: optional(line, verb) },
        "choose-shell" | "shells" => Action::ChooseShell(match optional(line, verb) {
            Some(verb) => verb,
            None => "new-window".to_string(),
        }),
        "container-shell" => {
            let mut words = line.split_whitespace().skip(1);
            let how = words.next().ok_or_else(|| "container-shell needs a command".to_string())?;
            let spec = words.next().ok_or_else(|| "container-shell needs a spec".to_string())?;
            Action::ContainerShell { verb: how.to_string(), spec: spec.to_string() }
        }
        "container-mount" | "mount" => Action::ContainerMounts {
            add: Some(optional(line, verb).ok_or_else(|| "mount needs a directory".to_string())?),
            remove: None,
        },
        "container-unmount" | "unmount" => Action::ContainerMounts {
            add: None,
            remove: Some(
                optional(line, verb).ok_or_else(|| "unmount needs a directory".to_string())?,
            ),
        },
        "container-mounts" | "mounts" => Action::ContainerMounts { add: None, remove: None },
        "palette" | "command-palette" => Action::CommandPalette,
        "switch-table" | "table" => Action::SwitchTable(rest_of(line, verb)),
        "save-sessions" | "save" => Action::SaveSessions,
        "restore-sessions" | "restore" => Action::RestoreSessions,
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
        "respawn-pane" | "respawnp" => Action::RespawnPane { program: optional(line, verb) },
        "send-keys" | "send" => Action::SendKeys {
            pane: None,
            // `\n`, `\t`, `\e` and `\xNN` are decoded, so a script can send
            // a newline without having to put a literal one in its argument.
            bytes: unescape(&rest_of(line, verb)),
        },
        "detach-others" | "detach-clients" => Action::DetachOthers,
        "display-panes" | "displayp" => Action::DisplayPanes,
        "find-window" | "findw" => Action::FindWindow(rest_of(line, verb)),
        "synchronize-panes" | "sync" => Action::SynchronizePanes {
            on: match arg {
                Some("on") => Some(true),
                Some("off") => Some(false),
                _ => None,
            },
        },
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
        // Not `rest_of`: the trailing space is the point. `command-prompt
        // rename-window ` means "open the line with the verb typed and the
        // cursor after it", and trimming it left you typing the name onto
        // the end of the verb.
        "command-prompt" => Action::EnterCommand(prefill(line, verb)),
        "copy-mode" | "copy" => Action::Copy,
        "history" | "command-history" => Action::History,
        "grep" | "search-all" => Action::Grep(
            optional(line, verb).ok_or_else(|| "grep needs something to look for".to_string())?,
        ),
        "goto" => {
            let mut nums = line.split_whitespace().skip(1).map(str::parse::<u32>);
            let (Some(Ok(window)), Some(Ok(pane)), Some(Ok(row))) =
                (nums.next(), nums.next(), nums.next())
            else {
                return Err("goto takes a window, a pane and a row".to_string());
            };
            Action::GoTo { window: window as u16, pane, row }
        }
        "font-bigger" | "zoom-in" => Action::Font(haven_client::Zoom::In),
        "font-smaller" | "zoom-out" => Action::Font(haven_client::Zoom::Out),
        "font-reset" | "zoom-reset" => Action::Font(haven_client::Zoom::Reset),
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
        Action::SplitColumns { program } => P::SplitColumns { program },
        Action::SplitRows { program } => P::SplitRows { program },
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
        Action::NewWindow { program } => P::NewWindow { program },
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
        Action::RespawnPane { program } => P::RespawnPane { program },
        Action::SendKeys { pane, bytes } => P::SendKeys { pane, bytes },
        Action::SynchronizePanes { on } => P::SynchronizePanes { on },
        Action::DetachOthers => P::DetachOthers,
        // Both are the client's: one draws an overlay, the other searches a
        // listing the client already holds.
        Action::DisplayPanes
        | Action::FindWindow(_)
        | Action::ChooseShell(_)
        | Action::ContainerShell { .. }
        | Action::ContainerMounts { .. }
        | Action::Font(_)
        | Action::History
        | Action::Grep(_)
        | Action::GoTo { .. }
        | Action::CommandPalette
        | Action::SwitchTable(_) => return None,
        Action::NextLayout => P::NextLayout,
        Action::SaveSessions => P::SaveSessions,
        Action::RestoreSessions => P::RestoreSessions,
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

/// The direction a split action implies, and what it should run.
pub fn split_direction(action: &Action) -> Option<(Direction, Option<String>)> {
    match action {
        Action::SplitColumns { program } => Some((Direction::Columns, program.clone())),
        Action::SplitRows { program } => Some((Direction::Rows, program.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use haven_client::bindings::{Bindings, PREFIX};
    use haven_client::input::{KeyEvent, Modifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::char(c)
    }

    /// Resolve a key through the default table exactly as the client does.
    fn bound(event: &KeyEvent) -> Option<Action> {
        Bindings::default().command(PREFIX, event).and_then(|c| parse_command(c).ok())
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
        assert_eq!(bound(&key('%')), Some(Action::SplitColumns { program: None }));
        assert_eq!(bound(&key('"')), Some(Action::SplitRows { program: None }));
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
    fn send_keys_decodes_the_escapes_a_script_would_write() {
        // A script cannot put a literal newline in an argument, so the
        // escapes are the whole point of the command.
        assert_eq!(unescape("ls\\n"), b"ls\n".to_vec());
        assert_eq!(unescape("\\t\\r\\e"), vec![b'\t', b'\r', 0x1b]);
        assert_eq!(unescape("\\x03"), vec![3]);
        assert_eq!(unescape("a\\\\b"), b"a\\b".to_vec());
    }

    #[test]
    fn a_bad_escape_keeps_what_was_written() {
        // Dropping characters silently is how a script sends the wrong thing
        // and nobody finds out.
        assert_eq!(unescape("\\xzz"), b"\\xzz".to_vec());
        assert_eq!(unescape("\\q"), b"\\q".to_vec());
        assert_eq!(unescape("trailing\\"), b"trailing\\".to_vec());
    }

    #[test]
    fn send_keys_keeps_its_whole_argument() {
        match parse_command("send-keys echo hello world\\n") {
            Ok(Action::SendKeys { bytes, .. }) => {
                assert_eq!(bytes, b"echo hello world\n".to_vec());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn synchronize_panes_takes_on_off_or_a_toggle() {
        assert_eq!(
            parse_command("synchronize-panes on"),
            Ok(Action::SynchronizePanes { on: Some(true) })
        );
        assert_eq!(
            parse_command("synchronize-panes off"),
            Ok(Action::SynchronizePanes { on: Some(false) })
        );
        assert_eq!(parse_command("sync"), Ok(Action::SynchronizePanes { on: None }));
    }

    #[test]
    fn every_mux_action_has_a_wire_form() {
        // Anything a binding can do must be expressible to the daemon, or the
        // same keymap would behave differently attached and standalone.
        for action in [
            Action::SplitColumns { program: None },
            Action::SplitRows { program: None },
            Action::Focus(Edge::Left),
            Action::Resize(Edge::Down),
            Action::ClosePane,
            Action::ToggleZoom,
            Action::NewWindow { program: None },
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
        assert_eq!(
            split_direction(&Action::SplitColumns { program: None }),
            Some((Direction::Columns, None))
        );
        assert_eq!(
            split_direction(&Action::SplitRows { program: Some("fish".into()) }),
            Some((Direction::Rows, Some("fish".into()))),
            "the program a split was asked to run must survive"
        );
        assert_eq!(split_direction(&Action::ToggleZoom), None);
    }

    #[test]
    fn a_split_can_name_what_it_runs() {
        assert_eq!(
            parse_command("split-v /usr/bin/fish"),
            Ok(Action::SplitColumns { program: Some("/usr/bin/fish".into()) })
        );
        assert_eq!(parse_command("split-v"), Ok(Action::SplitColumns { program: None }));
        assert_eq!(
            parse_command("new-window /bin/sh"),
            Ok(Action::NewWindow { program: Some("/bin/sh".into()) })
        );
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
            // Two take a number, two take a directory, and the rest stand
            // alone. The ones that need an argument are offered through
            // `command-prompt`, which is what `palette_entry` is for.
            let line = match *name {
                "select-window" | "join-pane" => format!("{name} 1"),
                "container-mount" | "container-unmount" => format!("{name} /src"),
                "grep" => format!("{name} needle"),
                other => other.to_string(),
            };
            assert!(parse_command(&line).is_ok(), "{name} is offered but does not parse");
        }
    }
}
