//! Themes: what the interface is painted with.
//!
//! A theme is *data*, not code, which is why it does not wait for the plugin
//! host (`docs/PLAN.md` §15). Colours and a handful of glyphs can be read
//! from a file and applied; nothing needs to execute. When the WASM host
//! lands a plugin will be able to *supply* one of these, but it will supply
//! the same struct, not a different mechanism.
//!
//! It lives in the protocol crate because the daemon owns the configuration
//! and hands the theme to every client. One session therefore looks the same
//! in a window and in a terminal, and editing the config changes both at
//! once — which a per-client flag could never promise.
//!
//! Every colour the status bar and the chooser use comes from here. Nothing
//! downstream may hardcode one — a theme that only covers half the interface
//! is worse than no theme at all.

use haven_core::{Color, Rgb, Style};
use serde::{Deserialize, Serialize};

/// One painted role. Kept as a pair so a theme never has to specify a
/// foreground without the background it has to be legible against.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Pair {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
}

impl Pair {
    pub const fn new(fg: Color, bg: Color) -> Self {
        Self { fg, bg, bold: false }
    }

    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn style(self) -> Style {
        let mut style = Style { fg: self.fg, bg: self.bg, ..Style::default() };
        if self.bold {
            style.attrs.insert(haven_core::Attrs::BOLD);
        }
        style
    }
}

/// The glyphs the interface draws itself with.
///
/// Separate from the colours because a terminal with a font that lacks box
/// drawing needs different characters, not different colours — and because
/// the ASCII set is how the interface stays usable over a bad SSH link.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Glyphs {
    pub top_left: String,
    pub top_right: String,
    pub bottom_left: String,
    pub bottom_right: String,
    pub horizontal: String,
    pub vertical: String,
    /// Where the rules between panes meet. Named for the way the stem points:
    /// `tee_left` is the one whose stem runs left out of a vertical rule.
    /// Without these a cross between four panes is drawn as a bare `|`, and
    /// the horizontal rule appears to stop dead at it.
    pub tee_left: String,
    pub tee_right: String,
    pub tee_up: String,
    pub tee_down: String,
    pub cross: String,
    /// Tree guides in the chooser.
    pub branch: String,
    pub last_branch: String,
    pub trunk: String,
    /// Marks the row you are on.
    pub here: String,
    /// Separates the right-hand status fields.
    pub separator: String,
}

impl Default for Glyphs {
    fn default() -> Self {
        Self {
            // Square, not rounded. The renderer draws U+2500-259F itself so
            // that arms meet exactly; the rounded corners are outside that
            // range, fall back to the font, and leave a visible gap where
            // they should join the line.
            top_left: "┌".into(),
            top_right: "┐".into(),
            bottom_left: "└".into(),
            bottom_right: "┘".into(),
            horizontal: "─".into(),
            vertical: "│".into(),
            tee_left: "┤".into(),
            tee_right: "├".into(),
            tee_up: "┴".into(),
            tee_down: "┬".into(),
            cross: "┼".into(),
            branch: "├─".into(),
            last_branch: "└─".into(),
            trunk: "│ ".into(),
            here: "●".into(),
            separator: "·".into(),
        }
    }
}

