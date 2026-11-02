//! Cell styling and the style interner. See `docs/PLAN.md` §6.
//!
//! Cells hold a 16-bit [`StyleId`], not an inline style blob: it keeps `Cell`
//! small (cache-friendly scrolling and diffing) and makes damage diffing a
//! `u16` comparison.

use std::collections::HashMap;

use crate::color::Color;

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize)]
    pub struct Attrs: u16 {
        const BOLD      = 1 << 0;
        const DIM       = 1 << 1;
        const ITALIC    = 1 << 2;
        const BLINK     = 1 << 3;
        const REVERSE   = 1 << 4;
        const HIDDEN    = 1 << 5;
        const STRIKE    = 1 << 6;
        const OVERLINE  = 1 << 7;
    }
}

#[derive(
    Clone, Copy, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl Underline {
    /// `SGR 4:n`.
    pub fn from_sgr(n: u16) -> Self {
        match n {
            0 => Self::None,
            1 => Self::Single,
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => Self::Single,
        }
    }
}

#[derive(
    Clone, Copy, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize,
)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Color,
    pub attrs: Attrs,
    pub underline: Underline,
}

/// `Default` is index 0, which is `Style::default()` — see [`StyleTable`].
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Debug,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct StyleId(pub u16);

impl StyleId {
    /// Index 0 is always `Style::default()`, so a fresh grid needs no interning.
    pub const DEFAULT: StyleId = StyleId(0);
}

#[derive(Clone, Debug)]
pub struct StyleTable {
    styles: Vec<Style>,
    index: HashMap<Style, StyleId>,
}

impl Default for StyleTable {
    fn default() -> Self {
        let default = Style::default();
        let mut index = HashMap::new();
        index.insert(default, StyleId::DEFAULT);
        Self { styles: vec![default], index }
    }
}

impl StyleTable {
    pub fn intern(&mut self, style: Style) -> StyleId {
        if let Some(&id) = self.index.get(&style) {
            return id;
        }
        // u16 exhaustion needs 65k distinct styles on screen at once. If we ever
        // hit it, reuse the default rather than panicking in a render path.
        let Ok(raw) = u16::try_from(self.styles.len()) else {
            return StyleId::DEFAULT;
        };
        let id = StyleId(raw);
        self.styles.push(style);
        self.index.insert(style, id);
        id
    }

    pub fn get(&self, id: StyleId) -> Style {
        self.styles.get(id.0 as usize).copied().unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgb;

    #[test]
    fn interning_is_stable_and_deduplicated() {
        let mut t = StyleTable::default();
        assert_eq!(t.intern(Style::default()), StyleId::DEFAULT);

        let red = Style { fg: Color::Rgb(Rgb::new(255, 0, 0)), ..Default::default() };
        let a = t.intern(red);
        let b = t.intern(red);
        assert_eq!(a, b);
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(a), red);
    }
}
