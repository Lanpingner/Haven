//! The VT interpreter: a `vte::Perform` implementation over [`Grid`].
//!
//! See `docs/PLAN.md` §7 (color), §8 (the legacy surface real apps use).

use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

use crate::charset::{Charset, Charsets};
use crate::color::{Color, Palette, Rgb};
use crate::grid::{EraseMode, Grid};
use crate::style::{Attrs, Style, StyleTable, Underline};

/// Reported for `DA1`: VT220 (62), 132 columns (1), selective erase (6),
/// ANSI color (22). Deliberately modest — claiming a capability we do not have
/// sends apps down code paths that then break.
const DA1_REPLY: &[u8] = b"\x1b[?62;1;6;22c";
const DA2_REPLY: &[u8] = b"\x1b[>62;1;0c";
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug)]
pub struct Modes {
    /// DECAWM (7). Mirrored into the grid, which is what acts on it.
    pub autowrap: bool,
    /// DECTCEM (25).
    pub cursor_visible: bool,
    /// DECCKM (1).
    pub app_cursor_keys: bool,
    /// Bracketed paste (2004).
    pub bracketed_paste: bool,
    /// Focus reporting (1004).
    pub focus_events: bool,
    /// Mouse tracking (1000/1002/1003) and SGR encoding (1006).
    pub mouse_report: MouseReport,
    pub mouse_sgr: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseReport {
    Off,
    Click,
    ButtonEvent,
    AnyEvent,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            autowrap: true,
            cursor_visible: true,
            app_cursor_keys: false,
            bracketed_paste: false,
            focus_events: false,
            mouse_report: MouseReport::Off,
            mouse_sgr: false,
        }
    }
}