impl Glyphs {
    /// For a terminal that cannot draw the box set.
    pub fn ascii() -> Self {
        Self {
            top_left: "+".into(),
            top_right: "+".into(),
            bottom_left: "+".into(),
            bottom_right: "+".into(),
            horizontal: "-".into(),
            vertical: "|".into(),
            tee_left: "+".into(),
            tee_right: "+".into(),
            tee_up: "+".into(),
            tee_down: "+".into(),
            cross: "+".into(),
            branch: "|-".into(),
            last_branch: "`-".into(),
            trunk: "| ".into(),
            here: "*".into(),
            separator: "|".into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    /// The session name at the left of the bar.
    pub bar_session: Pair,
    /// The bar itself, where nothing else is drawn.
    pub bar: Pair,
    /// A tab that is not the current one.
    pub tab: Pair,
    pub tab_active: Pair,
    /// The counters and clock at the right.
    pub bar_dim: Pair,
    /// While the prefix is armed, so it is obvious the next key is a command.
    pub bar_alert: Pair,
    /// The command line.
    pub command: Pair,
    /// Panels: the chooser and the completion box.
    pub panel: Pair,
    pub panel_border: Pair,
    pub panel_title: Pair,
    pub panel_dim: Pair,
    /// The rules the layout leaves between panes. Its background stays the
    /// terminal's own: the gap is between panes, not a surface of its own,
    /// and a panel colour there reads as a stripe down the screen.
    pub pane_border: Pair,
    pub selection: Pair,
    pub glyphs: Glyphs,
}

impl Default for Theme {
    fn default() -> Self {
        Self::haven()
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(Rgb { r, g, b })
}

/// The six-and-a-bit colours a theme is actually built from. Named rather
/// than positional: seven colour arguments in a row is a swap waiting to
/// happen, and swapping ink for text produces something that still compiles.
#[derive(Clone, Copy, Debug)]
struct Swatches {
    ink: Color,
    surface: Color,
    raised: Color,
    text: Color,
    faint: Color,
    accent: Color,
    warm: Color,
}

impl Theme {
    /// The default. Built from a small palette rather than from the
    /// terminal's ANSI colours, so the interface looks the same whatever the
    /// pane contents have done to the palette.
    pub fn haven() -> Self {
        Self::from_palette(
            "haven",
            Swatches {
                ink: rgb(0x1b, 0x1e, 0x28),
                surface: rgb(0x24, 0x28, 0x36),
                raised: rgb(0x2e, 0x33, 0x44),
                text: rgb(0xc8, 0xd0, 0xe0),
                faint: rgb(0x7a, 0x84, 0x9c),
                accent: rgb(0x6c, 0x9c, 0xf0),
                warm: rgb(0xf0, 0xb4, 0x6c),
            },
        )
    }

    /// Sixteen-colour only: for a host terminal that cannot do better, and
    /// for anyone who wants the interface to follow their own palette.
    pub fn ansi() -> Self {
        Self {
            name: "ansi".into(),
            bar_session: Pair::new(Color::Indexed(0), Color::Indexed(4)).bold(),
            bar: Pair::new(Color::Indexed(7), Color::Indexed(8)),
            tab: Pair::new(Color::Indexed(7), Color::Indexed(8)),
            tab_active: Pair::new(Color::Indexed(0), Color::Indexed(15)).bold(),
            bar_dim: Pair::new(Color::Indexed(7), Color::Indexed(8)),
            bar_alert: Pair::new(Color::Indexed(0), Color::Indexed(3)).bold(),
            command: Pair::new(Color::Indexed(15), Color::Indexed(8)),
            panel: Pair::new(Color::Indexed(7), Color::Indexed(0)),
            panel_border: Pair::new(Color::Indexed(12), Color::Indexed(0)),
            panel_title: Pair::new(Color::Indexed(12), Color::Indexed(0)).bold(),
            panel_dim: Pair::new(Color::Indexed(8), Color::Indexed(0)),
            pane_border: Pair::new(Color::Indexed(8), Color::Default),
            selection: Pair::new(Color::Indexed(0), Color::Indexed(12)).bold(),
            glyphs: Glyphs::default(),
        }
    }

    /// No colour at all, and no box drawing: the fallback that works over
    /// anything, including a terminal that lies about what it supports.
    pub fn plain() -> Self {
        Self {
            name: "plain".into(),
            bar_session: Pair::new(Color::Default, Color::Default).bold(),
            bar: Pair::new(Color::Default, Color::Default),
            tab: Pair::new(Color::Default, Color::Default),
            tab_active: Pair::new(Color::Default, Color::Default).bold(),
            bar_dim: Pair::new(Color::Default, Color::Default),
            bar_alert: Pair::new(Color::Default, Color::Default).bold(),
            command: Pair::new(Color::Default, Color::Default),
            panel: Pair::new(Color::Default, Color::Default),
            panel_border: Pair::new(Color::Default, Color::Default),
            panel_title: Pair::new(Color::Default, Color::Default).bold(),
            panel_dim: Pair::new(Color::Default, Color::Default),
            pane_border: Pair::new(Color::Default, Color::Default),
            selection: Pair::new(Color::Default, Color::Default).bold(),
            glyphs: Glyphs::ascii(),
        }
    }

    /// A theme built from six colours, which is all a status bar and a panel
    /// actually need. Every built-in below is one line of palette and nothing
    /// else, so adding another is a matter of taste rather than of plumbing.
    fn from_palette(name: &str, p: Swatches) -> Self {
        let Swatches { ink, surface, raised, text, faint, accent, warm } = p;
        Self {
            name: name.into(),
            bar_session: Pair::new(ink, accent).bold(),
            bar: Pair::new(faint, surface),
            tab: Pair::new(faint, surface),
            tab_active: Pair::new(ink, text).bold(),
            bar_dim: Pair::new(faint, surface),
            bar_alert: Pair::new(ink, warm).bold(),
            command: Pair::new(text, raised),
            panel: Pair::new(text, surface),
            panel_border: Pair::new(accent, surface),
            panel_title: Pair::new(accent, surface).bold(),
            panel_dim: Pair::new(faint, surface),
            pane_border: Pair::new(faint, Color::Default),
            selection: Pair::new(ink, accent).bold(),
            glyphs: Glyphs::default(),
        }
    }

    /// Nord: muted, low contrast, blue-grey.
    pub fn nord() -> Self {
        Self::from_palette(
            "nord",
            Swatches {
                ink: rgb(0x2e, 0x34, 0x40),
                surface: rgb(0x3b, 0x42, 0x52),
                raised: rgb(0x43, 0x4c, 0x5e),
                text: rgb(0xe5, 0xe9, 0xf0),
                faint: rgb(0x81, 0xa1, 0xc1),
                accent: rgb(0x88, 0xc0, 0xd0),
                warm: rgb(0xeb, 0xcb, 0x8b),
            },
        )
    }

    /// Gruvbox dark: warm, high contrast.
    pub fn gruvbox() -> Self {
        Self::from_palette(
            "gruvbox",
            Swatches {
                ink: rgb(0x1d, 0x20, 0x21),
                surface: rgb(0x32, 0x30, 0x2f),
                raised: rgb(0x3c, 0x38, 0x36),
                text: rgb(0xeb, 0xdb, 0xb2),
                faint: rgb(0xa8, 0x99, 0x84),
                accent: rgb(0xfa, 0xbd, 0x2f),
                warm: rgb(0xfe, 0x80, 0x19),
            },
        )
    }

    /// For a light terminal. The one case where getting the ink and the
    /// surface the wrong way round is immediately obvious.
    pub fn daylight() -> Self {
        Self::from_palette(
            "daylight",
            Swatches {
                ink: rgb(0xff, 0xff, 0xff),
                surface: rgb(0xe8, 0xe8, 0xe4),
                raised: rgb(0xd8, 0xd8, 0xd2),
                text: rgb(0x2a, 0x2a, 0x30),
                faint: rgb(0x60, 0x64, 0x70),
                accent: rgb(0x20, 0x62, 0xc0),
                warm: rgb(0xb0, 0x5c, 0x00),
            },
        )
    }

    /// Greyscale, for anyone who wants the terminal's own colours to be the
    /// only colours on screen.
    pub fn slate() -> Self {
        Self::from_palette(
            "slate",
            Swatches {
                ink: rgb(0x10, 0x10, 0x10),
                surface: rgb(0x20, 0x20, 0x20),
                raised: rgb(0x2c, 0x2c, 0x2c),
                text: rgb(0xdc, 0xdc, 0xdc),
                faint: rgb(0x8a, 0x8a, 0x8a),
                accent: rgb(0xb4, 0xb4, 0xb4),
                warm: rgb(0xff, 0xff, 0xff),
            },
        )
    }

    pub fn builtin(name: &str) -> Option<Self> {
        Some(match name {
            "haven" => Self::haven(),
            "nord" => Self::nord(),
            "gruvbox" => Self::gruvbox(),
            "daylight" => Self::daylight(),
            "slate" => Self::slate(),
            "ansi" => Self::ansi(),
            "plain" => Self::plain(),
            _ => return None,
        })
    }

    pub const BUILTIN: &'static [&'static str] =
        &["haven", "nord", "gruvbox", "daylight", "slate", "ansi", "plain"];
}

/// Accept `"#rrggbb"`, a palette index, or `"default"`. Used by whoever
/// reads a theme *file*; the wire form is the plain [`Color`] enum, because
/// postcard is not self-describing and cannot deserialize an untagged one.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("default") {
        return Some(Color::Default);
    }
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        return Some(Color::Rgb(Rgb { r, g, b }));
    }
    s.parse::<u8>().ok().map(Color::Indexed)
}

impl Default for Pair {
    fn default() -> Self {
        Self::new(Color::Default, Color::Default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_resolves_by_name() {
        for name in Theme::BUILTIN {
            let theme = Theme::builtin(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(&theme.name, name);
        }
    }

    #[test]
    fn colours_parse_in_each_accepted_form() {
        assert_eq!(parse_color("#102030"), Some(Color::Rgb(Rgb { r: 16, g: 32, b: 48 })));
        assert_eq!(parse_color("12"), Some(Color::Indexed(12)));
        assert_eq!(parse_color("default"), Some(Color::Default));
        assert_eq!(parse_color("#fff"), None, "a three-digit hex is not accepted silently");
        assert_eq!(parse_color("puce"), None);
    }

    #[test]
    fn the_plain_theme_uses_no_colour_and_no_box_drawing() {
        let t = Theme::plain();
        assert_eq!(t.bar.fg, Color::Default);
        assert_eq!(t.glyphs, Glyphs::ascii());
        assert!(t.glyphs.horizontal.is_ascii());
    }
}
