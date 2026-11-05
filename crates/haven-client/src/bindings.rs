//! Key bindings as data: a chord, and the name of a command to run.
//!
//! The table is *data* for the same reason a theme is. It comes from the
//! daemon's config, so one session behaves the same in a window and in a
//! terminal, and an edit reaches both without reattaching. Nothing here knows
//! what a command means — the composition root parses the string, which is
//! what keeps a bound key and a typed command the same vocabulary rather than
//! two that drift.

use crate::input::{Key, KeyEvent, Modifiers, NamedKey};

/// One key with its modifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    pub key: Key,
    pub mods: Modifiers,
}

impl Chord {
    pub fn new(key: Key, mods: Modifiers) -> Self {
        Self { key, mods }
    }

    pub fn matches(&self, event: &KeyEvent) -> bool {
        if self.key != event.key {
            return false;
        }
        // Shift is already in the character a terminal reports — `%` *is*
        // shift+5 — so requiring it as well would make `%` unbindable.
        let mask = match self.key {
            Key::Char(_) => Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER,
            _ => Modifiers::all(),
        };
        (self.mods & mask) == (event.mods & mask)
    }

    /// Parse `C-a`, `S-Left`, `%`, `PageUp`. Prefixes may be combined and in
    /// any order: `C-S-x` and `S-C-x` are the same chord.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut mods = Modifiers::empty();
        let mut rest = spec;
        loop {
            let (flag, tail) = match rest.split_at_checked(2) {
                Some(("C-", tail)) => (Modifiers::CTRL, tail),
                Some(("S-", tail)) => (Modifiers::SHIFT, tail),
                Some(("A-", tail)) => (Modifiers::ALT, tail),
                Some(("M-", tail)) => (Modifiers::ALT, tail),
                Some(("D-", tail)) => (Modifiers::SUPER, tail),
                // A bare `-` is the key itself, not an empty modifier.
                _ => break,
            };
            if tail.is_empty() {
                break;
            }
            mods |= flag;
            rest = tail;
        }

        let key = match rest {
            "Space" | "space" => Key::Char(' '),
            "Enter" | "Return" => Key::Named(NamedKey::Enter),
            "Escape" | "Esc" => Key::Named(NamedKey::Escape),
            "Tab" => Key::Named(NamedKey::Tab),
            "BSpace" | "Backspace" => Key::Named(NamedKey::Backspace),
            "Delete" | "DC" => Key::Named(NamedKey::Delete),
            "Insert" | "IC" => Key::Named(NamedKey::Insert),
            "Up" => Key::Named(NamedKey::Up),
            "Down" => Key::Named(NamedKey::Down),
            "Left" => Key::Named(NamedKey::Left),
            "Right" => Key::Named(NamedKey::Right),
            "Home" => Key::Named(NamedKey::Home),
            "End" => Key::Named(NamedKey::End),
            "PageUp" | "PPage" => Key::Named(NamedKey::PageUp),
            "PageDown" | "NPage" => Key::Named(NamedKey::PageDown),
            other => {
                if let Some(n) = other.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
                    Key::Named(NamedKey::Function(n))
                } else {
                    let mut chars = other.chars();
                    let c = chars.next()?;
                    // One character, or it is not a key at all: a typo must
                    // be reported, not silently bound to its first letter.
                    if chars.next().is_some() {
                        return None;
                    }
                    Key::Char(c)
                }
            }
        };
        Some(Self { key, mods })
    }
}

/// A chord written the way a config would write it, for telling someone
/// which key they just pressed to no effect.
pub fn label(event: &KeyEvent) -> String {
    let mut out = String::new();
    if event.mods.contains(Modifiers::CTRL) {
        out.push_str("C-");
    }
    if event.mods.contains(Modifiers::ALT) {
        out.push_str("A-");
    }
    if event.mods.contains(Modifiers::SUPER) {
        out.push_str("D-");
    }
    match &event.key {
        Key::Char(' ') => out.push_str("Space"),
        Key::Char(c) => out.push(*c),
        Key::Named(named) => out.push_str(&format!("{named:?}")),
    }
    out
}

/// Named key tables.
///
/// `root` is consulted with nothing armed and `prefix` after the prefix key,
/// which are the two every terminal multiplexer has. Any other name is a
/// table of your own, reached with `switch-table` — which is what makes the
/// key handling modal rather than one-shot: a table can stay armed, so a
/// sequence of keys can mean something without a prefix before each.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bindings {
    pub prefix: Chord,
    /// Keyed by table name. `root` and `prefix` are always present.
    pub tables: std::collections::BTreeMap<String, Vec<(Chord, String)>>,
}

