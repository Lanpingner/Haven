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
use swash::zeno::{Format, Transform, Vector};
use swash::{CacheKey, Charmap, FontRef, GlyphId};

/// A loaded face, kept alive because `FontRef` borrows its data.
/// Font bytes, either read into memory or mapped.
///
/// Mapping matters: there is 8.6GB of fonts in 3829 faces on this machine,
/// and the fallback scan only ever looks at each one's character map. Reading
/// whole files to find a cmap cost seconds of pure syscall time; a mapping
/// faults in the few pages actually touched.
enum FontData {
    Owned(Vec<u8>),
    Mapped(memmap2::Mmap),
}

impl std::ops::Deref for FontData {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v,
            Self::Mapped(m) => m,
        }
    }
}

impl std::fmt::Debug for FontData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FontData({} bytes)", self.len())
    }
}

/// Map a font file, falling back to reading it.
fn load_font_file(path: &std::path::Path) -> Option<FontData> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: a font file changing underneath a live mapping would be
    // observed as changed bytes, not as unsoundness in our own memory; the
    // alternative is reading gigabytes to look at a few kilobytes of cmap.
    match unsafe { memmap2::Mmap::map(&file) } {
        Ok(map) => Some(FontData::Mapped(map)),
        Err(_) => std::fs::read(path).ok().map(FontData::Owned),
    }
}

#[derive(Debug)]
struct Face {
    data: Arc<FontData>,
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

/// Which cut of the primary family a cell wants.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FaceStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FaceStyle {
    pub fn of(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Self::Regular,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (true, true) => Self::BoldItalic,
        }
    }

    fn slot(self) -> Option<usize> {
        match self {
            Self::Regular => None,
            Self::Bold => Some(0),
            Self::Italic => Some(1),
            Self::BoldItalic => Some(2),
        }
    }

    fn is_italic(self) -> bool {
        matches!(self, Self::Italic | Self::BoldItalic)
    }

    fn is_bold(self) -> bool {
        matches!(self, Self::Bold | Self::BoldItalic)
    }
}

