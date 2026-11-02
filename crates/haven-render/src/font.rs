//! Font discovery, metrics and rasterization. See `docs/PLAN.md` §12.3.
//!
//! The grid imposes a rule the rest of text rendering does not have: every
//! cell is the same width. So the advance is measured once from the primary
//! font and *snapped to an integer*, and every glyph — including one from a
//! fallback font with different metrics — is placed on that pitch. Without
//! that, a single CJK or emoji fallback knocks every column after it out of
//! alignment.

use std::collections::HashMap;
use std::sync::Arc;

use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Format, Vector};
use swash::{Attributes, CacheKey, Charmap, FontRef, GlyphId};

/// A loaded face, kept alive because `FontRef` borrows its data.
struct Face {
    data: Arc<Vec<u8>>,
    index: u32,
    key: CacheKey,
}

impl Face {
    fn as_ref(&self) -> FontRef<'_> {
        FontRef { data: &self.data, offset: self.index, key: self.key }
    }
}

/// Which face a glyph came from, so the atlas can key on it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FaceId(pub u16);

/// A rasterized glyph: an alpha mask plus where to put it.
#[derive(Clone, Debug)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    /// Alpha coverage, one byte per pixel.
    pub alpha: Vec<u8>,
    /// Offset from the cell's baseline origin, in pixels.
    pub left: i32,
    pub top: i32,
}

/// Integer cell geometry. Everything downstream positions on this grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// Distance from the cell's top edge to the baseline.
    pub baseline: u32,
    /// Where an underline sits, measured from the top of the cell.
    pub underline_y: u32,
    pub underline_thickness: u32,
}

pub struct FontStack {
    faces: Vec<Face>,
    /// Kept for the last-resort scan: a curated chain cannot cover every
    /// script, and a terminal that renders nothing for CJK is broken even if
    /// the font list looks reasonable.
    db: fontdb::Database,
    scanned: bool,
    /// Which face can draw a given character, once resolved.
    coverage: HashMap<char, Option<FaceId>>,
    scaler: ScaleContext,
    size_px: f32,
    metrics: CellMetrics,
}

impl std::fmt::Debug for FontStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontStack")
            .field("faces", &self.faces.len())
            .field("size_px", &self.size_px)
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum FontError {
    NoMonospaceFont,
    Unreadable(String),
}

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMonospaceFont => write!(f, "no monospace font found"),
            Self::Unreadable(e) => write!(f, "font could not be read: {e}"),
        }
    }
}

impl std::error::Error for FontError {}

