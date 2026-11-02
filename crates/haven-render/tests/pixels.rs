//! The renderer checked by reading its own pixels back.
//!
//! `docs/PLAN.md` §17 asks for headless wgpu with image comparison; this is
//! that, phrased as assertions about what must be true of the image rather
//! than as golden files, so the tests survive a font change.

use haven_core::term::CursorShape;
use haven_core::{blank_screen, Attrs, Color, Palette, Rgb, Screen, ScreenCell, Style, Underline};
use haven_render::Renderer;

/// Skip rather than fail where there is no GPU or no font: both are legitimate
/// CI environments, and a missing adapter is not a bug in the renderer.
fn renderer(cols: u32, rows: u32) -> Option<Renderer> {
    match Renderer::offscreen(cols, rows, 16.0, None) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

fn text_screen(cols: usize, rows: usize, lines: &[&str]) -> Screen {
    let mut screen = blank_screen(cols, rows);
    for (r, line) in lines.iter().enumerate().take(rows) {
        for (c, ch) in line.chars().enumerate().take(cols) {
            screen[r][c] = ScreenCell::new(ch.to_string(), Style::default());
        }
    }
    screen
}

fn close(a: [u8; 4], b: Rgb) -> bool {
    let d = |x: u8, y: u8| (x as i32 - y as i32).abs() <= 3;
    d(a[0], b.r) && d(a[1], b.g) && d(a[2], b.b)
}

/// Mean brightness of a cell. Sampling a single pixel is unreliable: a corner
/// may land inside the glyph, which is exactly what "is this cell filled?"
/// is not asking about.
fn cell_mean(r: &Renderer, pixels: &[u8], col: u32, row: u32) -> f32 {
    let cell = r.cell_metrics();
    let (x0, y0) = (col * cell.width, row * cell.height);
    let mut total = 0f32;
    let mut count = 0f32;
    for y in y0..(y0 + cell.height).min(r.size().1) {
        for x in x0..(x0 + cell.width).min(r.size().0) {
            let p = r.pixel(pixels, x, y);
            total += (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
            count += 1.0;
        }
    }
    if count == 0.0 {
        0.0
    } else {
        total / count
    }
}

/// Darkest pixel in a cell, for "is the glyph visible against its fill?".
fn cell_darkest(r: &Renderer, pixels: &[u8], col: u32, row: u32) -> u8 {
    let cell = r.cell_metrics();
    let (x0, y0) = (col * cell.width, row * cell.height);
    let mut darkest = 255u8;
    for y in y0..(y0 + cell.height).min(r.size().1) {
        for x in x0..(x0 + cell.width).min(r.size().0) {
            darkest = darkest.min(r.pixel(pixels, x, y)[0]);
        }
    }
    darkest
}

/// Does any pixel in this cell differ from the background?
fn cell_has_ink(r: &Renderer, pixels: &[u8], col: u32, row: u32, bg: Rgb) -> bool {
    let cell = r.cell_metrics();
    let (x0, y0) = (col * cell.width, row * cell.height);
    for y in y0..(y0 + cell.height).min(r.size().1) {
        for x in x0..(x0 + cell.width).min(r.size().0) {
            if !close(r.pixel(pixels, x, y), bg) {
                return true;
            }
        }
    }
    false
}

#[test]
fn a_blank_screen_is_the_background_colour_everywhere() {
    let Some(mut r) = renderer(10, 3) else { return };
    let palette = Palette::default();
    r.render(&blank_screen(10, 3), (99, 99), &palette, CursorShape::Block);
    let pixels = r.read_pixels().expect("readback");

    let (w, h) = r.size();
    assert_eq!(pixels.len(), (w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let got = r.pixel(&pixels, x, y);
            assert!(
                close(got, palette.bg),
                "pixel ({x},{y}) is {got:?}, expected the background {:?}",
                palette.bg
            );
        }
    }
    // The default background is the clear colour, so it costs no instances.
    assert_eq!(r.instance_counts(), (0, 0), "a blank screen should draw nothing");
}

#[test]
fn a_glyph_puts_ink_in_its_own_cell_and_nowhere_else() {
    let Some(mut r) = renderer(6, 2) else { return };
    let palette = Palette::default();
    // 'M' in the first cell, spaces everywhere else.
    r.render(&text_screen(6, 2, &["M"]), (99, 99), &palette, CursorShape::Block);
    let pixels = r.read_pixels().unwrap();

    assert!(cell_has_ink(&r, &pixels, 0, 0, palette.bg), "the glyph did not render at all");
    for col in 1..6 {
        assert!(!cell_has_ink(&r, &pixels, col, 0, palette.bg), "ink leaked into cell ({col},0)");
    }
    assert!(!cell_has_ink(&r, &pixels, 0, 1, palette.bg), "ink leaked onto the next row");
}

#[test]
fn text_is_drawn_in_its_foreground_colour() {
    let Some(mut r) = renderer(4, 1) else { return };
    let palette = Palette::default();
    let mut screen = blank_screen(4, 1);
    let red = Rgb::new(255, 0, 0);
    screen[0][0] = ScreenCell::new("M", Style { fg: Color::Rgb(red), ..Default::default() });
    r.render(&screen, (99, 99), &palette, CursorShape::Block);
    let pixels = r.read_pixels().unwrap();

    // The glyph's darkest pixels should be close to pure red: more red than
    // anything else, and nothing like the background.
    let cell = r.cell_metrics();
    let mut best = [0u8; 4];
    for y in 0..cell.height {
        for x in 0..cell.width {
            let p = r.pixel(&pixels, x, y);
            if p[0] > best[0] {
                best = p;
            }
        }
    }
    assert!(best[0] > 150, "no red ink found, brightest was {best:?}");
    assert!(best[0] > best[1] + 60 && best[0] > best[2] + 60, "ink is not red: {best:?}");
}

#[test]
fn a_background_colour_fills_exactly_one_cell() {
    let Some(mut r) = renderer(4, 1) else { return };
    let palette = Palette::default();
    let blue = Rgb::new(0, 0, 255);
    let mut screen = blank_screen(4, 1);
    screen[0][1] = ScreenCell::new(" ", Style { bg: Color::Rgb(blue), ..Default::default() });
    r.render(&screen, (99, 99), &palette, CursorShape::Block);
    let pixels = r.read_pixels().unwrap();

    let cell = r.cell_metrics();
    let mid_y = cell.height / 2;
    assert!(close(r.pixel(&pixels, cell.width + 1, mid_y), blue), "the cell is not blue");
    assert!(close(r.pixel(&pixels, 1, mid_y), palette.bg), "colour bled left");
    assert!(close(r.pixel(&pixels, cell.width * 2 + 1, mid_y), palette.bg), "colour bled right");
}

#[test]
fn adjacent_cells_with_the_same_background_merge_into_one_instance() {
    let Some(mut r) = renderer(10, 1) else { return };
    let palette = Palette::default();
    let green = Rgb::new(0, 128, 0);
    let mut screen = blank_screen(10, 1);
    for cell in screen[0].iter_mut() {
        *cell = ScreenCell::new(" ", Style { bg: Color::Rgb(green), ..Default::default() });
    }
    r.render(&screen, (99, 99), &palette, CursorShape::Block);
    let (solid, _) = r.instance_counts();
    assert_eq!(solid, 1, "ten identical backgrounds should be one quad, not ten");
}

#[test]
fn a_block_cursor_inverts_its_cell() {
    let Some(mut r) = renderer(4, 1) else { return };
    let palette = Palette::default();
    r.render(&text_screen(4, 1, &["AB"]), (0, 0), &palette, CursorShape::Block);
    let pixels = r.read_pixels().unwrap();

    // The cursor cell is mostly cursor-coloured...
    let filled = cell_mean(&r, &pixels, 0, 0);
    let plain = cell_mean(&r, &pixels, 2, 0);
    assert!(filled > 120.0, "the cursor cell was not filled (mean {filled})");
    assert!(filled > plain + 80.0, "cursor cell {filled} vs ordinary cell {plain}");
    // ...and the glyph inside it is drawn dark, so it stays readable.
    let darkest = cell_darkest(&r, &pixels, 0, 0);
    assert!(darkest < 100, "the glyph inside the cursor is invisible (darkest {darkest})");
}

#[test]
fn reverse_video_swaps_foreground_and_background() {
    let Some(mut r) = renderer(4, 1) else { return };
    let palette = Palette::default();
    let mut screen = blank_screen(4, 1);
    screen[0][0] = ScreenCell::new("M", Style { attrs: Attrs::REVERSE, ..Default::default() });
    r.render(&screen, (99, 99), &palette, CursorShape::Block);
    let pixels = r.read_pixels().unwrap();

    // With fg and bg swapped the cell is mostly light, and the glyph inside it
    // is dark — the inverse of how it renders normally.
    let reversed = cell_mean(&r, &pixels, 0, 0);
    let normal = cell_mean(&r, &pixels, 2, 0);
    assert!(reversed > 120.0, "reverse did not fill the cell (mean {reversed})");
    assert!(reversed > normal + 80.0, "reversed {reversed} vs plain {normal}");
    assert!(cell_darkest(&r, &pixels, 0, 0) < 100, "no dark glyph inside the reversed cell");
    let _ = palette.fg;
}

#[test]
fn an_underline_is_drawn_near_the_baseline() {
    let Some(mut r) = renderer(4, 1) else { return };
    let palette = Palette::default();
    let mut screen = blank_screen(4, 1);
    screen[0][0] =
        ScreenCell::new(" ", Style { underline: Underline::Single, ..Default::default() });
    r.render(&screen, (99, 99), &palette, CursorShape::Block);
    let pixels = r.read_pixels().unwrap();

    let cell = r.cell_metrics();
    // Ink on the underline row, nothing on the row above the glyph area.
    let on_line = r.pixel(&pixels, 1, cell.underline_y);
    assert!(!close(on_line, palette.bg), "no underline at y={}", cell.underline_y);
    assert!(close(r.pixel(&pixels, 1, 0), palette.bg), "the underline is in the wrong place");
}

#[test]
fn repeated_characters_rasterize_once() {
    let Some(mut r) = renderer(20, 2) else { return };
    let palette = Palette::default();
    r.render(
        &text_screen(20, 2, &["aaaaaaaaaa", "aaaaaaaaaa"]),
        (99, 99),
        &palette,
        CursorShape::Block,
    );
    assert_eq!(r.atlas.len(), 1, "twenty 'a's should share one atlas entry");
    let (_, glyphs) = r.instance_counts();
    assert_eq!(glyphs, 20, "each cell still needs its own quad");
}

#[test]
fn resizing_changes_the_surface_and_still_renders() {
    let Some(mut r) = renderer(4, 2) else { return };
    let palette = Palette::default();
    let small = r.size();
    r.resize(12, 4);
    let large = r.size();
    assert!(large.0 > small.0 && large.1 > small.1, "{small:?} -> {large:?}");

    r.render(&text_screen(12, 4, &["after resize"]), (99, 99), &palette, CursorShape::Block);
    let pixels = r.read_pixels().unwrap();
    assert_eq!(pixels.len(), (large.0 * large.1 * 4) as usize);
    assert!(cell_has_ink(&r, &pixels, 0, 0, palette.bg), "nothing rendered after resize");
}

#[test]
fn a_full_screen_of_text_renders_in_one_frame_per_pass() {
    let Some(mut r) = renderer(80, 24) else { return };
    let palette = Palette::default();
    let line: String = "the quick brown fox jumps over the lazy dog 0123456789".into();
    let lines: Vec<&str> = (0..24).map(|_| line.as_str()).collect();
    r.render(&text_screen(80, 24, &lines), (0, 0), &palette, CursorShape::Block);
    let pixels = r.read_pixels().unwrap();

    // Ink on every row, and the atlas holds one entry per distinct character.
    for row in 0..24 {
        assert!(cell_has_ink(&r, &pixels, 2, row, palette.bg), "row {row} is empty");
    }
    let distinct: std::collections::HashSet<char> = line.chars().filter(|c| *c != ' ').collect();
    assert!(
        r.atlas.len() <= distinct.len() + 1,
        "atlas holds {} entries for {} distinct characters",
        r.atlas.len(),
        distinct.len()
    );
    assert_eq!(r.frames, 1);
}

#[test]
fn a_grid_larger_than_the_device_allows_is_cropped_not_fatal() {
    // Asking for a surface or texture bigger than `max_texture_dimension_2d`
    // is a validation error, and a validation error aborts the process — it
    // cannot be caught. A window is whatever size the user dragged it to, so
    // the size has to be clamped rather than hoped about.
    //
    // This bit on a real window: requesting `downlevel_defaults` limits caps
    // textures at 2048 whatever the hardware can do, and a 2088-pixel-tall
    // window could not be given a surface at all.
    let Some(mut r) = renderer(4000, 4000) else { return };
    let (w, h) = r.size();
    let max = r.max_dimension();
    assert!(w >= 1 && h >= 1, "clamped to nothing: {w}x{h}");
    assert!(w <= max && h <= max, "{w}x{h} exceeds the device limit of {max}");

    // It must still draw, not merely survive being constructed.
    let screen = text_screen(4, 1, &["ok"]);
    r.render(&screen, (0, 0), &haven_core::Palette::default(), CursorShape::Block);
    assert!(r.read_pixels().is_ok(), "a clamped renderer could not read back");
}
