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
    /// Movement with nothing held.
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MouseEvent {
    pub button: MouseButton,
    /// Cell coordinates, relative to whatever surface produced the event.
    pub col: u16,
    pub row: u16,
    pub pressed: bool,
    /// The pointer moved rather than a button changing state. A program only
    /// hears about these if it asked for them (1002 or 1003), so the flag has
    /// to survive as far as the encoder.
    pub motion: bool,
    pub mods: Modifiers,
}

impl MouseEvent {
    pub fn press(button: MouseButton, col: u16, row: u16) -> Self {
        Self { button, col, row, pressed: true, motion: false, mods: Modifiers::empty() }
    }

    pub fn with(mut self, mods: Modifiers) -> Self {
        self.mods = mods;
        self
    }

    /// The button's number as the protocol counts them, before modifiers and
    /// the motion bit are folded in.
    fn code(&self) -> u16 {
        match self.button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::None => 3,
        }
    }

    /// The bytes a pane expects for this event, or empty when the pane has
    /// not asked for mouse reporting at all.
    ///
    /// Coordinates are one-based and relative to the pane, so the caller
    /// subtracts the pane's origin before calling.
    pub fn encode(&self, mode: haven_proto::MouseMode) -> Vec<u8> {
        use haven_proto::MouseTracking;
        if mode.tracking == MouseTracking::Off {
            return Vec::new();
        }
        // A wheel notch is a press with no release, and it is reported at
        // every tracking level.
        let wheel = matches!(self.button, MouseButton::WheelUp | MouseButton::WheelDown);
        if self.motion && !wheel {
            match mode.tracking {
                MouseTracking::Off | MouseTracking::Click => return Vec::new(),
                // 1002 reports motion only while a button is down.
                MouseTracking::ButtonEvent if self.button == MouseButton::None => {
                    return Vec::new()
                }
                _ => {}
            }
        }

        let mut code = self.code();
        if self.mods.contains(Modifiers::SHIFT) {
            code += 4;
        }
        if self.mods.contains(Modifiers::ALT) {
            code += 8;
        }
        if self.mods.contains(Modifiers::CTRL) {
            code += 16;
        }
        if self.motion {
            code += 32;
        }
        let (col, row) = (self.col + 1, self.row + 1);

        if mode.sgr {
            let final_byte = if self.pressed || wheel { 'M' } else { 'm' };
            return format!("\x1b[<{code};{col};{row}{final_byte}").into_bytes();
        }
        // The original encoding has no room for a release to say which button
        // it was, and no room for a coordinate past 223.
        let code = if self.pressed || wheel { code } else { 3 };
        if col > 223 || row > 223 {
            return Vec::new();
        }
        vec![0x1b, b'[', b'M', 32 + code as u8, 32 + col as u8, 32 + row as u8]
    }
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
mod mouse_tests {
    use super::*;
    use haven_proto::{MouseMode, MouseTracking};

    fn mode(tracking: MouseTracking, sgr: bool) -> MouseMode {
        MouseMode { tracking, sgr }
    }

    #[test]
    fn a_pane_that_did_not_ask_hears_nothing() {
        let event = MouseEvent::press(MouseButton::Left, 3, 4);
        assert!(event.encode(mode(MouseTracking::Off, true)).is_empty());
    }

    #[test]
    fn sgr_reports_press_and_release_distinctly() {
        let press = MouseEvent::press(MouseButton::Left, 3, 4);
        assert_eq!(press.encode(mode(MouseTracking::Click, true)), b"\x1b[<0;4;5M".to_vec());
        let release = MouseEvent { pressed: false, ..press };
        // Lowercase `m`, and the button survives — which is the whole reason
        // SGR exists.
        assert_eq!(release.encode(mode(MouseTracking::Click, true)), b"\x1b[<0;4;5m".to_vec());
    }

    #[test]
    fn the_legacy_encoding_loses_the_button_on_release() {
        let press = MouseEvent::press(MouseButton::Right, 0, 0);
        assert_eq!(
            press.encode(mode(MouseTracking::Click, false)),
            vec![0x1b, b'[', b'M', 34, 33, 33]
        );
        let release = MouseEvent { pressed: false, ..press };
        // 3 is "some button came up"; the original encoding cannot say which.
        assert_eq!(
            release.encode(mode(MouseTracking::Click, false)),
            vec![0x1b, b'[', b'M', 35, 33, 33]
        );
    }

    #[test]
    fn the_legacy_encoding_refuses_a_coordinate_it_cannot_express() {
        // Past 223 the byte would wrap and point at the wrong cell, which is
        // worse than reporting nothing.
        let far = MouseEvent::press(MouseButton::Left, 300, 0);
        assert!(far.encode(mode(MouseTracking::Click, false)).is_empty());
        assert!(!far.encode(mode(MouseTracking::Click, true)).is_empty(), "SGR has no such limit");
    }

    #[test]
    fn modifiers_fold_into_the_button_byte() {
        let event = MouseEvent::press(MouseButton::Left, 0, 0).with(Modifiers::CTRL);
        assert_eq!(event.encode(mode(MouseTracking::Click, true)), b"\x1b[<16;1;1M".to_vec());
        let event = MouseEvent::press(MouseButton::Left, 0, 0).with(Modifiers::SHIFT);
        assert_eq!(event.encode(mode(MouseTracking::Click, true)), b"\x1b[<4;1;1M".to_vec());
    }

    #[test]
    fn motion_is_reported_only_to_a_pane_that_asked_for_it() {
        let drag = MouseEvent { motion: true, ..MouseEvent::press(MouseButton::Left, 0, 0) };
        assert!(drag.encode(mode(MouseTracking::Click, true)).is_empty(), "1000 is clicks only");
        assert!(!drag.encode(mode(MouseTracking::ButtonEvent, true)).is_empty());

        // 1002 is motion *while a button is held*; a bare hover is not that.
        let hover = MouseEvent {
            button: MouseButton::None,
            pressed: false,
            motion: true,
            ..MouseEvent::press(MouseButton::Left, 0, 0)
        };
        assert!(hover.encode(mode(MouseTracking::ButtonEvent, true)).is_empty());
        assert!(!hover.encode(mode(MouseTracking::AnyEvent, true)).is_empty());
    }

    #[test]
    fn the_wheel_reports_at_every_level_and_never_releases() {
        let wheel = MouseEvent::press(MouseButton::WheelUp, 0, 0);
        assert_eq!(wheel.encode(mode(MouseTracking::Click, true)), b"\x1b[<64;1;1M".to_vec());
        // A notch has no up stroke, so even one marked as a release is `M`.
        let odd = MouseEvent { pressed: false, ..wheel };
        assert_eq!(odd.encode(mode(MouseTracking::Click, true)), b"\x1b[<64;1;1M".to_vec());
    }
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
