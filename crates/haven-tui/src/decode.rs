//! Decoding what a host terminal sends us back into key events.
//!
//! This is the lossy direction described in `docs/PLAN.md` §11.1: the bytes
//! arriving here have already thrown information away, and the decoder's job
//! is to recover as much as the host was willing to encode — legacy control
//! codes, xterm function keys, `modifyOtherKeys`, kitty CSI-u — without ever
//! blocking on an ambiguous `ESC`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use haven_client::input::{InputEvent, Key, KeyEvent, KeyState, Modifiers, NamedKey};
use haven_proto::KeyTier;

/// Parse an SGR mouse report's parameters: `Cb ; Cx ; Cy`.
///
/// The button byte folds three things together — which button, which
/// modifiers, and whether this is motion — and taking them apart in the wrong
/// order is how a ctrl-drag turns into a middle click.
fn mouse_event(params: &[u8], pressed: bool) -> Option<haven_client::input::MouseEvent> {
    use haven_client::input::{MouseButton, MouseEvent};
    let text = String::from_utf8_lossy(params);
    let mut fields = text.split(';').map(|f| f.parse::<u32>().ok());
    let code = fields.next()??;
    let col = fields.next()??;
    let row = fields.next()??;

    let mut mods = Modifiers::empty();
    if code & 4 != 0 {
        mods |= Modifiers::SHIFT;
    }
    if code & 8 != 0 {
        mods |= Modifiers::ALT;
    }
    if code & 16 != 0 {
        mods |= Modifiers::CTRL;
    }
    let motion = code & 32 != 0;
    let button = match code & 0b1100_0011 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        3 => MouseButton::None,
        64 => MouseButton::WheelUp,
        65 => MouseButton::WheelDown,
        _ => return None,
    };
    Some(MouseEvent {
        button,
        // The wire is one-based and everything above it is not.
        col: col.saturating_sub(1).min(u16::MAX as u32) as u16,
        row: row.saturating_sub(1).min(u16::MAX as u32) as u16,
        pressed,
        motion,
        mods,
    })
}