/// The two tables that always exist, by the names a config uses for them.
pub const ROOT: &str = "root";
pub const PREFIX: &str = "prefix";

impl Bindings {
    pub fn command(&self, table: &str, event: &KeyEvent) -> Option<&str> {
        let entries = self.tables.get(table)?;
        // Last wins, so a config's binding overrides the default it repeats
        // without having to unbind it first.
        entries.iter().rev().find(|(chord, _)| chord.matches(event)).map(|(_, cmd)| cmd.as_str())
    }

    /// Whether a table exists, so `switch-table` can refuse a typo instead
    /// of arming a table that will swallow every key and answer none.
    pub fn has(&self, table: &str) -> bool {
        self.tables.contains_key(table)
    }

    /// Whether a table holds the keystroke down until something matches.
    ///
    /// `prefix` is one-shot, as it has always been: a key after it acts and
    /// the table is dropped. A table of your own stays until something
    /// switches away, which is what makes it a mode.
    pub fn sticky(&self, table: &str) -> bool {
        table != PREFIX && table != ROOT
    }

    /// How to press a command, as a person would write it.
    ///
    /// The reverse of what the tables are for, and worth having: a palette
    /// that lists every command without saying which of them already has a
    /// key teaches nobody the keys. Matched on the whole command string, so
    /// `command-prompt rename-window ` finds `,` rather than nothing.
    ///
    /// The shortest first: a key with no prefix is one press, and that is
    /// the one worth showing.
    pub fn keys_for(&self, command: &str) -> Option<String> {
        self.keys_matching(|bound| bound == command)
    }

    /// [`keys_for`](Self::keys_for), for a caller that can tell when two
    /// spellings mean the same thing.
    ///
    /// The tables are written in aliases — `n`, not `next-window`; `%`, not
    /// `split-v` — because that is how a person writes a binding. A palette
    /// comparing strings therefore found a key for about half of what was
    /// bound and silently dropped the rest, which is worse than showing
    /// none: it says `next-window` has no key when `C-a n` is right there.
    /// This crate cannot resolve an alias — it does not know what a command
    /// is, deliberately — so the question goes back to whoever does.
    pub fn keys_matching(&self, same: impl Fn(&str) -> bool) -> Option<String> {
        let label_of = |chord: &Chord| {
            label(&KeyEvent {
                key: chord.key,
                mods: chord.mods,
                state: crate::input::KeyState::Press,
            })
        };
        if let Some((chord, _)) =
            self.tables.get(ROOT).into_iter().flatten().find(|(_, bound)| same(bound))
        {
            return Some(label_of(chord));
        }
        if let Some((chord, _)) =
            self.tables.get(PREFIX).into_iter().flatten().find(|(_, bound)| same(bound))
        {
            return Some(format!("{} {}", label_of(&self.prefix), label_of(chord)));
        }
        // A table of someone's own: name it, since the key alone would not
        // say how to get into the table it lives in.
        self.tables
            .iter()
            .filter(|(name, _)| name.as_str() != ROOT && name.as_str() != PREFIX)
            .find_map(|(name, binds)| {
                binds
                    .iter()
                    .find(|(_, bound)| same(bound))
                    .map(|(chord, _)| format!("{name} {}", label_of(chord)))
            })
    }

