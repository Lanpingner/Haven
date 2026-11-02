//! SGR emission: the smallest sequence that moves a terminal's pen from one
//! style to another, at whatever colour depth it supports.

use haven_core::{Attrs, Color, Palette, Role, Style, Underline};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorDepth {
    Ansi16,
    Indexed256,
    TrueColor,
}

/// Emit the transition from `from` to `to`, or an empty string if they match.
///
/// Two strategies: additive when nothing has to be switched *off*, and
/// reset-then-rebuild otherwise. A reset is 4 bytes and removes the need to
/// emit a disable for every attribute, so it usually wins the moment anything
/// turns off.
pub fn sgr_transition(
    from: &Style,
    to: &Style,
    depth: ColorDepth,
    palette: &Palette,
    underline_styles: bool,
) -> String {
    if from == to {
        return String::new();
    }
    if *to == Style::default() {
        return "\x1b[m".to_string();
    }

    let removals = from.attrs.difference(to.attrs);
    let underline_off = from.underline != Underline::None && to.underline == Underline::None;
    let mut params: Vec<String> = Vec::new();

    if !removals.is_empty() || underline_off {
        // A reset already puts everything at its default, so only the
        // non-default parts of `to` need restating. Emitting 24;39;49;59 after
        // an SGR 0 is pure byte waste, and in TUI mode bytes are the budget.
        params.push("0".into());
        push_attrs(&mut params, to.attrs);
        if to.underline != Underline::None {
            push_underline(&mut params, to.underline, underline_styles);
        }
        if to.fg != Color::Default {
            push_color(&mut params, to.fg, Role::Fg, depth, palette);
        }
        if to.bg != Color::Default {
            push_color(&mut params, to.bg, Role::Bg, depth, palette);
        }
        if to.underline_color != Color::Default {
            push_underline_color(&mut params, to.underline_color, depth, palette, underline_styles);
        }
    } else {
        push_attrs(&mut params, to.attrs.difference(from.attrs));
        if from.underline != to.underline {
            push_underline(&mut params, to.underline, underline_styles);
        }
        if from.fg != to.fg {
            push_color(&mut params, to.fg, Role::Fg, depth, palette);
        }
        if from.bg != to.bg {
            push_color(&mut params, to.bg, Role::Bg, depth, palette);
        }
        if from.underline_color != to.underline_color {
            push_underline_color(&mut params, to.underline_color, depth, palette, underline_styles);
        }
    }

    if params.is_empty() {
        return String::new();
    }
    format!("\x1b[{}m", params.join(";"))
}

fn push_attrs(params: &mut Vec<String>, attrs: Attrs) {
    for (flag, code) in [
        (Attrs::BOLD, "1"),
        (Attrs::DIM, "2"),
        (Attrs::ITALIC, "3"),
        (Attrs::BLINK, "5"),
        (Attrs::REVERSE, "7"),
        (Attrs::HIDDEN, "8"),
        (Attrs::STRIKE, "9"),
        (Attrs::OVERLINE, "53"),
    ] {
        if attrs.contains(flag) {
            params.push(code.into());
        }
    }
}

fn push_underline(params: &mut Vec<String>, underline: Underline, styles: bool) {
    let code = match underline {
        Underline::None => "24",
        Underline::Single => "4",
        // A host without 4:n gets a plain underline rather than nothing.
        Underline::Double if styles => "4:2",
        Underline::Curly if styles => "4:3",
        Underline::Dotted if styles => "4:4",
        Underline::Dashed if styles => "4:5",
        _ => "4",
    };
    params.push(code.into());
}

fn push_color(
    params: &mut Vec<String>,
    color: Color,
    role: Role,
    depth: ColorDepth,
    palette: &Palette,
) {
    let (default, base, bright, extended) = match role {
        Role::Fg => ("39", 30, 90, "38"),
        Role::Bg => ("49", 40, 100, "48"),
    };
    match color {
        Color::Default => params.push(default.into()),
        Color::Indexed(i) if i < 8 => params.push((base + i as u16).to_string()),
        Color::Indexed(i) if i < 16 => params.push((bright + (i as u16 - 8)).to_string()),
        Color::Indexed(i) => match depth {
            ColorDepth::Ansi16 => {
                let narrowed = palette.nearest_index(palette.colors[i as usize], 16);
                push_color(params, Color::Indexed(narrowed), role, depth, palette);
            }
            _ => params.push(format!("{extended};5;{i}")),
        },
        Color::Rgb(rgb) => match depth {
            ColorDepth::TrueColor => {
                params.push(format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b));
            }
            // Perceptual matching, not naive RGB distance (`docs/PLAN.md` §7.5).
            ColorDepth::Indexed256 => {
                params.push(format!("{extended};5;{}", palette.nearest_index(rgb, 256)));
            }
            ColorDepth::Ansi16 => {
                let narrowed = palette.nearest_index(rgb, 16);
                push_color(params, Color::Indexed(narrowed), role, depth, palette);
            }
        },
    }
}

