//! The four cuts, rendered to a PPM so a person can look at them.
//!
//! Tests can prove a bold cell resolves to a different face and a different
//! image; they cannot tell you the italic looks like an italic. This is for
//! the part only eyes settle.
//!
//! `cargo run -p haven-render --example faces -- /tmp/faces.ppm`
use haven_core::term::CursorShape;
use haven_core::{blank_screen, Attrs, Palette, ScreenCell, Style};

fn main() {
    let lines: [(&str, Attrs); 4] = [
        ("regular Hamburg", Attrs::empty()),
        ("bold    Hamburg", Attrs::BOLD),
        ("italic  Hamburg", Attrs::ITALIC),
        ("both    Hamburg", Attrs::BOLD | Attrs::ITALIC),
    ];
    let (cols, rows) = (24usize, 4usize);
    let mut screen = blank_screen(cols, rows);
    for (r, (text, attrs)) in lines.iter().enumerate() {
        for (c, ch) in text.chars().enumerate().take(cols) {
            screen[r][c] =
                ScreenCell::new(ch.to_string(), Style { attrs: *attrs, ..Style::default() });
        }
    }
    let mut r =
        haven_render::Renderer::offscreen(cols as u32, rows as u32, 20.0, None).expect("offscreen");
    r.render(&screen, (0, 0), &Palette::default(), CursorShape::Block);
    let pixels = r.read_pixels().expect("readback");
    let (w, h) = r.size();
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for y in 0..h {
        for x in 0..w {
            let p = r.pixel(&pixels, x, y);
            ppm.extend_from_slice(&[p[0], p[1], p[2]]);
        }
    }
    std::fs::write(std::env::args().nth(1).expect("usage: faces <out.ppm>"), ppm).unwrap();
}
