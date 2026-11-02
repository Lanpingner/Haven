//! Box-drawing and block characters, drawn as rectangles rather than glyphs.
//!
//! Font glyphs for `U+2500`-`U+259F` are designed for the font's own em box,
//! not for our cell, so adjacent cells do not join: tables come apart into
//! dashes with gaps. Every serious terminal draws these procedurally for that
//! reason, and so do we — the arms are computed from the cell, so they always
//! meet their neighbours exactly.

use crate::font::CellMetrics;

/// A rectangle in cell-local pixels, with a coverage multiplier for shades.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h, alpha: 1.0 }
}

/// Line weights, in the sense the Unicode block uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Weight {
    None,
    Light,
    Heavy,
    Double,
}

/// Which arms a character has, and how heavy each one is.
#[derive(Clone, Copy, Debug)]
struct Arms {
    left: Weight,
    right: Weight,
    up: Weight,
    down: Weight,
}

impl Arms {
    const fn new(left: Weight, right: Weight, up: Weight, down: Weight) -> Self {
        Self { left, right, up, down }
    }
}

/// The rectangles that draw `c`, or `None` if it is not ours to draw.
pub fn rects(c: char, cell: CellMetrics) -> Option<Vec<Rect>> {
    let w = cell.width as f32;
    let h = cell.height as f32;
    // Line thickness scales with the cell so it looks right at any font size.
    let light = (cell.width as f32 / 8.0).round().max(1.0);
    let heavy = (light * 2.0).max(2.0);

    if let Some(arms) = arms_for(c) {
        return Some(draw_arms(arms, w, h, light, heavy));
    }
    block_for(c, w, h)
}

fn thickness(weight: Weight, light: f32, heavy: f32) -> f32 {
    match weight {
        Weight::None => 0.0,
        Weight::Light | Weight::Double => light,
        Weight::Heavy => heavy,
    }
}

fn draw_arms(arms: Arms, w: f32, h: f32, light: f32, heavy: f32) -> Vec<Rect> {
    let mut out = Vec::new();
    // Gap between the two strokes of a double line.
    let gap = light.max(1.0);
    let mid_x = ((w - light) / 2.0).floor();
    let mid_y = ((h - light) / 2.0).floor();

    let mut horizontal = |weight: Weight, from: f32, to: f32| {
        if weight == Weight::None {
            return;
        }
        let t = thickness(weight, light, heavy);
        if weight == Weight::Double {
            out.push(rect(from, mid_y - gap, to - from, t));
            out.push(rect(from, mid_y + gap, to - from, t));
        } else {
            out.push(rect(from, ((h - t) / 2.0).floor(), to - from, t));
        }
    };
    // Arms run from the cell edge to the centre, so two neighbours meet
    // exactly at the boundary with no seam.
    horizontal(arms.left, 0.0, (w / 2.0).ceil());
    horizontal(arms.right, (w / 2.0).floor(), w);

    let mut vertical = |weight: Weight, from: f32, to: f32| {
        if weight == Weight::None {
            return;
        }
        let t = thickness(weight, light, heavy);
        if weight == Weight::Double {
            out.push(rect(mid_x - gap, from, t, to - from));
            out.push(rect(mid_x + gap, from, t, to - from));
        } else {
            out.push(rect(((w - t) / 2.0).floor(), from, t, to - from));
        }
    };
    vertical(arms.up, 0.0, (h / 2.0).ceil());
    vertical(arms.down, (h / 2.0).floor(), h);
    out
}

