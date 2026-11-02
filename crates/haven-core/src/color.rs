//! Color model. See `docs/PLAN.md` §7.
//!
//! The important invariant: `Default` and `Indexed` are *stored*, never eagerly
//! resolved to RGB. `OSC 4/10/11` and live theme reload must repaint existing
//! cells, so resolution happens at present time against the current palette.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

#[derive(
    Clone, Copy, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Color {
    /// `SGR 39`/`49`: whatever the theme's fg/bg is *now*.
    #[default]
    Default,
    /// 0-15 ANSI (theme controlled), 16-255 (6x6x6 cube + 24 greys).
    Indexed(u8),
    /// `SGR 38;2` / `48;2`.
    Rgb(Rgb),
}

/// Which default a [`Color::Default`] resolves to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Fg,
    Bg,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Palette {
    pub colors: [Rgb; 256],
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        let mut colors = [Rgb::new(0, 0, 0); 256];

        // 0-15: the xterm defaults, so a bare `SGR 31` looks like it does everywhere else.
        const ANSI: [Rgb; 16] = [
            Rgb::new(0x00, 0x00, 0x00),
            Rgb::new(0xcd, 0x00, 0x00),
            Rgb::new(0x00, 0xcd, 0x00),
            Rgb::new(0xcd, 0xcd, 0x00),
            Rgb::new(0x00, 0x00, 0xee),
            Rgb::new(0xcd, 0x00, 0xcd),
            Rgb::new(0x00, 0xcd, 0xcd),
            Rgb::new(0xe5, 0xe5, 0xe5),
            Rgb::new(0x7f, 0x7f, 0x7f),
            Rgb::new(0xff, 0x00, 0x00),
            Rgb::new(0x00, 0xff, 0x00),
            Rgb::new(0xff, 0xff, 0x00),
            Rgb::new(0x5c, 0x5c, 0xff),
            Rgb::new(0xff, 0x00, 0xff),
            Rgb::new(0x00, 0xff, 0xff),
            Rgb::new(0xff, 0xff, 0xff),
        ];
        colors[..16].copy_from_slice(&ANSI);

        // 16-231: 6x6x6 cube, with xterm's non-linear step table.
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let mut i = 16;
        for &r in &STEPS {
            for &g in &STEPS {
                for &b in &STEPS {
                    colors[i] = Rgb::new(r, g, b);
                    i += 1;
                }
            }
        }

        // 232-255: 24 greys, 8..238 in steps of 10.
        for n in 0..24u8 {
            let v = 8 + n * 10;
            colors[232 + n as usize] = Rgb::new(v, v, v);
        }

        Self {
            colors,
            fg: Rgb::new(0xd0, 0xd0, 0xd0),
            bg: Rgb::new(0x10, 0x10, 0x14),
            cursor: Rgb::new(0xd0, 0xd0, 0xd0),
        }
    }
}

impl Palette {
    pub fn resolve(&self, color: Color, role: Role) -> Rgb {
        match color {
            Color::Default => match role {
                Role::Fg => self.fg,
                Role::Bg => self.bg,
            },
            Color::Indexed(i) => self.colors[i as usize],
            Color::Rgb(rgb) => rgb,
        }
    }
}

/// Perceptual color space, for choosing the closest available color when a
/// client cannot show what an app asked for (`docs/PLAN.md` §7.5).
///
/// Naive RGB distance picks visibly wrong colors — it treats a change in blue
/// as mattering as much as the same change in green, which it does not.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Oklab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

impl From<Rgb> for Oklab {
    fn from(rgb: Rgb) -> Self {
        // sRGB -> linear
        let f = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (f(rgb.r), f(rgb.g), f(rgb.b));

        // linear sRGB -> LMS -> Oklab (Björn Ottosson's matrices)
        let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b;
        let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
        let s = 0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_7 * b;
        let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());

        Self {
            l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        }
    }
}

impl Oklab {
    pub fn distance_sq(self, other: Self) -> f32 {
        let (dl, da, db) = (self.l - other.l, self.a - other.a, self.b - other.b);
        dl * dl + da * da + db * db
    }
}

impl Palette {
    /// The closest palette entry below `limit`, matched perceptually.
    ///
    /// `limit` is 16 or 256; matching happens against *this* palette, so a
    /// client that learned its host's real colors via `OSC 4` gets a better
    /// answer than one assuming xterm defaults.
    pub fn nearest_index(&self, target: Rgb, limit: usize) -> u8 {
        let target = Oklab::from(target);
        let limit = limit.clamp(1, 256);
        let mut best = 0usize;
        let mut best_distance = f32::MAX;
        for (i, &candidate) in self.colors[..limit].iter().enumerate() {
            let d = Oklab::from(candidate).distance_sq(target);
            if d < best_distance {
                best_distance = d;
                best = i;
            }
        }
        best as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_and_greys_match_xterm() {
        let p = Palette::default();
        assert_eq!(p.colors[16], Rgb::new(0, 0, 0));
        assert_eq!(p.colors[231], Rgb::new(255, 255, 255));
        // 196 is the canonical "bright red" cube entry.
        assert_eq!(p.colors[196], Rgb::new(255, 0, 0));
        assert_eq!(p.colors[232], Rgb::new(8, 8, 8));
        assert_eq!(p.colors[255], Rgb::new(238, 238, 238));
    }

    #[test]
    fn oklab_matching_picks_perceptually_sensible_colours() {
        let p = Palette::default();
        // An exact palette entry must map to *a* slot holding that colour.
        // Not necessarily the same index: the xterm palette has duplicates
        // (15 and 231 are both pure white), and either answer is correct.
        for index in [0u8, 1, 9, 15, 100, 231, 250] {
            let rgb = p.colors[index as usize];
            let matched = p.nearest_index(rgb, 256);
            assert_eq!(p.colors[matched as usize], rgb, "index {index} matched a different colour");
        }
        // A near-black maps into the dark end, not to white.
        let near_black = p.nearest_index(Rgb::new(3, 3, 3), 16);
        assert!(matches!(near_black, 0 | 8), "near-black mapped to {near_black}");
        // Saturated red maps to a red, not to magenta or yellow.
        let red = p.nearest_index(Rgb::new(250, 5, 5), 16);
        assert!(matches!(red, 1 | 9), "red mapped to {red}");
    }

    #[test]
    fn narrowing_to_16_colours_stays_within_the_ansi_range() {
        let p = Palette::default();
        for rgb in [Rgb::new(12, 200, 90), Rgb::new(255, 255, 200), Rgb::new(40, 40, 120)] {
            assert!(p.nearest_index(rgb, 16) < 16);
        }
    }

    #[test]
    fn default_resolves_per_role() {
        let p = Palette::default();
        assert_eq!(p.resolve(Color::Default, Role::Fg), p.fg);
        assert_eq!(p.resolve(Color::Default, Role::Bg), p.bg);
    }
}