/// Cursor shapes from `DECSCUSR`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// Terminal state for one pane.
#[derive(Debug)]
pub struct Terminal {
    /// The *active* grid. On the alternate screen this is the alt grid and the
    /// primary is parked in `inactive`.
    pub grid: Grid,
    pub styles: StyleTable,
    pub modes: Modes,
    pub palette: Palette,
    pub title: Option<String>,
    pub bell_count: u32,
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
    /// Set by `OSC 52`; the client owns the actual clipboard.
    pub clipboard_write: Option<String>,
    /// Set by `OSC 7`. The only reliable source for "open a split here".
    pub cwd: Option<String>,
    /// The screen that is not currently shown. `mc`'s `Ctrl+O` toggle depends
    /// on this being preserved intact (`docs/PLAN.md` §8).
    inactive: Option<Grid>,
    alt_active: bool,
    /// Columns with a tab stop. `HTS`/`TBC` edit these.
    tab_stops: Vec<bool>,
    pen: Style,
    charsets: Charsets,
    /// For `REP`.
    last_printed: Option<char>,
    /// Bytes to write back to the pty. Queries that go unanswered do not look
    /// like a missing feature to a shell; they look like a hang
    /// (`docs/PLAN.md` §9.4).
    reply: Vec<u8>,
    dcs: Vec<u8>,
    in_xtgettcap: bool,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            grid: Grid::new(cols, rows),
            styles: StyleTable::default(),
            modes: Modes::default(),
            palette: Palette::default(),
            title: None,
            bell_count: 0,
            cursor_shape: CursorShape::default(),
            cursor_blink: true,
            clipboard_write: None,
            cwd: None,
            inactive: None,
            alt_active: false,
            tab_stops: default_tab_stops(cols),
            pen: Style::default(),
            charsets: Charsets::default(),
            last_printed: None,
            reply: Vec::new(),
            dcs: Vec::new(),
            in_xtgettcap: false,
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.grid.resize(cols, rows);
        // The parked screen must track geometry too, or switching back after a
        // resize shows a differently-shaped screen.
        if let Some(other) = &mut self.inactive {
            other.resize(cols, rows);
        }
        self.tab_stops = default_tab_stops(cols);
    }

    pub fn alt_active(&self) -> bool {
        self.alt_active
    }

    /// The primary screen, whether or not it is currently shown. Scrollback
    /// lives here.
    pub fn primary(&self) -> &Grid {
        if self.alt_active {
            self.inactive.as_ref().unwrap_or(&self.grid)
        } else {
            &self.grid
        }
    }

    fn enter_alt(&mut self, save_cursor: bool) {
        if self.alt_active {
            return;
        }
        if save_cursor {
            self.grid.save_cursor();
        }
        let (cols, rows) = (self.grid.cols(), self.grid.num_rows());
        let mut alt = Grid::new_alt(cols, rows);
        alt.autowrap = self.modes.autowrap;
        std::mem::swap(&mut self.grid, &mut alt);
        self.inactive = Some(alt);
        self.alt_active = true;
    }

    fn leave_alt(&mut self, restore_cursor: bool) {
        if !self.alt_active {
            return;
        }
        if let Some(mut primary) = self.inactive.take() {
            std::mem::swap(&mut self.grid, &mut primary);
            // `primary` now holds the alt screen; dropping it discards it,
            // which is correct: the alt screen is scratch space.
        }
        self.alt_active = false;
        if restore_cursor {
            self.grid.restore_cursor();
        }
    }

    /// Drain pending replies for the caller to write to the pty master.
    pub fn take_reply(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.reply)
    }

    /// Advance to the next tab stop, honouring `HTS`/`TBC` rather than
    /// assuming every eighth column.
    fn forward_tab(&mut self, count: usize) {
        for _ in 0..count.max(1) {
            let start = self.grid.cursor.col;
            let next = (start + 1..self.grid.cols()).find(|&c| self.tab_stops[c]);
            let target = next.unwrap_or(self.grid.cols() - 1);
            let row = self.grid.cursor.row;
            self.grid.move_to(row, target);
            if target == self.grid.cols() - 1 {
                break;
            }
        }
    }

    fn backward_tab(&mut self, count: usize) {
        for _ in 0..count.max(1) {
            let start = self.grid.cursor.col;
            let prev = (0..start).rev().find(|&c| self.tab_stops[c]).unwrap_or(0);
            let row = self.grid.cursor.row;
            self.grid.move_to(row, prev);
            if prev == 0 {
                break;
            }
        }
    }

    /// `DECSTR`: soft reset. Narrower than `RIS` — it must not clear the
    /// screen, which is why apps use it on startup.
    fn soft_reset(&mut self) {
        self.modes = Modes::default();
        self.grid.autowrap = true;
        self.grid.scroll_top = 0;
        self.grid.scroll_bot = self.grid.num_rows() - 1;
        self.charsets.reset();
        self.cursor_shape = CursorShape::default();
        self.tab_stops = default_tab_stops(self.grid.cols());
        self.set_pen(Style::default());
    }

    fn set_pen(&mut self, pen: Style) {
        self.pen = pen;
        self.grid.cursor.style = self.styles.intern(pen);
    }

    fn reply_str(&mut self, s: &str) {
        self.reply.extend_from_slice(s.as_bytes());
    }

    // ---- SGR -------------------------------------------------------------

    /// `docs/PLAN.md` §7.2: both the `;` and the colon sub-parameter forms are
    /// real and apps emit both. A malformed SGR skips the parameter rather than
    /// aborting the rest of the sequence.
    fn sgr(&mut self, params: &Params) {
        let mut pen = self.pen;
        let mut iter = params.iter();
        if params.is_empty() {
            pen = Style::default();
        }
        while let Some(param) = iter.next() {
            match param {
                // Colon form: 38:2::R:G:B, 38:5:N, 4:3, 58:2::R:G:B.
                [n, sub @ ..] if !sub.is_empty() => match *n {
                    4 => pen.underline = Underline::from_sgr(sub[0]),
                    38 => {
                        if let Some(c) = color_from_subparams(sub) {
                            pen.fg = c;
                        }
                    }
                    48 => {
                        if let Some(c) = color_from_subparams(sub) {
                            pen.bg = c;
                        }
                    }
                    58 => {
                        if let Some(c) = color_from_subparams(sub) {
                            pen.underline_color = c;
                        }
                    }
                    _ => {}
                },
                [0] => pen = Style::default(),
                [1] => pen.attrs.insert(Attrs::BOLD),
                [2] => pen.attrs.insert(Attrs::DIM),
                [3] => pen.attrs.insert(Attrs::ITALIC),
                [4] => pen.underline = Underline::Single,
                [5..=6] => pen.attrs.insert(Attrs::BLINK),
                [7] => pen.attrs.insert(Attrs::REVERSE),
                [8] => pen.attrs.insert(Attrs::HIDDEN),
                [9] => pen.attrs.insert(Attrs::STRIKE),
                [21] => pen.underline = Underline::Double,
                [22] => pen.attrs.remove(Attrs::BOLD | Attrs::DIM),
                [23] => pen.attrs.remove(Attrs::ITALIC),
                [24] => pen.underline = Underline::None,
                [25] => pen.attrs.remove(Attrs::BLINK),
                [27] => pen.attrs.remove(Attrs::REVERSE),
                [28] => pen.attrs.remove(Attrs::HIDDEN),
                [29] => pen.attrs.remove(Attrs::STRIKE),
                [53] => pen.attrs.insert(Attrs::OVERLINE),
                [55] => pen.attrs.remove(Attrs::OVERLINE),
                [n @ 30..=37] => pen.fg = Color::Indexed((n - 30) as u8),
                [38] => {
                    if let Some(c) = color_from_iter(&mut iter) {
                        pen.fg = c;
                    }
                }
                [39] => pen.fg = Color::Default,
                [n @ 40..=47] => pen.bg = Color::Indexed((n - 40) as u8),
                [48] => {
                    if let Some(c) = color_from_iter(&mut iter) {
                        pen.bg = c;
                    }
                }
                [49] => pen.bg = Color::Default,
                [58] => {
                    if let Some(c) = color_from_iter(&mut iter) {
                        pen.underline_color = c;
                    }
                }
                [59] => pen.underline_color = Color::Default,
                [n @ 90..=97] => pen.fg = Color::Indexed((n - 90 + 8) as u8),
                [n @ 100..=107] => pen.bg = Color::Indexed((n - 100 + 8) as u8),
                _ => {}
            }
        }
        self.set_pen(pen);
    }

    // ---- modes -----------------------------------------------------------

    fn set_mode(&mut self, private: bool, params: &Params, enable: bool) {
        if !private {
            return;
        }
        for p in params.iter() {
            match p.first().copied().unwrap_or(0) {
                1 => self.modes.app_cursor_keys = enable,
                7 => {
                    self.modes.autowrap = enable;
                    self.grid.autowrap = enable;
                }
                25 => self.modes.cursor_visible = enable,
                1000 => {
                    self.modes.mouse_report =
                        if enable { MouseReport::Click } else { MouseReport::Off }
                }
                1002 => {
                    self.modes.mouse_report =
                        if enable { MouseReport::ButtonEvent } else { MouseReport::Off }
                }
                1003 => {
                    self.modes.mouse_report =
                        if enable { MouseReport::AnyEvent } else { MouseReport::Off }
                }
                1004 => self.modes.focus_events = enable,
                1006 => self.modes.mouse_sgr = enable,
                2004 => self.modes.bracketed_paste = enable,
                // 1049: switch *and* save/restore the cursor. 47/1047 switch
                // only; 1048 saves/restores the cursor only. mc uses 1049 and
                // its Ctrl+O toggle relies on the primary screen surviving.
                1049 => {
                    if enable {
                        self.enter_alt(true);
                        self.grid.erase_in_display(EraseMode::All);
                    } else {
                        self.leave_alt(true);
                    }
                }
                47 | 1047 => {
                    if enable {
                        self.enter_alt(false);
                    } else {
                        self.leave_alt(false);
                    }
                }
                1048 => {
                    if enable {
                        self.grid.save_cursor();
                    } else {
                        self.grid.restore_cursor();
                    }
                }
                _ => {}
            }
        }
    }

    /// `XTGETTCAP` (`DCS + q`). fish queries this at startup; an unanswered
    /// query costs visible latency per prompt.
    fn answer_xtgettcap(&mut self) {
        let payload = std::mem::take(&mut self.dcs);
        for name_hex in payload.split(|&b| b == b';') {
            let Some(name) = hex_decode(name_hex) else { continue };
            let value = match name.as_slice() {
                b"TN" => Some("xterm-256color".to_string()),
                b"Co" | b"colors" => Some("256".to_string()),
                b"RGB" => Some("8/8/8".to_string()),
                _ => None,
            };
            match value {
                Some(v) => {
                    let reply =
                        format!("\x1bP1+r{}={}\x1b\\", hex_encode(&name), hex_encode(v.as_bytes()));
                    self.reply_str(&reply);
                }
                None => self.reply_str("\x1bP0+r\x1b\\"),
            }
        }
    }
}