/// Arm layout per character. Covers the light, heavy and double sets that
/// actually appear in terminal UIs; anything else falls back to the font.
fn arms_for(c: char) -> Option<Arms> {
    use Weight::{Double as D, Heavy as H, Light as L, None as N};
    Some(match c {
        // Lines.
        '─' => Arms::new(L, L, N, N),
        '━' => Arms::new(H, H, N, N),
        '│' => Arms::new(N, N, L, L),
        '┃' => Arms::new(N, N, H, H),
        '═' => Arms::new(D, D, N, N),
        '║' => Arms::new(N, N, D, D),
        // Corners, light.
        '┌' => Arms::new(N, L, N, L),
        '┐' => Arms::new(L, N, N, L),
        '└' => Arms::new(N, L, L, N),
        '┘' => Arms::new(L, N, L, N),
        // Corners, heavy.
        '┏' => Arms::new(N, H, N, H),
        '┓' => Arms::new(H, N, N, H),
        '┗' => Arms::new(N, H, H, N),
        '┛' => Arms::new(H, N, H, N),
        // Corners, double.
        '╔' => Arms::new(N, D, N, D),
        '╗' => Arms::new(D, N, N, D),
        '╚' => Arms::new(N, D, D, N),
        '╝' => Arms::new(D, N, D, N),
        // Tees, light.
        '├' => Arms::new(N, L, L, L),
        '┤' => Arms::new(L, N, L, L),
        '┬' => Arms::new(L, L, N, L),
        '┴' => Arms::new(L, L, L, N),
        '┼' => Arms::new(L, L, L, L),
        // Tees, heavy.
        '┣' => Arms::new(N, H, H, H),
        '┫' => Arms::new(H, N, H, H),
        '┳' => Arms::new(H, H, N, H),
        '┻' => Arms::new(H, H, H, N),
        '╋' => Arms::new(H, H, H, H),
        // Tees, double.
        '╠' => Arms::new(N, D, D, D),
        '╣' => Arms::new(D, N, D, D),
        '╦' => Arms::new(D, D, N, D),
        '╩' => Arms::new(D, D, D, N),
        '╬' => Arms::new(D, D, D, D),
        // Mixed single/double, which mc uses for its panel frames.
        '╒' => Arms::new(N, D, N, L),
        '╕' => Arms::new(D, N, N, L),
        '╘' => Arms::new(N, D, L, N),
        '╛' => Arms::new(D, N, L, N),
        '╓' => Arms::new(N, L, N, D),
        '╖' => Arms::new(L, N, N, D),
        '╙' => Arms::new(N, L, D, N),
        '╜' => Arms::new(L, N, D, N),
        '╞' => Arms::new(N, D, L, L),
        '╡' => Arms::new(D, N, L, L),
        '╤' => Arms::new(D, D, N, L),
        '╧' => Arms::new(D, D, L, N),
        '╪' => Arms::new(D, D, L, L),
        '╫' => Arms::new(L, L, D, D),
        '╟' => Arms::new(N, L, D, D),
        '╢' => Arms::new(L, N, D, D),
        '╥' => Arms::new(L, L, N, D),
        '╨' => Arms::new(L, L, D, N),
        _ => return None,
    })
}