    pub fn prefix_table(&self) -> &[(Chord, String)] {
        self.tables.get(PREFIX).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn root(&self) -> &[(Chord, String)] {
        self.tables.get(ROOT).map(Vec::as_slice).unwrap_or_default()
    }

    /// What a key would do in a table, falling back to `root`.
    ///
    /// A root binding means "this acts with no prefix", and an armed table
    /// blocking it is surprising: pressing the prefix and then changing your
    /// mind should not disable `Alt+C`. This is what makes that true.
    pub fn command_or_root(&self, table: &str, event: &KeyEvent) -> Option<&str> {
        self.command(table, event).or_else(|| self.command(ROOT, event))
    }

    pub fn is_prefix(&self, event: &KeyEvent) -> bool {
        self.prefix.matches(event)
    }

    /// Fold a config's entries onto a named table. An empty command unbinds,
    /// and a table that does not exist yet is made.
    pub fn with(mut self, prefix: Option<&str>, table: &str, entries: &[(String, String)]) -> Self {
        if let Some(chord) = prefix.and_then(Chord::parse) {
            self.prefix = chord;
        }
        let into = self.tables.entry(table.to_string()).or_default();
        for (spec, command) in entries {
            let Some(chord) = Chord::parse(spec) else { continue };
            if command.trim().is_empty() {
                into.retain(|(c, _)| *c != chord);
            } else {
                into.push((chord, command.clone()));
            }
        }
        self
    }
}

/// The built-in table. These are the defaults the README documents, written
/// as data so that a configured binding and a built-in one go through exactly
/// the same path — the alternative is a `match` that config can only partly
/// override, which is how bindings end up with two behaviours.
pub const DEFAULT_PREFIX: &str = "C-a";

pub const DEFAULT_BINDINGS: &[(&str, &str)] = &[
    ("%", "split-v"),
    ("\"", "split-h"),
    ("x", "kill-pane"),
    ("z", "zoom"),
    ("c", "new-window"),
    ("C", "new-session"),
    ("n", "next"),
    ("w", "last"),
    (";", "last-pane"),
    (",", "command-prompt rename-window "),
    ("$", "command-prompt rename-session "),
    ("p", "prev"),
    ("d", "detach"),
    ("s", "choose"),
    (":", "command-prompt"),
    ("[", "copy-mode"),
    ("]", "paste-buffer"),
    ("e", "even-horizontal"),
    ("E", "even-vertical"),
    ("t", "tiled"),
    ("Space", "next-layout"),
    ("o", "swap-pane"),
    ("{", "rotate"),
    ("}", "rotate -D"),
    ("!", "break-pane"),
    ("&", "kill-window"),
    ("r", "respawn-pane"),
    ("S", "synchronize-panes"),
    ("q", "display-panes"),
    ("f", "command-prompt find-window "),
    // `/` searches, as it does in less, vim and everything since ed — but
    // over every pane in the session, with the matches as a list.
    ("/", "command-prompt grep "),
    ("D", "detach-others"),
    // tmux-resurrect's keys, because the muscle memory is worth importing.
    ("C-s", "save-sessions"),
    ("C-r", "restore-sessions"),
    // Capital P: `p` is already the previous tab, and last-wins would have
    // quietly taken it away.
    ("P", "command-palette"),
    // Also on the prefix, not only on `Alt+C`. A feature reachable solely
    // through a modifier combination is at the mercy of whatever else on the
    // desktop wants that combination.
    // `g` makes a tab, like `c`; `G` makes a pane, like `%`.
    ("g", "choose-shell"),
    ("G", "choose-shell split-v"),
    ("h", "focus-left"),
    ("j", "focus-down"),
    ("k", "focus-up"),
    ("l", "focus-right"),
    ("H", "resize-left"),
    ("J", "resize-down"),
    ("K", "resize-up"),
    ("L", "resize-right"),
    ("Left", "focus-left"),
    ("Down", "focus-down"),
    ("Up", "focus-up"),
    ("Right", "focus-right"),
    ("S-Left", "resize-left"),
    ("S-Down", "resize-down"),
    ("S-Up", "resize-up"),
    ("S-Right", "resize-right"),
    ("PageUp", "scroll-up"),
    ("PageDown", "scroll-down"),
    ("Home", "scroll-top"),
    ("End", "scroll-end"),
    ("C-a", "send-prefix"),
    ("1", "select-window 1"),
    ("2", "select-window 2"),
    ("3", "select-window 3"),
    ("4", "select-window 4"),
    ("5", "select-window 5"),
    ("6", "select-window 6"),
    ("7", "select-window 7"),
    ("8", "select-window 8"),
    ("9", "select-window 9"),
];

impl Default for Bindings {
    fn default() -> Self {
        let parse = |table: &[(&str, &str)]| -> Vec<(Chord, String)> {
            table
                .iter()
                .filter_map(|(spec, cmd)| Some((Chord::parse(spec)?, (*cmd).to_string())))
                .collect()
        };
        Self {
            prefix: Chord::parse(DEFAULT_PREFIX).expect("the default prefix must parse"),
            tables: [
                (PREFIX.to_string(), parse(DEFAULT_BINDINGS)),
                (ROOT.to_string(), parse(DEFAULT_ROOT)),
            ]
            .into_iter()
            .collect(),
        }
    }
}

/// Bound with no prefix at all. Two by default, both things that are faster
/// than typing the command they stand for.
pub const DEFAULT_ROOT: &[(&str, &str)] = &[
    ("A-c", "choose-shell"),
    ("A-p", "command-palette"),
    // The history, with what each command is beside it. No prefix, because
    // reaching for a command you ran an hour ago should be one key.
    ("A-h", "history"),
    // What every other terminal and every browser uses. `C-+` as well as
    // `C-=`, because a keyboard that needs shift for `+` sends the one and
    // a keyboard with a numpad sends the other.
    ("C-=", "font-bigger"),
    ("C-+", "font-bigger"),
    ("C--", "font-smaller"),
    ("C-0", "font-reset"),
];

#[cfg(test)]
mod tests {

