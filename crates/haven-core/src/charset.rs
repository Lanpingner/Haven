//! Character sets (`SCS`) and DEC Special Graphics. See `docs/PLAN.md` §8.
//!
//! This is the single most-cited reason a from-scratch terminal "cannot run
//! mc": in a non-UTF-8 locale, mc (and `dialog`, and plenty of ncurses builds)
//! draws every frame by designating DEC Special Graphics into G0 and sending
//! plain ASCII letters. Without this translation the user sees `qqqq` where the
//! box should be.
//!
//! We translate to Unicode at print time and keep only the charset *state*, so
//! the rest of the system never sees a non-Unicode cell.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Charset {
    #[default]
    Ascii,
    /// `ESC ( 0`
    DecSpecialGraphics,
    /// `ESC ( A`
    Uk,
}

impl Charset {
    /// Designation byte from an `SCS` sequence.
    pub fn from_designator(byte: u8) -> Option<Self> {
        match byte {
            b'0' | b'2' => Some(Self::DecSpecialGraphics),
            b'A' => Some(Self::Uk),
            b'B' | b'1' => Some(Self::Ascii),
            _ => None,
        }
    }

    pub fn map(self, c: char) -> char {
        match self {
            Self::Ascii => c,
            Self::Uk => {
                if c == '#' {
                    '£'
                } else {
                    c
                }
            }
            Self::DecSpecialGraphics => dec_graphics(c),
        }
    }
}

/// The DEC Special Graphics repertoire, `0x5f..=0x7e`.
fn dec_graphics(c: char) -> char {
    match c {
        '_' => ' ',
        '`' => '◆',
        'a' => '▒',
        'b' => '␉',
        'c' => '␌',
        'd' => '␍',
        'e' => '␊',
        'f' => '°',
        'g' => '±',
        'h' => '␤',
        'i' => '␋',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        other => other,
    }
}

/// The four designatable slots, plus which one `GL` currently points at.
#[derive(Clone, Copy, Debug, Default)]
pub struct Charsets {
    slots: [Charset; 4],
    /// `SI` selects G0, `SO` selects G1.
    gl: usize,
    /// `SS2`/`SS3` apply to exactly one following character.
    single_shift: Option<usize>,
}

impl Charsets {
    pub fn designate(&mut self, slot: usize, charset: Charset) {
        if let Some(s) = self.slots.get_mut(slot) {
            *s = charset;
        }
    }

    pub fn shift_in(&mut self) {
        self.gl = 0;
    }

    pub fn shift_out(&mut self) {
        self.gl = 1;
    }

    pub fn single_shift(&mut self, slot: usize) {
        self.single_shift = Some(slot);
    }

    /// Translate one printable character, consuming any pending single shift.
    pub fn map(&mut self, c: char) -> char {
        let slot = self.single_shift.take().unwrap_or(self.gl);
        self.slots[slot.min(3)].map(c)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dec_graphics_maps_the_box_drawing_range() {
        // The exact bytes mc sends for a panel frame.
        assert_eq!(Charset::DecSpecialGraphics.map('q'), '─');
        assert_eq!(Charset::DecSpecialGraphics.map('x'), '│');
        assert_eq!(Charset::DecSpecialGraphics.map('l'), '┌');
        assert_eq!(Charset::DecSpecialGraphics.map('n'), '┼');
        // Outside the repertoire, characters pass through unchanged.
        assert_eq!(Charset::DecSpecialGraphics.map('A'), 'A');
    }

    #[test]
    fn shift_in_and_out_switch_between_g0_and_g1() {
        let mut cs = Charsets::default();
        cs.designate(1, Charset::DecSpecialGraphics);
        assert_eq!(cs.map('q'), 'q');
        cs.shift_out();
        assert_eq!(cs.map('q'), '─');
        cs.shift_in();
        assert_eq!(cs.map('q'), 'q');
    }

    #[test]
    fn single_shift_applies_to_exactly_one_character() {
        let mut cs = Charsets::default();
        cs.designate(2, Charset::DecSpecialGraphics);
        cs.single_shift(2);
        assert_eq!(cs.map('q'), '─');
        assert_eq!(cs.map('q'), 'q');
    }
}
