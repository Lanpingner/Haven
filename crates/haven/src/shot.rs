//! `haven screenshot`: run a program, render the result on the GPU, write the
//! pixels to a file.
//!
//! It exists for two reasons. It is the only way to see what the GPU frontend
//! draws without a display, which makes the renderer reviewable; and it is the
//! same path a windowed frame takes, so if a screenshot is right the window is
//! right (`docs/PLAN.md` §17).

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use haven_core::term::CursorShape;
use haven_pty::PtyConfig;
use haven_render::Renderer;

use crate::pump::{run_to_screen, PumpLimits};

/// Render a program's final screen to a file. Returns the image size.
pub fn capture(
    cfg: &PtyConfig,
    input: &[u8],
    idle: Duration,
    font_size: f32,
    font: Option<&str>,
    out: &Path,
) -> io::Result<(u32, u32)> {
    let limits = PumpLimits { idle, total: Duration::from_secs(30) };
    let (emu, _, _) = run_to_screen(cfg, limits, input)?;

    // The same composited screen the TUI frontend would serialize.
    let screen = haven_vtwrite::snapshot(&emu.term.grid, &emu.term.styles);
    let cursor = (emu.term.grid.cursor.row, emu.term.grid.cursor.col);

    let mut renderer =
        Renderer::offscreen(cfg.size.cols as u32, cfg.size.rows as u32, font_size, font)
            .map_err(|e| io::Error::other(e.to_string()))?;
    renderer.render(&screen, cursor, &emu.term.palette, CursorShape::Block);
    let pixels = renderer.read_pixels().map_err(|e| io::Error::other(e.to_string()))?;
    let (width, height) = renderer.size();

    write_ppm(out, &pixels, width, height)?;
    Ok((width, height))
}

/// Binary PPM: no dependencies, and every image viewer reads it.
fn write_ppm(path: &Path, rgba: &[u8], width: u32, height: u32) -> io::Result<()> {
    let mut file = io::BufWriter::new(std::fs::File::create(path)?);
    write!(file, "P6\n{width} {height}\n255\n")?;
    for pixel in rgba.chunks_exact(4) {
        file.write_all(&pixel[..3])?;
    }
    file.flush()
}