/// How long to wait for the rest of a sequence before deciding a lone `ESC`
/// was the Escape key. A blind timeout on every Escape is what makes vim feel
/// soggy inside tmux, so a buffered follow-on byte short-circuits it.
pub const DEFAULT_ESC_TIMEOUT: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub struct Decoder {
    buf: VecDeque<u8>,
    /// When the pending `ESC` arrived.
    esc_since: Option<Instant>,
    pub esc_timeout: Duration,
    pub tier: KeyTier,
    /// Inside a bracketed paste.
    paste: Option<String>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            buf: VecDeque::new(),
            esc_since: None,
            esc_timeout: DEFAULT_ESC_TIMEOUT,
            tier: KeyTier::Legacy,
            paste: None,
        }
    }
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Pull the next event, if one can be decided yet.
    pub fn next_event(&mut self) -> Option<InputEvent> {
        loop {
            if self.buf.is_empty() {
                return None;
            }
            // Paste mode swallows everything until the end marker.
            if self.paste.is_some() {
                if let Some(event) = self.consume_paste() {
                    return Some(event);
                }
                return None;
            }
            let first = *self.buf.front()?;
            if first != 0x1b {
                return self.decode_plain();
            }
            match self.decode_escape() {
                Decoded::Event(event) => return Some(event),
                Decoded::NeedMore => {
                    // A lone ESC is only the Escape key once we have waited.
                    let since = *self.esc_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= self.esc_timeout {
                        self.buf.pop_front();
                        self.esc_since = None;
                        return Some(InputEvent::Key(KeyEvent::named(NamedKey::Escape)));
                    }
                    return None;
                }
                Decoded::Ignore => continue,
            }
        }
    }

    fn take(&mut self, n: usize) {
        for _ in 0..n {
            self.buf.pop_front();
        }
        self.esc_since = None;
    }

    fn decode_plain(&mut self) -> Option<InputEvent> {
        let first = *self.buf.front()?;
        let event = match first {
            b'\r' | b'\n' => {
                self.take(1);
                KeyEvent::named(NamedKey::Enter)
            }
            b'\t' => {
                self.take(1);
                KeyEvent::named(NamedKey::Tab)
            }
            0x7f | 0x08 => {
                self.take(1);
                KeyEvent::named(NamedKey::Backspace)
            }
            // Control codes map back onto their letters. At Legacy this is
            // genuinely ambiguous: 0x09 is both Tab and ctrl+i, and Tab wins
            // because that is what the user more likely pressed.
            0x01..=0x1a => {
                self.take(1);
                let letter = (b'a' + (first - 1)) as char;
                KeyEvent::char(letter).with(Modifiers::CTRL)
            }
            0x00 => {
                self.take(1);
                KeyEvent::char(' ').with(Modifiers::CTRL)
            }
            0x1c..=0x1f => {
                self.take(1);
                let letter = (b'4' + (first - 0x1c)) as char;
                KeyEvent::char(letter).with(Modifiers::CTRL)
            }
            _ => return self.decode_utf8(),
        };
        Some(InputEvent::Key(event))
    }

    fn decode_utf8(&mut self) -> Option<InputEvent> {
        let first = *self.buf.front()?;
        let len = utf8_len(first);
        if self.buf.len() < len {
            return None; // A split multi-byte character: wait for the rest.
        }
        let bytes: Vec<u8> = self.buf.iter().take(len).copied().collect();
        match std::str::from_utf8(&bytes) {
            Ok(text) => {
                let c = text.chars().next()?;
                self.take(len);
                Some(InputEvent::Key(KeyEvent::char(c)))
            }
            Err(_) => {
                // Invalid UTF-8: drop one byte rather than stalling forever.
                self.take(1);
                None
            }
        }
    }

    fn consume_paste(&mut self) -> Option<InputEvent> {
        const END: &[u8] = b"\x1b[201~";
        let bytes: Vec<u8> = self.buf.iter().copied().collect();
        let end = bytes.windows(END.len()).position(|w| w == END);
        match end {
            Some(at) => {
                let text = String::from_utf8_lossy(&bytes[..at]).into_owned();
                self.take(at + END.len());
                let mut acc = self.paste.take().unwrap_or_default();
                acc.push_str(&text);
                Some(InputEvent::Paste(acc))
            }
            None => {
                // Keep accumulating; a large paste arrives over many reads.
                if bytes.len() > 4096 {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    self.take(bytes.len());
                    self.paste.get_or_insert_with(String::new).push_str(&text);
                }
                None
            }
        }
    }

    fn decode_escape(&mut self) -> Decoded {
        let bytes: Vec<u8> = self.buf.iter().take(64).copied().collect();
        if bytes.len() < 2 {
            return Decoded::NeedMore;
        }
        match bytes[1] {
            b'[' => self.decode_csi(&bytes),
            b'O' => {
                if bytes.len() < 3 {
                    return Decoded::NeedMore;
                }
                let key = match bytes[2] {
                    b'P' => NamedKey::Function(1),
                    b'Q' => NamedKey::Function(2),
                    b'R' => NamedKey::Function(3),
                    b'S' => NamedKey::Function(4),
                    b'A' => NamedKey::Up,
                    b'B' => NamedKey::Down,
                    b'C' => NamedKey::Right,
                    b'D' => NamedKey::Left,
                    b'H' => NamedKey::Home,
                    b'F' => NamedKey::End,
                    _ => {
                        self.take(3);
                        return Decoded::Ignore;
                    }
                };
                self.take(3);
                Decoded::Event(InputEvent::Key(KeyEvent::named(key)))
            }
            // ESC followed by a normal key is Alt+key.
            _ => {
                self.take(1);
                match self.decode_plain() {
                    Some(InputEvent::Key(mut key)) => {
                        key.mods |= Modifiers::ALT;
                        Decoded::Event(InputEvent::Key(key))
                    }
                    Some(other) => Decoded::Event(other),
                    None => Decoded::NeedMore,
                }
            }
        }
    }

    fn decode_csi(&mut self, bytes: &[u8]) -> Decoded {
        // CSI <params> <final>
        let Some(end) = bytes.iter().position(|b| (0x40..=0x7e).contains(b) && *b != b'[') else {
            return Decoded::NeedMore;
        };
        let params_raw = &bytes[2..end];
        let final_byte = bytes[end];
        let total = end + 1;

        if params_raw.starts_with(b"200") && final_byte == b'~' {
            self.take(total);
            self.paste = Some(String::new());
            return Decoded::Ignore;
        }

        // SGR mouse (1006), which is what `raw.rs` asks the host for.
        if params_raw.first() == Some(&b'<') && matches!(final_byte, b'M' | b'm') {
            self.take(total);
            return match mouse_event(&params_raw[1..], final_byte == b'M') {
                Some(event) => Decoded::Event(InputEvent::Mouse(event)),
                None => Decoded::Ignore,
            };
        }

        let text = String::from_utf8_lossy(params_raw);
        let fields: Vec<&str> = text.split(';').collect();
        let number = |i: usize| -> u32 { fields.get(i).and_then(|f| f.parse().ok()).unwrap_or(0) };
        let mods = modifiers_from_param(number(1));

        let key = match final_byte {
            b'A' => Some(Key::Named(NamedKey::Up)),
            b'B' => Some(Key::Named(NamedKey::Down)),
            b'C' => Some(Key::Named(NamedKey::Right)),
            b'D' => Some(Key::Named(NamedKey::Left)),
            b'H' => Some(Key::Named(NamedKey::Home)),
            b'F' => Some(Key::Named(NamedKey::End)),
            b'Z' => {
                self.take(total);
                return Decoded::Event(InputEvent::Key(
                    KeyEvent::named(NamedKey::Tab).with(Modifiers::SHIFT),
                ));
            }
            b'I' => {
                self.take(total);
                return Decoded::Event(InputEvent::Focus(true));
            }
            b'O' => {
                self.take(total);
                return Decoded::Event(InputEvent::Focus(false));
            }
            // Kitty CSI-u: the unambiguous form. Receiving these is what
            // promotes a host to KeyTier::Full.
            b'u' => {
                let code = number(0);
                char::from_u32(code).map(Key::Char)
            }
            b'~' => match number(0) {
                1 | 7 => Some(Key::Named(NamedKey::Home)),
                2 => Some(Key::Named(NamedKey::Insert)),
                3 => Some(Key::Named(NamedKey::Delete)),
                4 | 8 => Some(Key::Named(NamedKey::End)),
                5 => Some(Key::Named(NamedKey::PageUp)),
                6 => Some(Key::Named(NamedKey::PageDown)),
                11..=15 => Some(Key::Named(NamedKey::Function((number(0) - 10) as u8))),
                17..=21 => Some(Key::Named(NamedKey::Function((number(0) - 11) as u8))),
                23..=26 => Some(Key::Named(NamedKey::Function((number(0) - 12) as u8))),
                // modifyOtherKeys level 2: CSI 27 ; mods ; code ~
                27 => char::from_u32(number(2)).map(Key::Char),
                _ => None,
            },
            _ => None,
        };

        self.take(total);
        match key {
            Some(key) => {
                let mods = if final_byte == b'~' && number(0) == 27 {
                    modifiers_from_param(number(1))
                } else {
                    mods
                };
                Decoded::Event(InputEvent::Key(KeyEvent { key, mods, state: KeyState::Press }))
            }
            None => Decoded::Ignore,
        }
    }
}