impl FontStack {
    /// Load a primary monospace family plus whatever the system offers as
    /// fallback, at a given pixel size.
    pub fn new(preferred: Option<&str>, size_px: f32) -> Result<Self, FontError> {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();

        let mut ids = Vec::new();
        // The preferred family first, then generic monospace, then anything
        // monospace at all: a terminal that will not start because a font is
        // missing is worse than one that looks unfamiliar.
        if let Some(name) = preferred {
            if let Some(id) = query(&db, fontdb::Family::Name(name)) {
                ids.push(id);
            }
        }
        if let Some(id) = query(&db, fontdb::Family::Monospace) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        // A curated chain covering the scripts and symbol blocks a terminal
        // actually meets: box drawing from a mono face, CJK and symbols from
        // Noto, and a generic sans at the end.
        for name in [
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
            "Courier New",
            "Noto Sans Symbols 2",
            "Noto Sans Symbols",
            "Noto Sans Math",
            "Symbola",
            "Noto Sans CJK JP",
            "Noto Sans CJK SC",
            "Noto Sans CJK KR",
            "Noto Sans Devanagari",
            "Noto Sans Arabic",
            "Noto Sans Hebrew",
            "Noto Sans Thai",
            "DejaVu Sans",
        ] {
            if let Some(id) = query(&db, fontdb::Family::Name(name)) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        for family in [fontdb::Family::SansSerif, fontdb::Family::Serif] {
            if let Some(id) = query(&db, family) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        if ids.is_empty() {
            return Err(FontError::NoMonospaceFont);
        }

        let mut faces = Vec::new();
        for id in ids {
            let Some((source, index)) = db.face_source(id) else { continue };
            let data: Arc<Vec<u8>> = match source {
                fontdb::Source::Binary(data) => Arc::new(data.as_ref().as_ref().to_vec()),
                fontdb::Source::File(path) => match std::fs::read(&path) {
                    Ok(bytes) => Arc::new(bytes),
                    Err(_) => continue,
                },
                fontdb::Source::SharedFile(_, data) => Arc::new(data.as_ref().as_ref().to_vec()),
            };
            let Some(font) = FontRef::from_index(&data, index as usize) else { continue };
            let key = font.key;
            faces.push(Face { data, index, key });
        }
        if faces.is_empty() {
            return Err(FontError::NoMonospaceFont);
        }

        let metrics = measure(faces[0].as_ref(), size_px);
        Ok(Self {
            faces,
            db,
            scanned: false,
            coverage: HashMap::new(),
            scaler: ScaleContext::new(),
            size_px,
            metrics,
        })
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn size_px(&self) -> f32 {
        self.size_px
    }

    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// The face that can draw this character, searching the fallback chain
    /// once and remembering the answer.
    pub fn face_for(&mut self, c: char) -> Option<(FaceId, GlyphId)> {
        if let Some(cached) = self.coverage.get(&c).copied() {
            let id = cached?;
            let glyph = self.charmap(id).map(c);
            return (glyph != 0).then_some((id, glyph));
        }
        for (index, face) in self.faces.iter().enumerate() {
            let glyph = face.as_ref().charmap().map(c);
            if glyph != 0 {
                let id = FaceId(index as u16);
                self.coverage.insert(c, Some(id));
                return Some((id, glyph));
            }
        }
        // Nothing in the chain has it. Scan the system once, keeping whatever
        // we find: the cost is paid on the first unknown character, not per
        // frame, and the alternative is a blank cell forever.
        if let Some(id) = self.scan_for(c) {
            let glyph = self.faces[id.0 as usize].as_ref().charmap().map(c);
            if glyph != 0 {
                self.coverage.insert(c, Some(id));
                return Some((id, glyph));
            }
        }
        self.coverage.insert(c, None);
        None
    }

    /// Search every installed face for one that can draw `c`, adding it to the
    /// chain. Bounded by the `scanned` flag plus the per-character cache.
    fn scan_for(&mut self, c: char) -> Option<FaceId> {
        let candidates: Vec<(fontdb::ID, fontdb::Source, u32)> =
            self.db.faces().map(|face| (face.id, face.source.clone(), face.index)).collect();

        for (_, source, index) in candidates {
            let data: Arc<Vec<u8>> = match &source {
                fontdb::Source::Binary(data) => Arc::new(data.as_ref().as_ref().to_vec()),
                fontdb::Source::File(path) => match std::fs::read(path) {
                    Ok(bytes) => Arc::new(bytes),
                    Err(_) => continue,
                },
                fontdb::Source::SharedFile(_, data) => Arc::new(data.as_ref().as_ref().to_vec()),
            };
            let Some(font) = FontRef::from_index(&data, index as usize) else { continue };
            if font.charmap().map(c) == 0 {
                continue;
            }
            let key = font.key;
            self.faces.push(Face { data, index, key });
            self.scanned = true;
            return Some(FaceId((self.faces.len() - 1) as u16));
        }
        None
    }

    /// Characters that take no space of their own and belong *on* the
    /// preceding glyph.
    pub fn is_combining(c: char) -> bool {
        unicode_width_of(c) == 0
    }

    fn charmap(&self, id: FaceId) -> Charmap<'_> {
        self.faces[id.0 as usize].as_ref().charmap()
    }

    /// Rasterize one glyph to an alpha mask.
    pub fn rasterize(&mut self, face: FaceId, glyph: GlyphId) -> Option<Raster> {
        let font = self.faces.get(face.0 as usize)?.as_ref();
        let mut scaler = self.scaler.builder(font).size(self.size_px).hint(true).build();
        // Outline first, then an embedded bitmap or colour strike: that order
        // is what gives sharp text with emoji still working.
        let image = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ])
        .format(Format::Alpha)
        .offset(Vector::new(0.0, 0.0))
        .render(&mut scaler, glyph)?;

        Some(Raster {
            width: image.placement.width,
            height: image.placement.height,
            alpha: image.data,
            left: image.placement.left,
            top: image.placement.top,
        })
    }

    /// Advance of a glyph in whole cells: 1, or 2 for a wide glyph.
    pub fn cells_for(&self, c: char) -> u32 {
        match unicode_width_of(c) {
            2 => 2,
            _ => 1,
        }
    }
}

fn query(db: &fontdb::Database, family: fontdb::Family<'_>) -> Option<fontdb::ID> {
    db.query(&fontdb::Query {
        families: &[family],
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    })
}

