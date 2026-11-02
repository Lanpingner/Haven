//! A minimal key table: the seed of the action registry in `docs/PLAN.md` §11.
//!
//! One prefix table, pushed by a prefix key and popped after one keystroke.
//! The full modal stack, the configurable bindings and the command palette are
//! M4; this exists so the mux is reachable from the keyboard today, and it is
//! deliberately shaped like the thing that replaces it: keys map to a
//! declarative [`Action`], never to inline behaviour.

use haven_client::input::{Key, KeyEvent, Modifiers, NamedKey};
use haven_mux::layout::{Direction, Edge};
use haven_mux::NamedLayout;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    SplitColumns,
    SplitRows,
    Focus(Edge),
    Resize(Edge),
    ClosePane,
    ToggleZoom,
    NewWindow,
    NextWindow,
    PreviousWindow,
    SelectWindow(usize),
    ApplyLayout(NamedLayout),
    Detach,
    /// Send the prefix key itself to the pane.
    SendPrefix,
    /// Open the command line.
    EnterCommand,
}

/// Ctrl+A, like screen and like most tmux configurations in the wild.
pub const PREFIX: (char, Modifiers) = ('a', Modifiers::CTRL);

pub fn is_prefix(event: &KeyEvent) -> bool {
    matches!(event.key, Key::Char(c) if c == PREFIX.0) && event.mods == PREFIX.1
}

/// Resolve a key pressed *after* the prefix.
pub fn prefix_table(event: &KeyEvent) -> Option<Action> {
    let shift = event.mods.contains(Modifiers::SHIFT);
    Some(match event.key {
        Key::Char(':') => Action::EnterCommand,
        Key::Char('%') => Action::SplitColumns,
        Key::Char('"') => Action::SplitRows,
        Key::Char('x') => Action::ClosePane,
        Key::Char('z') => Action::ToggleZoom,
        Key::Char('c') => Action::NewWindow,
        Key::Char('n') => Action::NextWindow,
        Key::Char('p') => Action::PreviousWindow,
        Key::Char('d') => Action::Detach,
        Key::Char('e') => Action::ApplyLayout(NamedLayout::EvenHorizontal),
        Key::Char('E') => Action::ApplyLayout(NamedLayout::EvenVertical),
        Key::Char('t') => Action::ApplyLayout(NamedLayout::Tiled),
        // Uppercase hjkl resizes, lowercase moves focus — the one piece of
        // muscle memory worth importing wholesale.
        Key::Char('H') => Action::Resize(Edge::Left),
        Key::Char('J') => Action::Resize(Edge::Down),
        Key::Char('K') => Action::Resize(Edge::Up),
        Key::Char('L') => Action::Resize(Edge::Right),
        Key::Char('h') => Action::Focus(Edge::Left),
        Key::Char('j') => Action::Focus(Edge::Down),
        Key::Char('k') => Action::Focus(Edge::Up),
        Key::Char('l') => Action::Focus(Edge::Right),
        Key::Char(c) if c.is_ascii_digit() && c != '0' => {
            Action::SelectWindow(c as usize - '1' as usize)
        }
        Key::Char(c) if c == PREFIX.0 && event.mods == PREFIX.1 => Action::SendPrefix,
        Key::Named(NamedKey::Left) if shift => Action::Resize(Edge::Left),
        Key::Named(NamedKey::Right) if shift => Action::Resize(Edge::Right),
        Key::Named(NamedKey::Up) if shift => Action::Resize(Edge::Up),
        Key::Named(NamedKey::Down) if shift => Action::Resize(Edge::Down),
        Key::Named(NamedKey::Left) => Action::Focus(Edge::Left),
        Key::Named(NamedKey::Right) => Action::Focus(Edge::Right),
        Key::Named(NamedKey::Up) => Action::Focus(Edge::Up),
        Key::Named(NamedKey::Down) => Action::Focus(Edge::Down),
        _ => return None,
    })
}

/// Parse a typed command line.
///
/// The names are the long forms of the prefix keys rather than a new
/// vocabulary: anything a binding can do, a typed command can do, because
/// both resolve to the same [`Action`] (`docs/PLAN.md` §11.2).
pub fn parse_command(line: &str) -> Result<Action, String> {
    let mut parts = line.split_whitespace();
    let Some(verb) = parts.next() else {
        return Err(String::new());
    };
    let arg = parts.next();
    Ok(match verb {
        "split-h" | "splitw" | "sp" => Action::SplitRows,
        "split-v" | "vsplit" | "vs" => Action::SplitColumns,
        "new-window" | "neww" => Action::NewWindow,
        "next" | "next-window" => Action::NextWindow,
        "prev" | "previous-window" => Action::PreviousWindow,
        "kill-pane" | "killp" => Action::ClosePane,
        "zoom" => Action::ToggleZoom,
        "detach" | "det" => Action::Detach,
        "even-horizontal" => Action::ApplyLayout(NamedLayout::EvenHorizontal),
        "even-vertical" => Action::ApplyLayout(NamedLayout::EvenVertical),
        "tiled" => Action::ApplyLayout(NamedLayout::Tiled),
        "select-window" | "selectw" => {
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
        Action::ApplyLayout(NamedLayout::EvenHorizontal) => P::EvenHorizontal,
        Action::ApplyLayout(NamedLayout::EvenVertical) => P::EvenVertical,
        Action::ApplyLayout(NamedLayout::Tiled) => P::Tiled,
        Action::Detach | Action::SendPrefix | Action::EnterCommand => return None,
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

    fn key(c: char) -> KeyEvent {
        KeyEvent::char(c)
    }

    #[test]
    fn the_prefix_is_recognised_and_nothing_else_is() {
        assert!(is_prefix(&key('a').with(Modifiers::CTRL)));
        assert!(!is_prefix(&key('a')));
        assert!(!is_prefix(&key('b').with(Modifiers::CTRL)));
    }

    #[test]
    fn prefix_bindings_resolve_to_actions() {
        assert_eq!(prefix_table(&key('%')), Some(Action::SplitColumns));
        assert_eq!(prefix_table(&key('"')), Some(Action::SplitRows));
        assert_eq!(prefix_table(&key('z')), Some(Action::ToggleZoom));
        assert_eq!(prefix_table(&key('l')), Some(Action::Focus(Edge::Right)));
        assert_eq!(prefix_table(&key('L')), Some(Action::Resize(Edge::Right)));
        assert_eq!(prefix_table(&key('3')), Some(Action::SelectWindow(2)));
    }

    #[test]
    fn pressing_the_prefix_twice_sends_it_through() {
        let event = key('a').with(Modifiers::CTRL);
        assert_eq!(prefix_table(&event), Some(Action::SendPrefix));
    }

    #[test]
    fn an_unbound_key_resolves_to_nothing_rather_than_a_default() {
        assert_eq!(prefix_table(&key('Q')), None);
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
        ] {
            assert!(to_proto(action).is_some(), "{action:?} has no wire form");
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
}