    #[test]
    fn a_command_can_say_which_key_runs_it() {
        // For the palette: a list of every command that says nothing about
        // which of them you could have pressed teaches nobody the keys.
        let b = Bindings::default();
        assert_eq!(b.keys_for("split-v").as_deref(), Some("C-a %"));
        assert_eq!(b.keys_for("new-window").as_deref(), Some("C-a c"));
        // A root binding is one press, and is preferred for being shorter.
        assert_eq!(b.keys_for("choose-shell").as_deref(), Some("A-c"));
        assert_eq!(b.keys_for("history").as_deref(), Some("A-h"));
        // Matched on the whole command, so a prompt-opening binding is
        // found by the string the palette would run.
        assert_eq!(b.keys_for("command-prompt rename-window ").as_deref(), Some("C-a ,"));
        // And a command nobody bound says nothing rather than something.
        assert_eq!(b.keys_for("container-mounts"), None);
    }

    #[test]
    fn a_changed_prefix_changes_what_the_palette_shows() {
        let b = Bindings::default().with(Some("C-b"), PREFIX, &[]);
        assert_eq!(b.keys_for("split-v").as_deref(), Some("C-b %"));
    }

    #[test]
    fn a_key_in_a_table_of_your_own_names_the_table() {
        // A key alone would not say how to get into the table it lives in.
        let b = Bindings::default().with(None, "copy", &[("y".into(), "paste-buffer".into())]);
        assert_eq!(b.keys_for("paste-buffer").as_deref(), Some("C-a ]"));
        // One the defaults leave alone, so the table's own key is the
        // only answer there is.
        let b = Bindings::default().with(None, "copy", &[("y".into(), "container-mounts".into())]);
        assert_eq!(b.keys_for("container-mounts").as_deref(), Some("copy y"));
    }
    use super::*;

    fn event(key: Key, mods: Modifiers) -> KeyEvent {
        KeyEvent { key, mods, state: crate::input::KeyState::Press }
    }

    #[test]
    fn every_default_spec_parses() {
        // A typo here would silently drop a binding the README promises.
        assert!(Chord::parse(DEFAULT_PREFIX).is_some());
        for (spec, _) in DEFAULT_BINDINGS {
            assert!(Chord::parse(spec).is_some(), "{spec:?} does not parse");
        }
        assert_eq!(
            Bindings::default().prefix_table().len(),
            DEFAULT_BINDINGS.len(),
            "a default binding was dropped"
        );
    }

    #[test]
    fn no_default_binding_is_quietly_overridden_by_another() {
        // The table resolves last-wins, so a key listed twice silently loses
        // its first meaning. That is a feature for a config and a bug here.
        let mut seen: Vec<Chord> = Vec::new();
        for (spec, _) in DEFAULT_BINDINGS {
            let chord = Chord::parse(spec).expect("parses");
            assert!(!seen.contains(&chord), "{spec:?} is bound twice in the defaults");
            seen.push(chord);
        }
    }

    #[test]
    fn a_root_binding_survives_an_armed_table() {
        // Pressing the prefix and then changing your mind must not disable
        // the keys that work without one. This cost an evening to find:
        // `Alt+C` after a stray `Ctrl+A` did nothing and said nothing.
        let b = Bindings::default();
        let alt_c = event(Key::Char('c'), Modifiers::ALT);
        assert_eq!(b.command(PREFIX, &alt_c), None, "it is not in the prefix table");
        assert_eq!(b.command_or_root(PREFIX, &alt_c), Some("choose-shell"));
    }

    #[test]
    fn the_armed_table_still_wins_where_both_bind_a_key() {
        let b = Bindings::default().with(None, ROOT, &[("x".into(), "new-window".into())]).with(
            None,
            PREFIX,
            &[("x".into(), "kill-pane".into())],
        );
        assert_eq!(
            b.command_or_root(PREFIX, &event(Key::Char('x'), Modifiers::empty())),
            Some("kill-pane")
        );
    }