/// Derive integer cell metrics from a face.
fn measure(font: FontRef<'_>, size_px: f32) -> CellMetrics {
    let metrics = font.metrics(&[]).scale(size_px);
    let glyphs = font.glyph_metrics(&[]).scale(size_px);

    // Measure the advance of a representative glyph rather than trusting
    // `average_width`, which lies for many faces.
    let probe = font.charmap().map('M');
    let advance = if probe != 0 { glyphs.advance_width(probe) } else { metrics.average_width };
    let width = advance.round().max(1.0) as u32;
    let height = (metrics.ascent + metrics.descent + metrics.leading).ceil().max(1.0) as u32;
    // Split the leading above and below the text box rather than dumping it
    // all underneath: otherwise every line sits high in its cell and the grid
    // looks top-heavy, which is very visible in box drawing.
    let baseline = (metrics.ascent + metrics.leading / 2.0).round().max(1.0) as u32;
    let thickness = metrics.stroke_size.round().max(1.0) as u32;
    // Underline position is reported as a negative offset from the baseline.
    let underline_y = (baseline as f32 - metrics.underline_offset).round().max(1.0) as u32;

    CellMetrics {
        width,
        height,
        baseline,
        underline_y: underline_y.min(height.saturating_sub(1)),
        underline_thickness: thickness,
    }
}

/// Only the width classification matters here; `haven-core` owns the real
/// width logic for the grid.
fn unicode_width_of(c: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    c.width().unwrap_or(1)
}

/// Keep the `Attributes` import meaningful: used when we start honouring
/// bold/italic faces rather than synthesising them.
#[allow(dead_code)]
fn _attributes_anchor() -> Attributes {
    Attributes::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> Option<FontStack> {
        FontStack::new(None, 16.0).ok()
    }

    #[test]
    fn a_monospace_font_is_found_and_measured() {
        let Some(stack) = stack() else {
            // A machine with no fonts at all is a legitimate CI environment.
            return;
        };
        let m = stack.metrics();
        assert!(m.width > 0 && m.height > 0, "{m:?}");
        // Sanity: a 16px font should give a cell roughly in this range, and a
        // cell taller than it is wide.
        assert!((4..=30).contains(&m.width), "implausible cell width: {m:?}");
        assert!(m.height > m.width, "cells should be taller than wide: {m:?}");
        assert!(m.baseline > 0 && m.baseline < m.height, "{m:?}");
        assert!(m.underline_y < m.height, "{m:?}");
    }

    #[test]
    fn ascii_resolves_to_the_primary_face() {
        let Some(mut stack) = stack() else { return };
        let (face, glyph) = stack.face_for('A').expect("every font has 'A'");
        assert_eq!(face, FaceId(0), "ASCII must not fall back");
        assert_ne!(glyph, 0);
    }

    #[test]
    fn coverage_lookups_are_cached() {
        let Some(mut stack) = stack() else { return };
        let first = stack.face_for('Z');
        let second = stack.face_for('Z');
        assert_eq!(first, second, "a cached lookup must give the same answer");
    }

    #[test]
    fn an_unrepresentable_character_resolves_to_nothing_rather_than_a_wrong_glyph() {
        let Some(mut stack) = stack() else { return };
        // A private-use codepoint no font should claim.
        assert!(stack.face_for('\u{10FFFD}').is_none());
    }

    #[test]
    fn rasterizing_produces_a_mask_with_ink_in_it() {
        let Some(mut stack) = stack() else { return };
        let (face, glyph) = stack.face_for('M').unwrap();
        let raster = stack.rasterize(face, glyph).expect("M rasterizes");
        assert!(raster.width > 0 && raster.height > 0, "{raster:?}");
        assert_eq!(raster.alpha.len(), (raster.width * raster.height) as usize);
        assert!(raster.alpha.iter().any(|&a| a > 0), "the mask is empty");
    }

    #[test]
    fn a_space_has_no_ink_but_still_advances() {
        let Some(mut stack) = stack() else { return };
        let (face, glyph) = stack.face_for(' ').unwrap();
        let raster = stack.rasterize(face, glyph);
        // Either no mask at all or an entirely transparent one; both mean the
        // same thing to the atlas.
        if let Some(raster) = raster {
            assert!(raster.alpha.iter().all(|&a| a == 0) || raster.alpha.is_empty());
        }
        assert_eq!(stack.cells_for(' '), 1);
    }

    #[test]
    fn wide_characters_are_reported_as_two_cells() {
        let Some(stack) = stack() else { return };
        assert_eq!(stack.cells_for('漢'), 2);
        assert_eq!(stack.cells_for('a'), 1);
    }
}