pub struct FontStack {
    faces: Vec<Face>,
    /// Bold, italic and bold-italic of the primary family, where the system
    /// has them. `None` means the family has no such cut and the renderer
    /// falls back to synthesising one.
    styled: [Option<FaceId>; 3],
    /// Kept for the last-resort scan: a curated chain cannot cover every
    /// script, and a terminal that renders nothing for CJK is broken even if
    /// the font list looks reasonable.
    db: fontdb::Database,
    /// Every system face, mapped, built once on the first miss. Keeping them
    /// is what makes the *second* unknown character free: the old code set
    /// its "already scanned" flag only when a font was found, so a character
    /// no font covers — an emoji, here — rescanned every font file on the
    /// system, every time one appeared.
    system: Vec<Face>,
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
        // Bold, italic and bold-italic of whichever family won, found by
        // asking for the same family with different attributes. They go
        // *after* the regular face in the chain so plain text still measures
        // and draws from the regular one, and they are remembered by slot so
        // a styled cell can ask for its own.
        let regular_family = ids
            .first()
            .and_then(|id| db.face(*id))
            .and_then(|f| f.families.first().map(|(name, _)| name.clone()));
        let mut styled = [None; 3];
        if let Some(family) = &regular_family {
            for (slot, weight, style) in [
                (0usize, fontdb::Weight::BOLD, fontdb::Style::Normal),
                (1, fontdb::Weight::NORMAL, fontdb::Style::Italic),
                (2, fontdb::Weight::BOLD, fontdb::Style::Italic),
            ] {
                let Some(id) = db.query(&fontdb::Query {
                    families: &[fontdb::Family::Name(family)],
                    weight,
                    stretch: fontdb::Stretch::Normal,
                    style,
                }) else {
                    continue;
                };
                // `query` falls back to the regular face when a family has no
                // such variant, so a match that *is* the regular face means
                // the variant does not exist.
                if Some(id) == ids.first().copied() {
                    continue;
                }
                styled[slot] = Some(ids.len());
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
            let data: Arc<FontData> = match source {
                fontdb::Source::Binary(data) => {
                    Arc::new(FontData::Owned(data.as_ref().as_ref().to_vec()))
                }
                fontdb::Source::File(path) => match load_font_file(&path) {
                    Some(data) => Arc::new(data),
                    None => continue,
                },
                fontdb::Source::SharedFile(_, data) => {
                    Arc::new(FontData::Owned(data.as_ref().as_ref().to_vec()))
                }
            };
            let Some(font) = FontRef::from_index(&data, index as usize) else { continue };
            let key = font.key;
            faces.push(Face { data, index, key });
        }
        if faces.is_empty() {
            return Err(FontError::NoMonospaceFont);
        }

        let metrics = measure(faces[0].as_ref(), size_px);
        // A styled face whose load failed must not leave an index pointing
        // at the wrong font.
        let styled = styled.map(|slot| slot.filter(|i| *i < faces.len()).map(|i| FaceId(i as u16)));
        Ok(Self {
            faces,
            styled,
            db,
            system: Vec::new(),
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

    /// The face that draws this character in the cut a cell asked for, and
    /// whether the cut had to be faked.
    ///
    /// A styled face is only consulted for characters the primary family
    /// covers. Falling through to it for, say, CJK would be asking a Latin
    /// italic face for a glyph it has never had.
    pub fn styled_face_for(&mut self, c: char, style: FaceStyle) -> Option<StyledGlyph> {
        let (id, glyph) = self.face_for(c)?;
        let Some(slot) = style.slot() else {
            return Some(StyledGlyph { face: id, glyph, synth_bold: false, synth_italic: false });
        };
        // Only the primary face has cuts; anything from the fallback chain
        // keeps its own shape and is slanted or doubled instead.
        if id == FaceId(0) {
            if let Some(face) = self.styled[slot] {
                let styled_glyph = self.charmap(face).map(c);
                if styled_glyph != 0 {
                    return Some(StyledGlyph {
                        face,
                        glyph: styled_glyph,
                        synth_bold: false,
                        synth_italic: false,
                    });
                }
            }
        }
        Some(StyledGlyph {
            face: id,
            glyph,
            synth_bold: style.is_bold(),
            synth_italic: style.is_italic(),
        })
    }

    /// Whether the primary family really has this cut, for tests and for
    /// anyone asking why their italics look sheared.
    pub fn has_face(&self, style: FaceStyle) -> bool {
        style.slot().is_none_or(|slot| self.styled[slot].is_some())
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
    /// chain. The system faces are mapped once; the per-character cache and
    /// the `scanned` flag keep it to a single pass however many characters
    /// turn out to be missing.
    fn scan_for(&mut self, c: char) -> Option<FaceId> {
        self.load_system_faces();
        let found = self.system.iter().position(|face| face.as_ref().charmap().map(c) != 0)?;
        // Promote it into the active chain so later characters in the same
        // script resolve without searching again.
        let face = Face {
            data: Arc::clone(&self.system[found].data),
            index: self.system[found].index,
            key: self.system[found].key,
        };
        self.faces.push(face);
        Some(FaceId((self.faces.len() - 1) as u16))
    }

    /// Map every system face once. Mapping is cheap — address space, not
    /// resident memory — and only the character maps are ever touched.
    fn load_system_faces(&mut self) {
        if self.scanned {
            return;
        }
        self.scanned = true;
        let candidates: Vec<(fontdb::Source, u32)> =
            self.db.faces().map(|face| (face.source.clone(), face.index)).collect();
        for (source, index) in candidates {
            let data: Arc<FontData> = match &source {
                fontdb::Source::Binary(data) => {
                    Arc::new(FontData::Owned(data.as_ref().as_ref().to_vec()))
                }
                fontdb::Source::File(path) => match load_font_file(path) {
                    Some(data) => Arc::new(data),
                    None => continue,
                },
                fontdb::Source::SharedFile(_, data) => {
                    Arc::new(FontData::Owned(data.as_ref().as_ref().to_vec()))
                }
            };
            let Some(font) = FontRef::from_index(&data, index as usize) else { continue };
            let key = font.key;
            self.system.push(Face { data, index, key });
        }
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
        self.rasterize_slanted(face, glyph, false)
    }

    /// As [`Self::rasterize`], but optionally sheared.
    ///
    /// A family with no italic cut gets an oblique one: the glyph is slanted
    /// as it is rasterized, which is what every terminal does and what the
    /// alternative — drawing italic text upright — fails to do at all.
    pub fn rasterize_slanted(
        &mut self,
        face: FaceId,
        glyph: GlyphId,
        slant: bool,
    ) -> Option<Raster> {
        let font = self.faces.get(face.0 as usize)?.as_ref();
        let mut builder = self.scaler.builder(font).size(self.size_px).hint(true).build();
        let scaler = &mut builder;
        // Outline first, then an embedded bitmap or colour strike: that order
        // is what gives sharp text with emoji still working.
        let mut render = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ]);
        render.format(Format::Alpha).offset(Vector::new(0.0, 0.0));
        if slant {
            // About 12 degrees, which is the slope most italic faces use.
            render.transform(Some(Transform::skew(
                swash::zeno::Angle::from_degrees(-12.0),
                swash::zeno::Angle::ZERO,
            )));
        }
        let image = render.render(scaler, glyph)?;

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

/// A glyph in the cut a cell asked for, plus what still has to be faked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StyledGlyph {
    pub face: FaceId,
    pub glyph: GlyphId,
    /// No bold cut exists, so draw it twice a pixel apart.
    pub synth_bold: bool,
    /// No italic cut exists, so shear it.
    pub synth_italic: bool,
}

#[cfg(test)]
mod style_tests {
    use super::*;

    fn stack() -> Option<FontStack> {
        FontStack::new(None, 16.0).ok()
    }

    #[test]
    fn a_cut_is_asked_for_and_either_found_or_faked() {
        let Some(mut fonts) = stack() else { return };
        let regular = fonts.styled_face_for('a', FaceStyle::Regular).expect("no regular 'a'");
        assert!(!regular.synth_bold && !regular.synth_italic);

        for style in [FaceStyle::Bold, FaceStyle::Italic, FaceStyle::BoldItalic] {
            let got = fonts.styled_face_for('a', style).expect("no glyph at all");
            if fonts.has_face(style) {
                // A real cut: a different face, and nothing to fake.
                assert_ne!(got.face, regular.face, "{style:?} resolved to the regular face");
                assert!(!got.synth_bold && !got.synth_italic, "{style:?} was faked anyway");
            } else {
                // No cut: the regular face, with the fakery flagged.
                assert_eq!(got.face, regular.face);
                assert_eq!(got.synth_bold, style.is_bold());
                assert_eq!(got.synth_italic, style.is_italic());
            }
        }
    }

    #[test]
    fn a_fallback_glyph_keeps_its_own_face_whatever_the_cut() {
        // The primary family's italic has never had a CJK glyph; asking it
        // for one would get nothing rather than a slanted character.
        let Some(mut fonts) = stack() else { return };
        let Some(plain) = fonts.styled_face_for('漢', FaceStyle::Regular) else { return };
        if plain.face == FaceId(0) {
            return; // The primary family covers it; nothing to prove here.
        }
        let italic = fonts.styled_face_for('漢', FaceStyle::Italic).expect("no glyph");
        assert_eq!(italic.face, plain.face, "a fallback glyph changed face for a cut");
        assert!(italic.synth_italic, "it should be slanted instead");
    }

    #[test]
    fn slanting_produces_a_different_image_from_the_same_glyph() {
        let Some(mut fonts) = stack() else { return };
        let Some((face, glyph)) = fonts.face_for('H') else { return };
        let upright = fonts.rasterize_slanted(face, glyph, false).expect("raster");
        let slanted = fonts.rasterize_slanted(face, glyph, true).expect("raster");
        assert_ne!(
            (upright.width, upright.alpha.clone()),
            (slanted.width, slanted.alpha.clone()),
            "the shear did nothing"
        );
    }

    #[test]
    fn the_cache_key_separates_a_slanted_glyph_from_an_upright_one() {
        // Sharing one entry would let whichever appeared first decide how
        // every later use of that character looks.
        let key = crate::atlas::GlyphKey::new(FaceId(0), 7, 16.0);
        assert_ne!(key, key.slanted(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> Option<FontStack> {
        FontStack::new(None, 16.0).ok()
    }

    #[test]
    fn a_character_no_font_has_does_not_rescan_the_system() {
        // The scan used to mark itself done only when it *found* something,
        // so a character nothing covers — an emoji on a machine with no
        // colour emoji font — re-read every font file on disk every time one
        // appeared. With 3829 faces that cost seconds per character, on the
        // render path.
        let Some(mut stack) = stack() else { return };

        // Two private-use characters: nothing can possibly cover them.
        let _ = stack.face_for('\u{F8FF}');
        assert!(stack.scanned, "the scan did not mark itself done");
        let after_first = stack.system.len();

        let _ = stack.face_for('\u{F8FE}');
        assert_eq!(
            stack.system.len(),
            after_first,
            "a second unknown character loaded the system fonts again"
        );
    }

    #[test]
    fn the_fallback_still_finds_a_face_for_an_uncovered_script() {
        let Some(mut stack) = stack() else { return };
        // Whatever the curated chain misses, the scan should still resolve —
        // speeding it up must not turn it off.
        if let Some((id, glyph)) = stack.face_for('漢') {
            assert_ne!(glyph, 0, "resolved to face {id:?} with no glyph");
        }
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