    #[test]
    fn a_chord_can_say_its_own_name() {
        assert_eq!(label(&event(Key::Char('c'), Modifiers::ALT)), "A-c");
        assert_eq!(label(&event(Key::Char('a'), Modifiers::CTRL)), "C-a");
        assert_eq!(label(&event(Key::Named(NamedKey::PageUp), Modifiers::empty())), "PageUp");
        assert_eq!(label(&event(Key::Char(' '), Modifiers::empty())), "Space");
    }

    #[test]
    fn modifier_prefixes_parse_in_any_order() {
        let a = Chord::parse("C-S-x").unwrap();
        let b = Chord::parse("S-C-x").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.mods, Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(a.key, Key::Char('x'));
    }

    #[test]
    fn a_bare_hyphen_is_a_key_not_an_empty_modifier() {
        let chord = Chord::parse("-").expect("- should be bindable");
        assert_eq!(chord.key, Key::Char('-'));
        assert_eq!(Chord::parse("C--").unwrap().key, Key::Char('-'));
    }

    #[test]
    fn nonsense_is_refused_rather_than_truncated() {
        // Binding "Pageup" to something must report a typo, not bind `P`.
        assert!(Chord::parse("Pageup").is_none());
        assert!(Chord::parse("").is_none());
    }

    #[test]
    fn shift_is_ignored_for_characters_because_it_is_already_in_them() {
        // A terminal reports `%` with shift held; requiring the modifier as
        // well would make every shifted character unbindable.
        let chord = Chord::parse("%").unwrap();
        assert!(chord.matches(&event(Key::Char('%'), Modifiers::SHIFT)));
        assert!(chord.matches(&event(Key::Char('%'), Modifiers::empty())));
        // Ctrl is not optional, though.
        assert!(!chord.matches(&event(Key::Char('%'), Modifiers::CTRL)));
    }

    #[test]
    fn shift_still_distinguishes_named_keys() {
        let plain = Chord::parse("Left").unwrap();
        let shifted = Chord::parse("S-Left").unwrap();
        assert!(plain.matches(&event(Key::Named(NamedKey::Left), Modifiers::empty())));
        assert!(!plain.matches(&event(Key::Named(NamedKey::Left), Modifiers::SHIFT)));
        assert!(shifted.matches(&event(Key::Named(NamedKey::Left), Modifiers::SHIFT)));
    }

    #[test]
    fn the_defaults_resolve_the_keys_the_readme_documents() {
        let b = Bindings::default();
        assert!(b.is_prefix(&event(Key::Char('a'), Modifiers::CTRL)));
        assert_eq!(b.command(PREFIX, &event(Key::Char('%'), Modifiers::SHIFT)), Some("split-v"));
        assert_eq!(
            b.command(PREFIX, &event(Key::Char('c'), Modifiers::empty())),
            Some("new-window")
        );
        assert_eq!(
            b.command(PREFIX, &event(Key::Char('C'), Modifiers::SHIFT)),
            Some("new-session")
        );
        assert_eq!(
            b.command(PREFIX, &event(Key::Char('4'), Modifiers::empty())),
            Some("select-window 4")
        );
        assert_eq!(b.command(PREFIX, &event(Key::Char('~'), Modifiers::empty())), None);
    }

    #[test]
    fn a_configured_binding_overrides_the_default_it_repeats() {
        let b = Bindings::default().with(None, PREFIX, &[("c".into(), "new-session".into())]);
        assert_eq!(
            b.command(PREFIX, &event(Key::Char('c'), Modifiers::empty())),
            Some("new-session")
        );
    }

    #[test]
    fn an_empty_command_unbinds() {
        let b = Bindings::default().with(None, PREFIX, &[("x".into(), String::new())]);
        assert_eq!(b.command(PREFIX, &event(Key::Char('x'), Modifiers::empty())), None);
    }

    #[test]
    fn the_prefix_itself_can_be_changed() {
        let b = Bindings::default().with(Some("C-b"), PREFIX, &[]);
        assert!(b.is_prefix(&event(Key::Char('b'), Modifiers::CTRL)));
        assert!(!b.is_prefix(&event(Key::Char('a'), Modifiers::CTRL)));
    }

    #[test]
    fn a_root_binding_is_kept_apart_from_the_prefix_table() {
        let b = Bindings::default().with(None, ROOT, &[("C-t".into(), "new-window".into())]);
        let key = event(Key::Char('t'), Modifiers::CTRL);
        assert_eq!(b.command(ROOT, &key), Some("new-window"));
        assert_eq!(b.command(PREFIX, &key), None, "a root binding leaked into the prefix table");
    }
}