fn push_underline_color(
    params: &mut Vec<String>,
    color: Color,
    depth: ColorDepth,
    palette: &Palette,
    styles: bool,
) {
    if !styles {
        return;
    }
    match color {
        Color::Default => params.push("59".into()),
        Color::Indexed(i) => params.push(format!("58;5;{i}")),
        Color::Rgb(rgb) => match depth {
            ColorDepth::TrueColor => params.push(format!("58;2;{};{};{}", rgb.r, rgb.g, rgb.b)),
            ColorDepth::Indexed256 => {
                params.push(format!("58;5;{}", palette.nearest_index(rgb, 256)))
            }
            ColorDepth::Ansi16 => params.push(format!("58;5;{}", palette.nearest_index(rgb, 16))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_core::Rgb;

    fn t(from: &Style, to: &Style) -> String {
        sgr_transition(from, to, ColorDepth::TrueColor, &Palette::default(), true)
            .replace('\x1b', "ESC")
    }

    fn bold() -> Style {
        Style { attrs: Attrs::BOLD, ..Default::default() }
    }

    #[test]
    fn no_change_emits_nothing() {
        assert_eq!(t(&bold(), &bold()), "");
    }

    #[test]
    fn additions_are_emitted_without_a_reset() {
        let to = Style { attrs: Attrs::BOLD | Attrs::ITALIC, ..Default::default() };
        assert_eq!(t(&bold(), &to), "ESC[3m", "only the new attribute is needed");
    }

    #[test]
    fn turning_something_off_resets_and_rebuilds() {
        let from = Style { attrs: Attrs::BOLD | Attrs::ITALIC, ..Default::default() };
        // One reset plus one re-add beats emitting a disable per attribute.
        assert_eq!(t(&from, &bold()), "ESC[0;1m");
    }

    #[test]
    fn returning_to_default_is_a_bare_reset() {
        assert_eq!(t(&bold(), &Style::default()), "ESC[m");
    }

    #[test]
    fn colours_use_the_short_ansi_forms_where_they_exist() {
        let red = Style { fg: Color::Indexed(1), ..Default::default() };
        assert_eq!(t(&Style::default(), &red), "ESC[31m");
        let bright = Style { fg: Color::Indexed(9), ..Default::default() };
        assert_eq!(t(&Style::default(), &bright), "ESC[91m");
        let cube = Style { fg: Color::Indexed(200), ..Default::default() };
        assert_eq!(t(&Style::default(), &cube), "ESC[38;5;200m");
    }

    #[test]
    fn truecolor_is_emitted_verbatim() {
        let style = Style { fg: Color::Rgb(Rgb::new(1, 2, 3)), ..Default::default() };
        assert_eq!(t(&Style::default(), &style), "ESC[38;2;1;2;3m");
    }

    #[test]
    fn a_256_colour_host_gets_the_nearest_indexed_match() {
        let style = Style { bg: Color::Rgb(Rgb::new(255, 0, 0)), ..Default::default() };
        let out = sgr_transition(
            &Style::default(),
            &style,
            ColorDepth::Indexed256,
            &Palette::default(),
            true,
        );
        assert!(out.starts_with("\x1b[48;5;"), "{out:?}");
        assert!(!out.contains("48;2"), "truecolor must not reach a 256-colour host");
    }

    #[test]
    fn a_16_colour_host_gets_an_ansi_code_not_an_index() {
        let style = Style { fg: Color::Rgb(Rgb::new(250, 5, 5)), ..Default::default() };
        let out = sgr_transition(
            &Style::default(),
            &style,
            ColorDepth::Ansi16,
            &Palette::default(),
            true,
        );
        assert!(out == "\x1b[31m" || out == "\x1b[91m", "{out:?}");
    }

    #[test]
    fn underline_styles_degrade_to_a_plain_underline() {
        let curly = Style { underline: Underline::Curly, ..Default::default() };
        assert_eq!(t(&Style::default(), &curly), "ESC[4:3m");
        let plain = sgr_transition(
            &Style::default(),
            &curly,
            ColorDepth::TrueColor,
            &Palette::default(),
            false,
        );
        assert_eq!(plain, "\x1b[4m", "a host without 4:n still gets an underline");
    }

    #[test]
    fn removing_an_underline_resets() {
        let curly = Style { underline: Underline::Curly, ..Default::default() };
        assert_eq!(t(&curly, &Style::default()), "ESC[m");
        let bold_only = bold();
        // No 24 needed: the reset already cleared the underline.
        assert_eq!(t(&curly, &bold_only), "ESC[0;1m");
    }
}