enum Decoded {
    Event(InputEvent),
    NeedMore,
    /// A sequence we understood but do not act on; consumed.
    Ignore,
}

fn modifiers_from_param(param: u32) -> Modifiers {
    if param == 0 {
        return Modifiers::empty();
    }
    let bits = param.saturating_sub(1);
    let mut mods = Modifiers::empty();
    if bits & 1 != 0 {
        mods |= Modifiers::SHIFT;
    }
    if bits & 2 != 0 {
        mods |= Modifiers::ALT;
    }
    if bits & 4 != 0 {
        mods |= Modifiers::CTRL;
    }
    if bits & 8 != 0 {
        mods |= Modifiers::SUPER;
    }
    mods
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod mouse_tests {
    use super::*;
    use haven_client::input::MouseButton;

    fn decode(bytes: &[u8]) -> Vec<InputEvent> {
        let mut d = Decoder::default();
        d.push(bytes);
        let mut out = Vec::new();
        while let Some(event) = d.next_event() {
            out.push(event);
        }
        out
    }

    fn mouse(bytes: &[u8]) -> haven_client::input::MouseEvent {
        match decode(bytes).into_iter().next() {
            Some(InputEvent::Mouse(m)) => m,
            other => panic!("not a mouse event: {other:?}"),
        }
    }

    #[test]
    fn an_sgr_press_decodes_to_zero_based_cells() {
        let m = mouse(b"\x1b[<0;5;3M");
        assert_eq!((m.col, m.row), (4, 2), "the wire is one-based and we are not");
        assert_eq!(m.button, MouseButton::Left);
        assert!(m.pressed);
        assert!(!m.motion);
    }

    #[test]
    fn a_release_is_the_lowercase_final_byte() {
        let m = mouse(b"\x1b[<2;1;1m");
        assert_eq!(m.button, MouseButton::Right);
        assert!(!m.pressed);
    }

    #[test]
    fn the_button_byte_is_taken_apart_in_the_right_order() {
        // Ctrl + drag: 0 (left) + 16 (ctrl) + 32 (motion) = 48. Masking in
        // the wrong order reads this as a middle click.
        let m = mouse(b"\x1b[<48;1;1M");
        assert_eq!(m.button, MouseButton::Left);
        assert!(m.motion);
        assert!(m.mods.contains(Modifiers::CTRL));
    }

    #[test]
    fn the_wheel_decodes() {
        assert_eq!(mouse(b"\x1b[<64;1;1M").button, MouseButton::WheelUp);
        assert_eq!(mouse(b"\x1b[<65;1;1M").button, MouseButton::WheelDown);
    }

    #[test]
    fn a_bare_hover_has_no_button() {
        // 3 + 32: motion with nothing held, which 1003 reports.
        let m = mouse(b"\x1b[<35;1;1M");
        assert_eq!(m.button, MouseButton::None);
        assert!(m.motion);
    }

    #[test]
    fn a_mouse_report_is_never_mistaken_for_keys() {
        // Before this was parsed, these bytes fell through to the key path
        // and typed rubbish into the pane.
        let events = decode(b"\x1b[<0;5;3M");
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], InputEvent::Mouse(_)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<InputEvent> {
        let mut d = Decoder::default();
        d.push(bytes);
        let mut out = Vec::new();
        while let Some(event) = d.next_event() {
            out.push(event);
        }
        out
    }

    fn keys(bytes: &[u8]) -> Vec<KeyEvent> {
        decode(bytes)
            .into_iter()
            .filter_map(|e| match e {
                InputEvent::Key(k) => Some(k),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn plain_text_decodes_to_characters() {
        assert_eq!(keys(b"hi"), vec![KeyEvent::char('h'), KeyEvent::char('i')]);
    }

    #[test]
    fn multibyte_characters_survive_a_split_read() {
        let mut d = Decoder::default();
        d.push(&[0xe6]); // first byte of 漢
        assert!(d.next_event().is_none(), "must wait rather than emit garbage");
        d.push(&[0xbc, 0xa2]);
        assert_eq!(d.next_event(), Some(InputEvent::Key(KeyEvent::char('漢'))));
    }

    #[test]
    fn control_codes_become_ctrl_keys() {
        assert_eq!(keys(&[0x03]), vec![KeyEvent::char('c').with(Modifiers::CTRL)]);
        // Ctrl+O, which is how mc toggles its subshell.
        assert_eq!(keys(&[0x0f]), vec![KeyEvent::char('o').with(Modifiers::CTRL)]);
    }

    #[test]
    fn tab_and_enter_are_named_keys_not_control_letters() {
        assert_eq!(keys(b"\t"), vec![KeyEvent::named(NamedKey::Tab)]);
        assert_eq!(keys(b"\r"), vec![KeyEvent::named(NamedKey::Enter)]);
        assert_eq!(keys(&[0x7f]), vec![KeyEvent::named(NamedKey::Backspace)]);
    }

    #[test]
    fn arrows_decode_in_both_normal_and_application_forms() {
        assert_eq!(keys(b"\x1b[A"), vec![KeyEvent::named(NamedKey::Up)]);
        assert_eq!(keys(b"\x1bOA"), vec![KeyEvent::named(NamedKey::Up)]);
    }

    #[test]
    fn modified_arrows_carry_their_modifiers() {
        assert_eq!(
            keys(b"\x1b[1;5C"),
            vec![KeyEvent::named(NamedKey::Right).with(Modifiers::CTRL)]
        );
        assert_eq!(keys(b"\x1b[1;2A"), vec![KeyEvent::named(NamedKey::Up).with(Modifiers::SHIFT)]);
    }

    #[test]
    fn function_keys_decode_from_both_encodings() {
        assert_eq!(keys(b"\x1bOP"), vec![KeyEvent::named(NamedKey::Function(1))]);
        assert_eq!(keys(b"\x1b[15~"), vec![KeyEvent::named(NamedKey::Function(5))]);
        assert_eq!(keys(b"\x1b[24~"), vec![KeyEvent::named(NamedKey::Function(12))]);
    }

    #[test]
    fn kitty_csi_u_recovers_what_legacy_cannot_express() {
        // ctrl+shift+a, which has no legacy encoding at all.
        assert_eq!(
            keys(b"\x1b[97;6u"),
            vec![KeyEvent::char('a').with(Modifiers::CTRL | Modifiers::SHIFT)]
        );
    }

    #[test]
    fn modify_other_keys_is_understood_too() {
        assert_eq!(
            keys(b"\x1b[27;6;97~"),
            vec![KeyEvent::char('a').with(Modifiers::CTRL | Modifiers::SHIFT)]
        );
    }

    #[test]
    fn alt_prefixed_keys_are_recognised() {
        assert_eq!(keys(b"\x1bx"), vec![KeyEvent::char('x').with(Modifiers::ALT)]);
    }

    #[test]
    fn a_lone_escape_waits_for_the_timeout_then_decides() {
        let mut d = Decoder { esc_timeout: Duration::from_millis(5), ..Default::default() };
        d.push(b"\x1b");
        assert!(d.next_event().is_none(), "must not guess immediately");
        std::thread::sleep(Duration::from_millis(8));
        assert_eq!(d.next_event(), Some(InputEvent::Key(KeyEvent::named(NamedKey::Escape))));
    }

    #[test]
    fn a_sequence_arriving_whole_never_waits_for_the_escape_timeout() {
        // The short-circuit that keeps arrow keys from feeling laggy.
        let mut d = Decoder { esc_timeout: Duration::from_secs(60), ..Default::default() };
        d.push(b"\x1b[B");
        assert_eq!(d.next_event(), Some(InputEvent::Key(KeyEvent::named(NamedKey::Down))));
    }

    #[test]
    fn bracketed_paste_arrives_as_one_event() {
        assert_eq!(
            decode(b"\x1b[200~hello\nworld\x1b[201~"),
            vec![InputEvent::Paste("hello\nworld".into())]
        );
    }

    #[test]
    fn focus_events_decode() {
        assert_eq!(decode(b"\x1b[I"), vec![InputEvent::Focus(true)]);
        assert_eq!(decode(b"\x1b[O"), vec![InputEvent::Focus(false)]);
    }

    #[test]
    fn an_unknown_sequence_is_consumed_rather_than_jamming_the_stream() {
        // A sequence we do not handle must not block what follows it.
        let events = keys(b"\x1b[>4;2mX");
        assert_eq!(events, vec![KeyEvent::char('X')]);
    }
}