fn color_from_subparams(sub: &[u16]) -> Option<Color> {
    // 38:2::R:G:B (colorspace id present) and 38:2:R:G:B (absent) both occur.
    match sub.first().copied()? {
        2 => {
            let v = sub[1..].to_vec();
            let rgb = match v.len() {
                3 => (v[0], v[1], v[2]),
                4.. => (v[1], v[2], v[3]),
                _ => return None,
            };
            Some(Color::Rgb(Rgb::new(rgb.0 as u8, rgb.1 as u8, rgb.2 as u8)))
        }
        5 => Some(Color::Indexed(sub.get(1).copied().unwrap_or(0) as u8)),
        _ => None,
    }
}

fn color_from_iter<'a>(iter: &mut impl Iterator<Item = &'a [u16]>) -> Option<Color> {
    match iter.next()?.first().copied()? {
        2 => {
            let r = iter.next()?.first().copied()? as u8;
            let g = iter.next()?.first().copied()? as u8;
            let b = iter.next()?.first().copied()? as u8;
            Some(Color::Rgb(Rgb::new(r, g, b)))
        }
        5 => Some(Color::Indexed(iter.next()?.first().copied()? as u8)),
        _ => None,
    }
}

fn hex_decode(s: &[u8]) -> Option<Vec<u8>> {
    if s.is_empty() || s.len() % 2 != 0 {
        return None;
    }
    let digit = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    s.chunks(2).map(|p| Some(digit(p[0])? << 4 | digit(p[1])?)).collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// xterm's reply format for a colour query. Apps string-match this, so the
/// 16-bit-per-channel form is not optional.
fn xterm_rgb(rgb: Rgb) -> String {
    let scale = |v: u8| (v as u16) * 0x101;
    format!("rgb:{:04x}/{:04x}/{:04x}", scale(rgb.r), scale(rgb.g), scale(rgb.b))
}

/// Accepts `#RGB`, `#RRGGBB`, `rgb:R/G/B` and `rgb:RRRR/GGGG/BBBB`.
fn parse_color_spec(spec: &[u8]) -> Option<Rgb> {
    let text = std::str::from_utf8(spec).ok()?.trim();
    if let Some(hex) = text.strip_prefix('#') {
        let per = match hex.len() {
            3 => 1,
            6 => 2,
            12 => 4,
            _ => return None,
        };
        let channel = |i: usize| -> Option<u8> {
            let raw = &hex[i * per..(i + 1) * per];
            let value = u32::from_str_radix(raw, 16).ok()?;
            // Scale whatever precision was given down to 8 bits.
            Some(match per {
                1 => (value * 17) as u8,
                2 => value as u8,
                _ => (value >> 8) as u8,
            })
        };
        return Some(Rgb::new(channel(0)?, channel(1)?, channel(2)?));
    }
    let body = text.strip_prefix("rgb:")?;
    let mut parts = body.split('/');
    let mut channel = || -> Option<u8> {
        let raw = parts.next()?;
        let value = u32::from_str_radix(raw, 16).ok()?;
        Some(match raw.len() {
            1 => (value * 17) as u8,
            2 => value as u8,
            3 => (value >> 4) as u8,
            _ => (value >> 8) as u8,
        })
    };
    Some(Rgb::new(channel()?, channel()?, channel()?))
}

/// Tab stops every 8 columns, the universal default.
fn default_tab_stops(cols: usize) -> Vec<bool> {
    (0..cols.max(1)).map(|c| c > 0 && c % 8 == 0).collect()
}

/// A parameter where zero is a *meaningful value*, not "absent". `ED`/`EL`
/// need this: mode 0 means erase-to-end, so folding 0 into a default of 1
/// shifts every mode by one and quietly breaks `[1K` and `[2K`.
fn raw_arg(params: &Params, n: usize) -> usize {
    params.iter().nth(n).and_then(|p| p.first().copied()).unwrap_or(0) as usize
}

fn arg(params: &Params, n: usize, default: u16) -> usize {
    let v = params.iter().nth(n).and_then(|p| p.first().copied()).unwrap_or(0);
    if v == 0 {
        default as usize
    } else {
        v as usize
    }
}

impl Perform for Terminal {
    fn print(&mut self, c: char) {
        let width = c.width().unwrap_or(1);
        if width == 0 {
            // Zero-width characters extend the previous cell's cluster.
            self.grid.combine(self.charsets.map(c));
            return;
        }
        // Charset translation happens here, so no non-Unicode char ever enters
        // a cell (`docs/PLAN.md` §8).
        let c = self.charsets.map(c);
        self.last_printed = Some(c);
        self.grid.write(c, c.width().unwrap_or(width));
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.bell_count += 1,
            0x08 => self.grid.backspace(),
            0x09 => self.forward_tab(1),
            // LF, VT and FF all index the cursor down one line.
            0x0a..=0x0c => self.grid.line_feed(),
            0x0d => self.grid.carriage_return(),
            // SO / SI: the locking shifts mc toggles around every frame.
            0x0e => self.charsets.shift_out(),
            0x0f => self.charsets.shift_in(),
            _ => {}
        }
    }

    fn hook(&mut self, _params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        self.dcs.clear();
        self.in_xtgettcap = intermediates == b"+" && action == 'q';
    }

    fn put(&mut self, byte: u8) {
        if self.in_xtgettcap && self.dcs.len() < 1024 {
            self.dcs.push(byte);
        }
    }

    fn unhook(&mut self) {
        if self.in_xtgettcap {
            self.answer_xtgettcap();
            self.in_xtgettcap = false;
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(&code) = params.first() else { return };
        match code {
            b"0" | b"2" => {
                if let Some(t) = params.get(1) {
                    self.title = Some(String::from_utf8_lossy(t).into_owned());
                }
            }
            // OSC 4: set or query an indexed colour. Queries must reply in
            // xterm's exact `rgb:RRRR/GGGG/BBBB` form — apps string-match it.
            b"4" => {
                for pair in params[1..].chunks(2) {
                    let (Some(index), Some(spec)) = (pair.first(), pair.get(1)) else { continue };
                    let Ok(index) = std::str::from_utf8(index).unwrap_or("").parse::<usize>()
                    else {
                        continue;
                    };
                    if index > 255 {
                        continue;
                    }
                    if *spec == b"?" {
                        let rgb = self.palette.colors[index];
                        self.reply_str(&format!("\x1b]4;{index};{}\x1b\\", xterm_rgb(rgb)));
                    } else if let Some(rgb) = parse_color_spec(spec) {
                        self.palette.colors[index] = rgb;
                    }
                }
            }
            // OSC 10/11/12: default fg, bg and cursor colour.
            b"10" | b"11" | b"12" => {
                let which = code[..].to_vec();
                let Some(spec) = params.get(1) else { return };
                let slot = match which.as_slice() {
                    b"10" => 0,
                    b"11" => 1,
                    _ => 2,
                };
                if *spec == b"?" {
                    let rgb = match slot {
                        0 => self.palette.fg,
                        1 => self.palette.bg,
                        _ => self.palette.cursor,
                    };
                    let code = String::from_utf8_lossy(&which).into_owned();
                    self.reply_str(&format!("\x1b]{code};{}\x1b\\", xterm_rgb(rgb)));
                } else if let Some(rgb) = parse_color_spec(spec) {
                    match slot {
                        0 => self.palette.fg = rgb,
                        1 => self.palette.bg = rgb,
                        _ => self.palette.cursor = rgb,
                    }
                }
            }
            b"104" => {
                if params.len() == 1 {
                    self.palette = Palette::default();
                } else {
                    let defaults = Palette::default();
                    for p in &params[1..] {
                        if let Ok(i) = std::str::from_utf8(p).unwrap_or("").parse::<usize>() {
                            if i < 256 {
                                self.palette.colors[i] = defaults.colors[i];
                            }
                        }
                    }
                }
            }
            b"110" => self.palette.fg = Palette::default().fg,
            b"111" => self.palette.bg = Palette::default().bg,
            b"112" => self.palette.cursor = Palette::default().cursor,
            // OSC 52: clipboard. The client decides whether to honour it.
            b"52" => {
                if let Some(data) = params.get(2) {
                    self.clipboard_write = Some(String::from_utf8_lossy(data).into_owned());
                }
            }
            // OSC 7: cwd reporting, consumed by the mux for "split here".
            b"7" => {
                if let Some(url) = params.get(1) {
                    self.cwd = Some(String::from_utf8_lossy(url).into_owned());
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let private = intermediates.first() == Some(&b'?');
        // A private parameter marker changes what a final byte means, and the
        // public and private forms share final bytes. `CSI > 4 ; 1 m` is
        // XTMODKEYS — fish sends it before every prompt — and running it as
        // `SGR 4;1` set underline on the pen and left it set, so every erased
        // cell after it was drawn with a rule through it. Only DA2 (`CSI > c`)
        // is implemented here, so anything else private is ignored rather than
        // mistaken for its namesake.
        if matches!(intermediates.first(), Some(b'<' | b'=' | b'>')) && action != 'c' {
            return;
        }
        match action {
            'A' => self.grid.move_by(-(arg(params, 0, 1) as isize), 0),
            'B' | 'e' => self.grid.move_by(arg(params, 0, 1) as isize, 0),
            'C' | 'a' => self.grid.move_by(0, arg(params, 0, 1) as isize),
            'D' => self.grid.move_by(0, -(arg(params, 0, 1) as isize)),
            'E' => {
                self.grid.move_by(arg(params, 0, 1) as isize, 0);
                self.grid.carriage_return();
            }
            'F' => {
                self.grid.move_by(-(arg(params, 0, 1) as isize), 0);
                self.grid.carriage_return();
            }
            'G' | '`' => {
                let row = self.grid.cursor.row;
                self.grid.move_to(row, arg(params, 0, 1) - 1);
            }
            'd' => {
                let col = self.grid.cursor.col;
                self.grid.move_to(arg(params, 0, 1) - 1, col);
            }
            'H' | 'f' => self.grid.move_to(arg(params, 0, 1) - 1, arg(params, 1, 1) - 1),
            'J' => self.grid.erase_in_display(erase_mode(raw_arg(params, 0))),
            'K' => self.grid.erase_in_line(erase_mode(raw_arg(params, 0))),
            'L' => self.grid.insert_lines(arg(params, 0, 1)),
            'M' => self.grid.delete_lines(arg(params, 0, 1)),
            'P' => self.grid.delete_chars(arg(params, 0, 1)),
            'S' => self.grid.scroll_up(arg(params, 0, 1)),
            'T' => self.grid.scroll_down(arg(params, 0, 1)),
            'X' => self.grid.erase_chars(arg(params, 0, 1)),
            '@' => self.grid.insert_chars(arg(params, 0, 1)),
            'I' => self.forward_tab(arg(params, 0, 1)),
            'Z' => self.backward_tab(arg(params, 0, 1)),
            'g' => {
                // TBC: clear this stop, or all of them.
                match raw_arg(params, 0) {
                    3 => self.tab_stops.iter_mut().for_each(|s| *s = false),
                    _ => {
                        let col = self.grid.cursor.col;
                        self.tab_stops[col] = false;
                    }
                }
            }
            // REP: repeat the last printed character. `tput rep` and some
            // curses builds lean on it for horizontal rules.
            'b' => {
                if let Some(c) = self.last_printed {
                    for _ in 0..arg(params, 0, 1) {
                        let width = c.width().unwrap_or(1);
                        self.grid.write(c, width);
                    }
                }
            }
            'q' if intermediates.first() == Some(&b' ') => {
                // DECSCUSR.
                let (shape, blink) = match arg(params, 0, 1) {
                    0 | 1 => (CursorShape::Block, true),
                    2 => (CursorShape::Block, false),
                    3 => (CursorShape::Underline, true),
                    4 => (CursorShape::Underline, false),
                    5 => (CursorShape::Bar, true),
                    _ => (CursorShape::Bar, false),
                };
                self.cursor_shape = shape;
                self.cursor_blink = blink;
            }
            'p' if intermediates.first() == Some(&b'!') => self.soft_reset(),
            'h' => self.set_mode(private, params, true),
            'l' => self.set_mode(private, params, false),
            'm' => self.sgr(params),
            // `CSI ? Pm r` / `CSI ? Pm s` are XTRESTORE/XTSAVE, not DECSTBM
            // and not a cursor save: same final byte, different sequence.
            'r' | 's' | 'u' if private => {}
            'r' => {
                let top = arg(params, 0, 1) - 1;
                let bot = arg(params, 1, self.grid.num_rows() as u16) - 1;
                if top < bot && bot < self.grid.num_rows() {
                    self.grid.scroll_top = top;
                    self.grid.scroll_bot = bot;
                }
                self.grid.move_to(top, 0);
            }
            's' => self.grid.save_cursor(),
            'u' => self.grid.restore_cursor(),
            'c' => {
                if intermediates.first() == Some(&b'>') {
                    self.reply.extend_from_slice(DA2_REPLY);
                } else {
                    self.reply.extend_from_slice(DA1_REPLY);
                }
            }
            'n' => match raw_arg(params, 0) {
                5 => self.reply_str("\x1b[0n"),
                // CPR. 1-based, and the row must be reported within the grid
                // even when a deferred wrap is pending.
                6 => {
                    let (r, c) = (self.grid.cursor.row + 1, self.grid.cursor.col + 1);
                    self.reply_str(&format!("\x1b[{r};{c}R"));
                }
                _ => {}
            },
            'q' if intermediates.first() == Some(&b'>') => {
                // XTVERSION.
                self.reply_str(&format!("\x1bP>|haven({VERSION})\x1b\\"));
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates.first(), byte) {
            (None, b'D') => self.grid.line_feed(),
            (None, b'E') => {
                self.grid.line_feed();
                self.grid.carriage_return();
            }
            (None, b'M') => self.grid.reverse_line_feed(),
            // HTS: set a tab stop at the cursor.
            (None, b'H') => {
                let col = self.grid.cursor.col;
                self.tab_stops[col] = true;
            }
            (None, b'7') => self.grid.save_cursor(),
            (None, b'8') => self.grid.restore_cursor(),
            (None, b'c') => {
                // RIS.
                let (cols, rows) = (self.grid.cols(), self.grid.num_rows());
                self.grid = Grid::new(cols, rows);
                self.modes = Modes::default();
                self.charsets.reset();
                self.palette = Palette::default();
                self.tab_stops = default_tab_stops(cols);
                self.cursor_shape = CursorShape::default();
                self.alt_active = false;
                self.inactive = None;
                self.set_pen(Style::default());
            }
            // SS2 / SS3.
            (None, b'N') => self.charsets.single_shift(2),
            (None, b'O') => self.charsets.single_shift(3),
            // SCS: designate a charset into G0-G3.
            (Some(&i), byte) => {
                let slot = match i {
                    b'(' => Some(0),
                    b')' => Some(1),
                    b'*' => Some(2),
                    b'+' => Some(3),
                    _ => None,
                };
                if let (Some(slot), Some(cs)) = (slot, Charset::from_designator(byte)) {
                    self.charsets.designate(slot, cs);
                }
            }
            _ => {}
        }
    }
}

fn erase_mode(n: usize) -> EraseMode {
    match n {
        1 => EraseMode::ToStart,
        2 | 3 => EraseMode::All,
        _ => EraseMode::ToEnd,
    }
}

/// A terminal plus its parser: feed it bytes, read the grid.
pub struct Emulator {
    parser: Parser,
    pub term: Terminal,
}

// `vte::Parser` is not Debug; the state we care about is all in `term`.
impl std::fmt::Debug for Emulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emulator").field("term", &self.term).finish_non_exhaustive()
    }
}

impl Emulator {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self { parser: Parser::new(), term: Terminal::new(cols, rows) }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn dump(&self) -> String {
        self.term.grid.dump()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emu(bytes: &[u8]) -> Emulator {
        let mut e = Emulator::new(20, 4);
        e.feed(bytes);
        e
    }

    #[test]
    fn cursor_addressing_and_erase() {
        let e = emu(b"hello\x1b[2;3Hworld\x1b[1;1H\x1b[K");
        assert_eq!(e.dump(), "\n  world\n\n\n");
    }

    #[test]
    fn sgr_semicolon_truecolor() {
        let e = emu(b"\x1b[38;2;10;20;30mx");
        let style = e.term.styles.get(e.term.grid.row(0).cells[0].style);
        assert_eq!(style.fg, Color::Rgb(Rgb::new(10, 20, 30)));
        assert_eq!(e.dump().lines().next().unwrap(), "x");
    }

    #[test]
    fn sgr_colon_truecolor_with_empty_colorspace() {
        // The ITU T.416 form that `;`-only parsers get wrong.
        let e = emu(b"\x1b[38:2::1:2:3mx");
        let style = e.term.styles.get(e.term.grid.row(0).cells[0].style);
        assert_eq!(style.fg, Color::Rgb(Rgb::new(1, 2, 3)));
    }

    #[test]
    fn xtmodkeys_is_not_sgr() {
        // `CSI > 4 ; 1 m` is modifyOtherKeys, which fish sends before every
        // prompt. Read as SGR it sets underline and bold on the pen, and
        // because nothing resets it the attribute runs on through every cell
        // erased afterwards — a rule drawn across the blank end of the row.
        let e = emu(b"\x1b[>4;1mx");
        let style = e.term.styles.get(e.term.grid.row(0).cells[0].style);
        assert_eq!(style.underline, Underline::None, "XTMODKEYS set an underline");
        assert!(!style.attrs.contains(Attrs::BOLD), "XTMODKEYS set bold");
    }

    #[test]
    fn private_erase_still_erases_but_xtsave_does_not_move_the_margins() {
        // `?` stays a DEC private mode marker; `>`/`=`/`<` do not reach the
        // public handlers at all.
        let mut e = emu(b"\x1b[?2;3r");
        assert_eq!(e.term.grid.scroll_top, 0, "XTRESTORE moved the scroll region");
        assert_eq!(e.term.grid.scroll_bot, e.term.grid.num_rows() - 1);
        // The public form still works.
        e = emu(b"\x1b[2;3r");
        assert_eq!(e.term.grid.scroll_top, 1);
        assert_eq!(e.term.grid.scroll_bot, 2);
    }

    #[test]
    fn sgr_colon_underline_style_and_color() {
        let e = emu(b"\x1b[4:3m\x1b[58:5:9mx");
        let style = e.term.styles.get(e.term.grid.row(0).cells[0].style);
        assert_eq!(style.underline, Underline::Curly);
        assert_eq!(style.underline_color, Color::Indexed(9));
    }

    #[test]
    fn malformed_sgr_does_not_abort_the_sequence() {
        let e = emu(b"\x1b[38;9;1mx");
        // The bogus 38;9 is skipped; bold from a later param still applies.
        let style = e.term.styles.get(e.term.grid.row(0).cells[0].style);
        assert!(style.attrs.contains(Attrs::BOLD) || style.fg == Color::Default);
    }

    #[test]
    fn bright_colors_map_into_the_upper_ansi_half() {
        let e = emu(b"\x1b[91mx");
        let style = e.term.styles.get(e.term.grid.row(0).cells[0].style);
        assert_eq!(style.fg, Color::Indexed(9));
    }

    #[test]
    fn answers_da1_da2_and_cpr() {
        let mut e = Emulator::new(20, 4);
        e.feed(b"\x1b[c");
        assert_eq!(e.term.take_reply(), DA1_REPLY);
        e.feed(b"\x1b[>c");
        assert_eq!(e.term.take_reply(), DA2_REPLY);
        e.feed(b"\x1b[3;5H\x1b[6n");
        assert_eq!(e.term.take_reply(), b"\x1b[3;5R");
        e.feed(b"\x1b[5n");
        assert_eq!(e.term.take_reply(), b"\x1b[0n");
    }

    #[test]
    fn answers_xtgettcap_for_known_and_unknown_caps() {
        let mut e = Emulator::new(20, 4);
        // "TN" and "RGB" hex-encoded, plus an unknown cap.
        e.feed(b"\x1bP+q544e;524742;5a5a\x1b\\");
        let reply = String::from_utf8(e.term.take_reply()).unwrap();
        let tn = hex_encode(b"xterm-256color");
        let rgb = hex_encode(b"8/8/8");
        assert!(reply.contains(&format!("\x1bP1+r544E={tn}\x1b\\")), "{reply:?}");
        assert!(reply.contains(&format!("\x1bP1+r524742={rgb}\x1b\\")), "{reply:?}");
        assert!(reply.contains("\x1bP0+r\x1b\\"), "unknown cap must be refused: {reply:?}");
    }

    #[test]
    fn scroll_region_confines_line_feeds() {
        let mut e = Emulator::new(4, 4);
        // LF alone does not carriage-return, hence the explicit \r.
        e.feed(b"\x1b[2;3r\x1b[2;1Ha\r\nb\r\nc");
        // Rows 1 and 4 (1-based) are outside the region and stay empty.
        assert_eq!(e.dump(), "\nb\nc\n\n");
    }

    #[test]
    fn modes_track_decset() {
        let mut e = Emulator::new(4, 4);
        e.feed(b"\x1b[?2004h\x1b[?25l\x1b[?7l");
        assert!(e.term.modes.bracketed_paste);
        assert!(!e.term.modes.cursor_visible);
        assert!(!e.term.modes.autowrap);
        assert!(!e.term.grid.autowrap);
    }

    #[test]
    fn dec_graphics_renders_a_box_the_way_mc_draws_one() {
        // Exactly what mc emits in a non-UTF-8 locale: designate graphics into
        // G0, draw with ASCII letters, designate ASCII back.
        let e = emu(b"\x1b(0lqqk\x1b(BText");
        assert_eq!(e.dump().lines().next().unwrap(), "┌──┐Text");
    }

    #[test]
    fn locking_shifts_switch_charsets_mid_line() {
        let e = emu(b"\x1b)0A\x0eqq\x0fB");
        assert_eq!(e.dump().lines().next().unwrap(), "A──B");
    }

    #[test]
    fn combining_characters_from_the_stream_extend_the_previous_cell() {
        // Decomposed "é" as a shell would emit it.
        let e = emu("e\u{0301}clair".as_bytes());
        assert_eq!(e.dump().lines().next().unwrap(), "e\u{0301}clair");
        // One cell per visible character, so columns still line up.
        assert_eq!(e.term.grid.cursor.col, 6);
    }

    #[test]
    fn erase_modes_are_not_off_by_one() {
        // Regression: `0` is a meaningful ED/EL mode, not an absent parameter.
        let mut e = Emulator::new(6, 1);
        e.feed(b"abcdef\x1b[1;4H\x1b[0K");
        assert_eq!(e.dump(), "abc\n");

        let mut e = Emulator::new(6, 1);
        e.feed(b"abcdef\x1b[1;4H\x1b[1K");
        assert_eq!(e.dump(), "    ef\n");

        let mut e = Emulator::new(6, 1);
        e.feed(b"abcdef\x1b[2K");
        assert_eq!(e.dump(), "\n");

        let mut e = Emulator::new(4, 2);
        e.feed(b"ab\r\ncd\x1b[2J");
        assert_eq!(e.dump(), "\n\n");
    }

    #[test]
    fn the_alternate_screen_preserves_the_primary_exactly() {
        // This is mc's Ctrl+O toggle: enter alt, scribble, leave, and the
        // shell's screen must be byte-identical to what it was.
        let mut e = Emulator::new(10, 3);
        e.feed(b"primary\r\nsecond");
        let before = e.dump();
        let cursor_before = e.term.grid.cursor;

        e.feed(b"\x1b[?1049h");
        assert!(e.term.alt_active());
        assert_eq!(e.dump().trim(), "", "entering alt must show a cleared screen");
        e.feed(b"\x1b[HALT SCREEN");
        assert!(e.dump().contains("ALT SCREEN"));
        // The primary is still intact underneath.
        assert!(e.term.primary().dump().contains("primary"));

        e.feed(b"\x1b[?1049l");
        assert!(!e.term.alt_active());
        assert_eq!(e.dump(), before, "primary screen was not restored");
        assert_eq!(
            (e.term.grid.cursor.row, e.term.grid.cursor.col),
            (cursor_before.row, cursor_before.col),
            "1049 must restore the cursor too"
        );
    }

    #[test]
    fn the_alternate_screen_contributes_no_scrollback() {
        let mut e = Emulator::new(6, 2);
        e.feed(b"\x1b[?1049h");
        e.feed(b"one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(e.term.grid.scrollback_len(), 0);
        e.feed(b"\x1b[?1049l");
        assert_eq!(e.term.grid.scrollback_len(), 0, "alt output must not enter history");
    }

    #[test]
    fn legacy_47_switches_without_touching_the_cursor() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"\x1b[2;5H\x1b[?47h");
        assert!(e.term.alt_active());
        e.feed(b"\x1b[?47l");
        assert!(!e.term.alt_active());
    }

    #[test]
    fn osc_4_sets_and_queries_indexed_colours() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"\x1b]4;1;#ff8000\x1b\\");
        assert_eq!(e.term.palette.colors[1], Rgb::new(0xff, 0x80, 0x00));

        e.feed(b"\x1b]4;1;?\x1b\\");
        let reply = String::from_utf8(e.term.take_reply()).unwrap();
        // xterm's exact format, which apps string-match.
        assert_eq!(reply, "\x1b]4;1;rgb:ffff/8080/0000\x1b\\");
    }

    #[test]
    fn osc_4_accepts_the_rgb_colon_form_at_several_precisions() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"\x1b]4;2;rgb:12/34/56\x1b\\");
        assert_eq!(e.term.palette.colors[2], Rgb::new(0x12, 0x34, 0x56));
        e.feed(b"\x1b]4;3;rgb:1234/5678/9abc\x1b\\");
        assert_eq!(e.term.palette.colors[3], Rgb::new(0x12, 0x56, 0x9a));
    }

    #[test]
    fn osc_10_and_11_set_and_query_the_defaults() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"\x1b]11;#000010\x1b\\");
        assert_eq!(e.term.palette.bg, Rgb::new(0, 0, 0x10));
        e.feed(b"\x1b]11;?\x1b\\");
        let reply = String::from_utf8(e.term.take_reply()).unwrap();
        assert_eq!(reply, "\x1b]11;rgb:0000/0000/1010\x1b\\");
        // Light-vs-dark detection depends on this query answering at all.
        e.feed(b"\x1b]111\x1b\\");
        assert_eq!(e.term.palette.bg, Palette::default().bg);
    }

    #[test]
    fn osc_104_resets_the_palette() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"\x1b]4;5;#010203\x1b\\");
        e.feed(b"\x1b]104;5\x1b\\");
        assert_eq!(e.term.palette.colors[5], Palette::default().colors[5]);
    }

    #[test]
    fn a_colour_stays_indexed_so_a_later_palette_change_repaints_it() {
        // The invariant from §7.1: cells store the index, not the resolved RGB.
        let mut e = Emulator::new(10, 2);
        e.feed(b"\x1b[31mX");
        let style = e.term.styles.get(e.term.grid.row(0).cells[0].style);
        assert_eq!(style.fg, Color::Indexed(1));
        e.feed(b"\x1b]4;1;#123456\x1b\\");
        // The cell is untouched; only resolution changes.
        let style = e.term.styles.get(e.term.grid.row(0).cells[0].style);
        assert_eq!(style.fg, Color::Indexed(1));
        assert_eq!(
            e.term.palette.resolve(style.fg, crate::color::Role::Fg),
            Rgb::new(0x12, 0x34, 0x56)
        );
    }

    #[test]
    fn tab_stops_can_be_set_and_cleared() {
        let mut e = Emulator::new(20, 2);
        // Default stops every 8 columns.
        e.feed(b"a\tb");
        assert_eq!(e.dump().lines().next().unwrap(), "a       b");

        // Clear all stops, set one at column 3, and tab to it.
        e.feed(b"\x1b[3g\x1b[1;4H\x1bH\x1b[1;1H\x1b[2K\tX");
        assert_eq!(e.dump().lines().next().unwrap(), "   X");
    }

    #[test]
    fn back_tab_moves_to_the_previous_stop() {
        let mut e = Emulator::new(30, 2);
        e.feed(b"\x1b[1;20H\x1b[Z");
        assert_eq!(e.term.grid.cursor.col, 16);
    }

    #[test]
    fn rep_repeats_the_last_printed_character() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"-\x1b[4b");
        assert_eq!(e.dump().lines().next().unwrap(), "-----");
    }

    #[test]
    fn decscusr_selects_a_cursor_shape() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"\x1b[4 q");
        assert_eq!(e.term.cursor_shape, CursorShape::Underline);
        assert!(!e.term.cursor_blink);
        e.feed(b"\x1b[5 q");
        assert_eq!(e.term.cursor_shape, CursorShape::Bar);
        assert!(e.term.cursor_blink);
    }

    #[test]
    fn soft_reset_restores_modes_without_clearing_the_screen() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"keep\x1b[?7l\x1b[3;3r\x1b[31m");
        e.feed(b"\x1b[!p");
        assert!(e.term.modes.autowrap, "DECSTR must restore DECAWM");
        assert_eq!(e.term.grid.scroll_top, 0);
        assert!(e.dump().contains("keep"), "DECSTR must not clear the screen");
    }

    #[test]
    fn osc_52_captures_a_clipboard_write_for_the_client_to_judge() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"\x1b]52;c;aGVsbG8=\x1b\\");
        assert_eq!(e.term.clipboard_write.as_deref(), Some("aGVsbG8="));
    }

    #[test]
    fn osc_7_records_the_working_directory() {
        let mut e = Emulator::new(10, 2);
        e.feed(b"\x1b]7;file://host/home/romeo\x1b\\");
        assert_eq!(e.term.cwd.as_deref(), Some("file://host/home/romeo"));
    }

    #[test]
    fn osc_sets_title() {
        let mut e = Emulator::new(4, 4);
        e.feed(b"\x1b]0;hello\x07");
        assert_eq!(e.term.title.as_deref(), Some("hello"));
    }
}
