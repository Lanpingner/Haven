//! Input events and key encoding. See `docs/PLAN.md` §11.
//!
//! Encoding lives client-side because only the client knows what its input
//! source actually delivered: a GPU window has every keycode, a TUI client has
//! whatever the host terminal chose to send. The [`KeyTier`] it reports is
//! therefore an input to encoding, not a cosmetic capability flag.

use haven_proto::{CellSize, KeyTier};

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
    pub struct Modifiers: u8 {
        const SHIFT = 1 << 0;
        const ALT   = 1 << 1;
        const CTRL  = 1 << 2;
        const SUPER = 1 << 3;
    }
}

impl Modifiers {
    /// The kitty/xterm modifier parameter: 1 + bitfield.
    fn csi_param(self) -> u8 {
        let mut n = 0;
        if self.contains(Self::SHIFT) {
            n |= 1;
        }
        if self.contains(Self::ALT) {
            n |= 2;
        }
        if self.contains(Self::CTRL) {
            n |= 4;
        }
        if self.contains(Self::SUPER) {
            n |= 8;
        }
        n + 1
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyState {
    Press,
    Repeat,
    /// Only ever delivered at [`KeyTier::Full`].
    Release,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NamedKey {
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Function(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Named(NamedKey),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Modifiers,
    pub state: KeyState,
}

impl KeyEvent {
    pub fn char(c: char) -> Self {
        Self { key: Key::Char(c), mods: Modifiers::empty(), state: KeyState::Press }
    }

    pub fn named(key: NamedKey) -> Self {
        Self { key: Key::Named(key), mods: Modifiers::empty(), state: KeyState::Press }
    }

    pub fn with(mut self, mods: Modifiers) -> Self {
        self.mods = mods;
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MouseEvent {
    pub button: MouseButton,
    pub col: u16,
    pub row: u16,
    pub pressed: bool,
    pub mods: Modifiers,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum InputEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize(CellSize),
    Focus(bool),
    Quit,
}

/// Turns key events into bytes for a pane.
///
/// `app_cursor` mirrors the pane's DECCKM, which lives in the *terminal's*
/// state, not ours — the daemon reports it, and until then arrows use the
/// normal form. Mirroring it rather than guessing is the difference between
/// arrow keys working in vim and inserting letters.
#[derive(Clone, Copy, Debug)]
pub struct KeyEncoder {
    pub tier: KeyTier,
    pub app_cursor: bool,
}

impl KeyEncoder {
    pub fn new(tier: KeyTier) -> Self {
        Self { tier, app_cursor: false }
    }

    pub fn encode(&self, event: &KeyEvent) -> Vec<u8> {
        // Release events exist only at Full, and no legacy app wants them.
        if event.state == KeyState::Release {
            return Vec::new();
        }
        match event.key {
            Key::Char(c) => self.encode_char(c, event.mods),
            Key::Named(key) => self.encode_named(key, event.mods),
        }
    }

    fn encode_char(&self, c: char, mods: Modifiers) -> Vec<u8> {
        let ctrl = mods.contains(Modifiers::CTRL);
        let alt = mods.contains(Modifiers::ALT);
        let shift = mods.contains(Modifiers::SHIFT);

        // Combinations the legacy encoding simply cannot express. At Full we
        // disambiguate with CSI u; below that the binding is unavailable, which
        // is what the tier system exists to make visible rather than mysterious.
        let needs_csi_u = ctrl && shift;
        if needs_csi_u && self.tier == KeyTier::Full {
            return csi_u(c as u32, mods);
        }
        if needs_csi_u && self.tier == KeyTier::Extended {
            // modifyOtherKeys=2 form.
            return format!("\x1b[27;{};{}~", mods.csi_param(), c as u32).into_bytes();
        }

        let mut out = Vec::new();
        if alt {
            out.push(0x1b);
        }
        if ctrl {
            // The classic collapse: ctrl+i is byte 0x09, indistinguishable from
            // Tab at Legacy. At Full the caller gets the unambiguous form.
            if self.tier == KeyTier::Full && matches!(c, 'i' | 'm' | '[') {
                return csi_u(c as u32, mods);
            }
            let upper = c.to_ascii_uppercase();
            if upper.is_ascii() && (b'@'..=b'_').contains(&(upper as u8)) {
                out.push(upper as u8 & 0x1f);
                return out;
            }
            if c == '?' {
                out.push(0x7f);
                return out;
            }
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        let _ = shift;
        out
    }

    fn encode_named(&self, key: NamedKey, mods: Modifiers) -> Vec<u8> {
        let plain = mods.is_empty();
        let param = mods.csi_param();

        let csi = |tail: &str| -> Vec<u8> {
            if plain {
                format!("\x1b[{tail}").into_bytes()
            } else {
                // CSI 1;<mod><final> for the letter-final keys.
                format!("\x1b[1;{param}{tail}").into_bytes()
            }
        };
        let tilde = |n: u8| -> Vec<u8> {
            if plain {
                format!("\x1b[{n}~").into_bytes()
            } else {
                format!("\x1b[{n};{param}~").into_bytes()
            }
        };

        match key {
            NamedKey::Enter => vec![b'\r'],
            NamedKey::Tab => {
                if mods.contains(Modifiers::SHIFT) {
                    b"\x1b[Z".to_vec()
                } else {
                    vec![b'\t']
                }
            }
            NamedKey::Backspace => {
                if mods.contains(Modifiers::ALT) {
                    vec![0x1b, 0x7f]
                } else {
                    vec![0x7f]
                }
            }
            NamedKey::Escape => vec![0x1b],
            // DECCKM swaps CSI for SS3 on the cursor keys; getting this wrong
            // is why arrows sometimes type letters in an editor.
            NamedKey::Up => self.cursor_key('A', mods),
            NamedKey::Down => self.cursor_key('B', mods),
            NamedKey::Right => self.cursor_key('C', mods),
            NamedKey::Left => self.cursor_key('D', mods),
            NamedKey::Home => csi("H"),
            NamedKey::End => csi("F"),
            NamedKey::Insert => tilde(2),
            NamedKey::Delete => tilde(3),
            NamedKey::PageUp => tilde(5),
            NamedKey::PageDown => tilde(6),
            NamedKey::Function(n) => self.function_key(n, mods),
        }
    }

    fn cursor_key(&self, final_byte: char, mods: Modifiers) -> Vec<u8> {
        if mods.is_empty() {
            if self.app_cursor {
                format!("\x1bO{final_byte}").into_bytes()
            } else {
                format!("\x1b[{final_byte}").into_bytes()
            }
        } else {
            format!("\x1b[1;{}{final_byte}", mods.csi_param()).into_bytes()
        }
    }

    fn function_key(&self, n: u8, mods: Modifiers) -> Vec<u8> {
        let param = mods.csi_param();
        // F1-F4 are SS3-final, F5+ are tilde-final: the split every terminal
        // gets wrong at least once, and the one mc leans on hardest.
        let ss3 = |c: char| {
            if mods.is_empty() {
                format!("\x1bO{c}").into_bytes()
            } else {
                format!("\x1b[1;{param}{c}").into_bytes()
            }
        };
        let tilde = |code: u8| {
            if mods.is_empty() {
                format!("\x1b[{code}~").into_bytes()
            } else {
                format!("\x1b[{code};{param}~").into_bytes()
            }
        };
        match n {
            1 => ss3('P'),
            2 => ss3('Q'),
            3 => ss3('R'),
            4 => ss3('S'),
            5 => tilde(15),
            6 => tilde(17),
            7 => tilde(18),
            8 => tilde(19),
            9 => tilde(20),
            10 => tilde(21),
            11 => tilde(23),
            12 => tilde(24),
            _ => Vec::new(),
        }
    }
}

fn csi_u(code: u32, mods: Modifiers) -> Vec<u8> {
    format!("\x1b[{code};{}u", mods.csi_param()).into_bytes()
}

/// Convenience wrapper used by the client loop.
pub fn encode_key(event: &KeyEvent, tier: KeyTier) -> Vec<u8> {
    KeyEncoder::new(tier).encode(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(tier: KeyTier, event: KeyEvent) -> String {
        String::from_utf8(encode_key(&event, tier)).unwrap().escape_debug().to_string()
    }

    #[test]
    fn plain_characters_pass_through() {
        assert_eq!(enc(KeyTier::Full, KeyEvent::char('a')), "a");
        assert_eq!(enc(KeyTier::Legacy, KeyEvent::char('ä')), "ä");
    }

    #[test]
    fn control_codes_match_the_classic_encoding() {
        let ctrl = Modifiers::CTRL;
        assert_eq!(encode_key(&KeyEvent::char('a').with(ctrl), KeyTier::Legacy), vec![0x01]);
        assert_eq!(encode_key(&KeyEvent::char('c').with(ctrl), KeyTier::Legacy), vec![0x03]);
        assert_eq!(encode_key(&KeyEvent::char('?').with(ctrl), KeyTier::Legacy), vec![0x7f]);
    }

    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(enc(KeyTier::Legacy, KeyEvent::char('x').with(Modifiers::ALT)), "\\u{1b}x");
    }

    #[test]
    fn ctrl_shift_is_unavailable_at_legacy_and_unambiguous_at_full() {
        let event = KeyEvent::char('a').with(Modifiers::CTRL | Modifiers::SHIFT);
        // Legacy collapses it onto plain ctrl+a: the binding is simply gone.
        assert_eq!(encode_key(&event, KeyTier::Legacy), vec![0x01]);
        // Extended uses modifyOtherKeys, Full uses CSI u.
        assert_eq!(enc(KeyTier::Extended, event), "\\u{1b}[27;6;97~");
        assert_eq!(enc(KeyTier::Full, event), "\\u{1b}[97;6u");
    }

    #[test]
    fn tab_and_ctrl_i_are_distinguishable_only_at_full() {
        let tab = KeyEvent::named(NamedKey::Tab);
        let ctrl_i = KeyEvent::char('i').with(Modifiers::CTRL);
        assert_eq!(encode_key(&tab, KeyTier::Legacy), encode_key(&ctrl_i, KeyTier::Legacy));
        assert_ne!(encode_key(&tab, KeyTier::Full), encode_key(&ctrl_i, KeyTier::Full));
        assert_eq!(enc(KeyTier::Full, ctrl_i), "\\u{1b}[105;5u");
    }

    #[test]
    fn cursor_keys_follow_decckm() {
        let mut enc = KeyEncoder::new(KeyTier::Full);
        assert_eq!(enc.encode(&KeyEvent::named(NamedKey::Up)), b"\x1b[A");
        enc.app_cursor = true;
        assert_eq!(enc.encode(&KeyEvent::named(NamedKey::Up)), b"\x1bOA");
        // Modified cursor keys always use the CSI form, app mode or not.
        let shifted = KeyEvent::named(NamedKey::Up).with(Modifiers::SHIFT);
        assert_eq!(enc.encode(&shifted), b"\x1b[1;2A");
    }

    #[test]
    fn function_keys_split_between_ss3_and_tilde_forms() {
        let e = KeyEncoder::new(KeyTier::Full);
        assert_eq!(e.encode(&KeyEvent::named(NamedKey::Function(1))), b"\x1bOP");
        assert_eq!(e.encode(&KeyEvent::named(NamedKey::Function(4))), b"\x1bOS");
        assert_eq!(e.encode(&KeyEvent::named(NamedKey::Function(5))), b"\x1b[15~");
        assert_eq!(e.encode(&KeyEvent::named(NamedKey::Function(10))), b"\x1b[21~");
        // mc's F-keys with modifiers still have to go somewhere sensible.
        let shifted = KeyEvent::named(NamedKey::Function(1)).with(Modifiers::SHIFT);
        assert_eq!(e.encode(&shifted), b"\x1b[1;2P");
    }

    #[test]
    fn shift_tab_is_back_tab() {
        let e = KeyEncoder::new(KeyTier::Legacy);
        assert_eq!(e.encode(&KeyEvent::named(NamedKey::Tab).with(Modifiers::SHIFT)), b"\x1b[Z");
    }

    #[test]
    fn release_events_are_dropped_below_the_protocol_that_wants_them() {
        let mut event = KeyEvent::char('a');
        event.state = KeyState::Release;
        assert!(encode_key(&event, KeyTier::Full).is_empty());
    }
}