/// Block elements: full, halves, eighths and shades.
fn block_for(c: char, w: f32, h: f32) -> Option<Vec<Rect>> {
    let eighth = |n: f32| (h * n / 8.0).round();
    let eighth_w = |n: f32| (w * n / 8.0).round();
    Some(match c {
        '█' => vec![rect(0.0, 0.0, w, h)],
        // Lower eighths.
        '▁' => vec![rect(0.0, h - eighth(1.0), w, eighth(1.0))],
        '▂' => vec![rect(0.0, h - eighth(2.0), w, eighth(2.0))],
        '▃' => vec![rect(0.0, h - eighth(3.0), w, eighth(3.0))],
        '▄' => vec![rect(0.0, h - eighth(4.0), w, eighth(4.0))],
        '▅' => vec![rect(0.0, h - eighth(5.0), w, eighth(5.0))],
        '▆' => vec![rect(0.0, h - eighth(6.0), w, eighth(6.0))],
        '▇' => vec![rect(0.0, h - eighth(7.0), w, eighth(7.0))],
        // Left eighths.
        '▏' => vec![rect(0.0, 0.0, eighth_w(1.0), h)],
        '▎' => vec![rect(0.0, 0.0, eighth_w(2.0), h)],
        '▍' => vec![rect(0.0, 0.0, eighth_w(3.0), h)],
        '▌' => vec![rect(0.0, 0.0, eighth_w(4.0), h)],
        '▋' => vec![rect(0.0, 0.0, eighth_w(5.0), h)],
        '▊' => vec![rect(0.0, 0.0, eighth_w(6.0), h)],
        '▉' => vec![rect(0.0, 0.0, eighth_w(7.0), h)],
        '▐' => vec![rect(w / 2.0, 0.0, w / 2.0, h)],
        '▀' => vec![rect(0.0, 0.0, w, (h / 2.0).round())],
        // Shades, as coverage rather than a dither pattern: at terminal cell
        // sizes a pattern aliases badly, and the alpha reads the same.
        '░' => vec![Rect { alpha: 0.25, ..rect(0.0, 0.0, w, h) }],
        '▒' => vec![Rect { alpha: 0.5, ..rect(0.0, 0.0, w, h) }],
        '▓' => vec![Rect { alpha: 0.75, ..rect(0.0, 0.0, w, h) }],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell() -> CellMetrics {
        CellMetrics { width: 10, height: 20, baseline: 15, underline_y: 17, underline_thickness: 1 }
    }

    #[test]
    fn ordinary_characters_are_left_to_the_font() {
        assert!(rects('A', cell()).is_none());
        assert!(rects(' ', cell()).is_none());
        assert!(rects('漢', cell()).is_none());
    }

    #[test]
    fn a_horizontal_rule_spans_the_whole_cell() {
        let r = rects('─', cell()).unwrap();
        let left = r.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        let right = r.iter().map(|r| r.x + r.w).fold(0.0, f32::max);
        assert_eq!(left, 0.0, "a rule must start at the cell edge");
        assert_eq!(right, 10.0, "a rule must reach the far edge, or cells show seams");
    }

    #[test]
    fn a_vertical_rule_spans_the_whole_cell() {
        let r = rects('│', cell()).unwrap();
        let top = r.iter().map(|r| r.y).fold(f32::MAX, f32::min);
        let bottom = r.iter().map(|r| r.y + r.h).fold(0.0, f32::max);
        assert_eq!(top, 0.0);
        assert_eq!(bottom, 20.0);
    }

    #[test]
    fn a_corner_only_reaches_two_edges() {
        // '┌' has arms to the right and down, nothing left or up.
        let r = rects('┌', cell()).unwrap();
        let left = r.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        let top = r.iter().map(|r| r.y).fold(f32::MAX, f32::min);
        let right = r.iter().map(|r| r.x + r.w).fold(0.0, f32::max);
        let bottom = r.iter().map(|r| r.y + r.h).fold(0.0, f32::max);
        assert!(left > 0.0, "a top-left corner must not reach the left edge");
        assert!(top > 0.0, "a top-left corner must not reach the top edge");
        assert_eq!(right, 10.0);
        assert_eq!(bottom, 20.0);
    }

    #[test]
    fn a_cross_reaches_all_four_edges() {
        let r = rects('┼', cell()).unwrap();
        assert_eq!(r.iter().map(|r| r.x).fold(f32::MAX, f32::min), 0.0);
        assert_eq!(r.iter().map(|r| r.y).fold(f32::MAX, f32::min), 0.0);
        assert_eq!(r.iter().map(|r| r.x + r.w).fold(0.0, f32::max), 10.0);
        assert_eq!(r.iter().map(|r| r.y + r.h).fold(0.0, f32::max), 20.0);
    }

    #[test]
    fn double_lines_are_drawn_as_two_strokes() {
        // Each arm is emitted separately so the halves meet at the centre,
        // so what matters is the number of distinct *bands*, not of rects.
        let bands = |c: char, horizontal: bool| {
            // Count only the strokes running the way we are asking about: a
            // cross has both, and mixing them counts crossings as bands.
            let mut values: Vec<i32> = rects(c, cell())
                .unwrap()
                .iter()
                .filter(|r| if horizontal { r.w > r.h } else { r.h > r.w })
                .map(|r| if horizontal { r.y as i32 } else { r.x as i32 })
                .collect();
            values.sort_unstable();
            values.dedup();
            values.len()
        };
        assert_eq!(bands('═', true), 2, "a double horizontal is two bands");
        assert_eq!(bands('║', false), 2, "a double vertical is two bands");
        assert_eq!(bands('╬', true), 2);
        assert_eq!(bands('╬', false), 2);
    }

    #[test]
    fn heavy_lines_are_thicker_than_light_ones() {
        let light = rects('─', cell()).unwrap()[0].h;
        let heavy = rects('━', cell()).unwrap()[0].h;
        assert!(heavy > light, "heavy {heavy} should exceed light {light}");
    }

    #[test]
    fn adjacent_cells_meet_without_a_seam() {
        // The right arm of one cell and the left arm of the next must share a
        // boundary: this is the property that makes tables look like tables.
        let c = cell();
        let a = rects('├', c).unwrap();
        let b = rects('┤', c).unwrap();
        let a_right = a.iter().map(|r| r.x + r.w).fold(0.0, f32::max);
        let b_left = b.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        assert_eq!(a_right, c.width as f32);
        assert_eq!(b_left, 0.0);
    }

    #[test]
    fn blocks_and_shades_cover_the_cell() {
        let full = rects('█', cell()).unwrap();
        assert_eq!(full.len(), 1);
        assert_eq!((full[0].w, full[0].h), (10.0, 20.0));
        assert_eq!(full[0].alpha, 1.0);

        let medium = rects('▒', cell()).unwrap();
        assert_eq!(medium[0].alpha, 0.5, "a shade is coverage, not a pattern");

        let half = rects('▄', cell()).unwrap();
        assert_eq!(half[0].h, 10.0, "a lower half block is half the cell");
        assert_eq!(half[0].y, 10.0);
    }
}
