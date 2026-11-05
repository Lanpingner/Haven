//! The glyph atlas. See `docs/PLAN.md` §12.2.
//!
//! A shelf packer over one RGBA texture. Glyphs are keyed by face, glyph id
//! and pixel size, so a resize re-rasterizes but a repeated character never
//! does — which is what keeps a full-screen repaint from costing thousands of
//! rasterizations.
//!
//! RGBA rather than coverage alone because a colour glyph — an emoji, or any
//! `COLR` face — brings its own colours, and a second texture for those would
//! mean a second bind group and a second draw call in the middle of a line of
//! text. A coverage mask is stored as white with the coverage as alpha, so
//! the shader's only question is whether to tint.

use std::collections::HashMap;

use crate::font::{FaceId, Raster};

/// One pixel of padding around every glyph, so linear sampling at a glyph's
/// edge cannot pick up its neighbour.
const PADDING: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GlyphKey {
    pub face: FaceId,
    pub glyph: u16,
    /// Pixel size, as bits so it can be hashed.
    pub size_bits: u32,
    /// Sheared, for a family with no italic cut. Part of the key because the
    /// same glyph from the same face at the same size is a *different image*
    /// slanted, and sharing one cache entry would make the first use of a
    /// character decide how every later one looks.
    pub slanted: bool,
}

impl GlyphKey {
    pub fn new(face: FaceId, glyph: u16, size_px: f32) -> Self {
        Self { face, glyph, size_bits: size_px.to_bits(), slanted: false }
    }

    pub fn slanted(mut self, slanted: bool) -> Self {
        self.slanted = slanted;
        self
    }
}

/// Where a glyph lives in the atlas and how to place it in a cell.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct GlyphEntry {
    /// Normalized texture coordinates: `[u0, v0, u1, v1]`.
    pub uv: [f32; 4],
    pub width: u32,
    pub height: u32,
    /// Offset from the pen position, in pixels.
    pub left: i32,
    pub top: i32,
    /// The glyph carries its own colours and must not be tinted.
    pub color: bool,
}

impl GlyphEntry {
    /// A glyph with no ink — a space, say. It still occupies a cell.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// A region of the texture waiting to be uploaded.
#[derive(Clone, Debug)]
pub struct Upload {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// RGBA, four bytes per pixel.
    pub rgba: Vec<u8>,
}

struct Shelf {
    y: u32,
    height: u32,
    cursor_x: u32,
}

#[derive(Debug)]
pub struct Atlas {
    size: u32,
    shelves: Vec<Shelf>,
    next_y: u32,
    map: HashMap<GlyphKey, GlyphEntry>,
    pending: Vec<Upload>,
    /// Bumped whenever the atlas is reset, so callers know their cached
    /// entries are stale.
    generation: u32,
}

impl std::fmt::Debug for Shelf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shelf")
            .field("y", &self.y)
            .field("height", &self.height)
            .field("cursor_x", &self.cursor_x)
            .finish()
    }
}

impl Atlas {
    pub fn new(size: u32) -> Self {
        Self {
            size: size.max(64),
            shelves: Vec::new(),
            next_y: 0,
            map: HashMap::new(),
            pending: Vec::new(),
            generation: 0,
        }
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn get(&self, key: &GlyphKey) -> Option<GlyphEntry> {
        self.map.get(key).copied()
    }

    /// Insert a rasterized glyph, or return the entry it already has.
    pub fn insert(&mut self, key: GlyphKey, raster: &Raster) -> GlyphEntry {
        if let Some(entry) = self.map.get(&key) {
            return *entry;
        }
        // A glyph with no ink needs no space, only an entry.
        if raster.width == 0 || raster.height == 0 {
            let entry = GlyphEntry {
                left: raster.left,
                top: raster.top,
                color: raster.color,
                ..Default::default()
            };
            self.map.insert(key, entry);
            return entry;
        }

        let (x, y) = match self.allocate(raster.width, raster.height) {
            Some(spot) => spot,
            None => {
                // Full. Resetting is crude but bounded; an LRU with a
                // per-frame pin set is the planned refinement, and it only
                // matters once a single screen holds thousands of distinct
                // glyphs.
                self.reset();
                match self.allocate(raster.width, raster.height) {
                    Some(spot) => spot,
                    None => {
                        // Genuinely larger than the whole atlas. Skip the
                        // glyph: a missing character is a cosmetic problem,
                        // and panicking here would take down the renderer.
                        let entry = GlyphEntry {
                            left: raster.left,
                            top: raster.top,
                            color: raster.color,
                            ..Default::default()
                        };
                        self.map.insert(key, entry);
                        return entry;
                    }
                }
            }
        };

        let inv = 1.0 / self.size as f32;
        let entry = GlyphEntry {
            uv: [
                x as f32 * inv,
                y as f32 * inv,
                (x + raster.width) as f32 * inv,
                (y + raster.height) as f32 * inv,
            ],
            width: raster.width,
            height: raster.height,
            left: raster.left,
            top: raster.top,
            color: raster.color,
        };
        self.pending.push(Upload {
            x,
            y,
            width: raster.width,
            height: raster.height,
            rgba: raster.rgba(),
        });
        self.map.insert(key, entry);
        entry
    }

    /// Find room, growing onto a new shelf when no existing one fits.
    fn allocate(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let need_w = width + PADDING * 2;
        let need_h = height + PADDING * 2;
        if need_w > self.size || need_h > self.size {
            return None;
        }
        for shelf in &mut self.shelves {
            if shelf.height >= need_h && shelf.cursor_x + need_w <= self.size {
                let x = shelf.cursor_x + PADDING;
                shelf.cursor_x += need_w;
                return Some((x, shelf.y + PADDING));
            }
        }
        if self.next_y + need_h > self.size {
            return None;
        }
        let y = self.next_y;
        self.next_y += need_h;
        self.shelves.push(Shelf { y, height: need_h, cursor_x: need_w });
        Some((PADDING, y + PADDING))
    }

    /// Forget every packed glyph.
    ///
    /// Public for the font-size change: every entry was rasterised at the
    /// old size and is keyed by it, so none would ever be read again —
    /// which makes them exactly the kind of thing that fills an atlas.
    pub fn reset(&mut self) {
        self.shelves.clear();
        self.next_y = 0;
        self.map.clear();
        self.pending.clear();
        self.generation += 1;
    }

    /// Take the uploads queued since the last call.
    pub fn take_uploads(&mut self) -> Vec<Upload> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod colour_tests {
    use super::*;
    use crate::font::Raster;

    fn mask(width: u32, height: u32, coverage: u8) -> Raster {
        Raster {
            width,
            height,
            data: vec![coverage; (width * height) as usize],
            color: false,
            left: 0,
            top: 0,
        }
    }

    fn colour(width: u32, height: u32, rgba: [u8; 4]) -> Raster {
        Raster {
            width,
            height,
            data: rgba.repeat((width * height) as usize),
            color: true,
            left: 0,
            top: 0,
        }
    }

    #[test]
    fn a_mask_becomes_white_with_its_coverage_as_alpha() {
        // One texture serves both kinds, so a mask has to arrive as RGBA
        // that a multiply by the foreground turns back into coverage.
        let rgba = mask(2, 1, 0x80).rgba();
        assert_eq!(rgba, vec![0xff, 0xff, 0xff, 0x80, 0xff, 0xff, 0xff, 0x80]);
    }

    #[test]
    fn a_colour_glyph_keeps_its_own_bytes() {
        let raster = colour(1, 1, [0x20, 0x40, 0x60, 0xff]);
        assert_eq!(raster.rgba(), vec![0x20, 0x40, 0x60, 0xff]);
    }

    #[test]
    fn the_entry_remembers_which_kind_it_is() {
        // The renderer has nothing else to go on when deciding whether to
        // tint, and tinting an emoji grey is the bug this prevents.
        let mut atlas = Atlas::new(128);
        let plain = atlas.insert(GlyphKey::new(FaceId(0), 1, 16.0), &mask(4, 4, 0xff));
        let emoji = atlas.insert(GlyphKey::new(FaceId(0), 2, 16.0), &colour(4, 4, [1, 2, 3, 4]));
        assert!(!plain.color);
        assert!(emoji.color);
    }

    #[test]
    fn uploads_are_four_bytes_a_pixel_whatever_went_in() {
        let mut atlas = Atlas::new(128);
        atlas.insert(GlyphKey::new(FaceId(0), 1, 16.0), &mask(3, 2, 0xff));
        atlas.insert(GlyphKey::new(FaceId(0), 2, 16.0), &colour(3, 2, [9, 9, 9, 9]));
        for upload in atlas.take_uploads() {
            assert_eq!(
                upload.rgba.len() as u32,
                upload.width * upload.height * 4,
                "a row stride that disagrees with the texture corrupts the atlas"
            );
        }
    }

    #[test]
    fn an_inkless_colour_glyph_is_still_flagged() {
        let mut atlas = Atlas::new(128);
        let entry = atlas.insert(GlyphKey::new(FaceId(0), 3, 16.0), &colour(0, 0, [0; 4]));
        assert!(entry.is_empty());
        assert!(entry.color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(width: u32, height: u32) -> Raster {
        Raster {
            width,
            height,
            data: vec![0xff; (width * height) as usize],
            color: false,
            left: 1,
            top: 2,
        }
    }

    fn key(glyph: u16) -> GlyphKey {
        GlyphKey::new(FaceId(0), glyph, 16.0)
    }

    fn overlaps(a: &GlyphEntry, b: &GlyphEntry, size: f32) -> bool {
        let (ax0, ay0, ax1, ay1) = (a.uv[0] * size, a.uv[1] * size, a.uv[2] * size, a.uv[3] * size);
        let (bx0, by0, bx1, by1) = (b.uv[0] * size, b.uv[1] * size, b.uv[2] * size, b.uv[3] * size);
        ax0 < bx1 && bx0 < ax1 && ay0 < by1 && by0 < ay1
    }

    #[test]
    fn glyphs_get_distinct_non_overlapping_space() {
        let mut atlas = Atlas::new(256);
        let entries: Vec<GlyphEntry> =
            (0..20).map(|i| atlas.insert(key(i), &raster(10, 14))).collect();

        for (i, a) in entries.iter().enumerate() {
            for b in &entries[i + 1..] {
                assert!(!overlaps(a, b, 256.0), "glyphs overlap: {a:?} vs {b:?}");
            }
        }
        assert_eq!(atlas.len(), 20);
        assert_eq!(atlas.take_uploads().len(), 20, "each new glyph needs an upload");
    }

    #[test]
    fn a_repeated_glyph_is_free() {
        let mut atlas = Atlas::new(256);
        let first = atlas.insert(key(7), &raster(8, 12));
        let _ = atlas.take_uploads();
        let second = atlas.insert(key(7), &raster(8, 12));
        assert_eq!(first, second);
        assert!(atlas.take_uploads().is_empty(), "a cache hit must not re-upload");
    }

    #[test]
    fn the_same_glyph_at_a_different_size_is_a_different_entry() {
        let mut atlas = Atlas::new(256);
        let small = atlas.insert(GlyphKey::new(FaceId(0), 1, 12.0), &raster(6, 9));
        let large = atlas.insert(GlyphKey::new(FaceId(0), 1, 24.0), &raster(12, 18));
        assert_ne!(small.uv, large.uv, "a resize must re-rasterize, not reuse");
    }

    #[test]
    fn uv_coordinates_stay_inside_the_texture() {
        let mut atlas = Atlas::new(128);
        for i in 0..40 {
            let entry = atlas.insert(key(i), &raster(9, 13));
            for v in entry.uv {
                assert!((0.0..=1.0).contains(&v), "uv out of range: {entry:?}");
            }
        }
    }

    #[test]
    fn an_inkless_glyph_takes_no_space_but_still_gets_an_entry() {
        let mut atlas = Atlas::new(128);
        let entry = atlas.insert(key(3), &raster(0, 0));
        assert!(entry.is_empty());
        assert_eq!(entry.left, 1, "placement is still remembered");
        assert!(atlas.take_uploads().is_empty(), "nothing to upload for a space");
    }

    #[test]
    fn a_full_atlas_resets_rather_than_failing() {
        // A deliberately tiny atlas, so it fills within the test.
        let mut atlas = Atlas::new(64);
        let mut generations = Vec::new();
        for i in 0..200 {
            atlas.insert(key(i), &raster(20, 20));
            generations.push(atlas.generation());
        }
        assert!(
            generations.last() > generations.first(),
            "the atlas should have recycled at least once"
        );
        // And it is still usable afterwards.
        let entry = atlas.insert(key(999), &raster(10, 10));
        assert!(!entry.is_empty());
    }

    #[test]
    fn a_glyph_larger_than_the_atlas_is_refused_not_wrapped() {
        let mut atlas = Atlas::new(64);
        let entry = atlas.insert(key(1), &raster(200, 200));
        // Resetting cannot help, so the glyph is skipped: an empty entry,
        // not a panic and not uv coordinates covering the whole texture.
        assert!(entry.is_empty(), "{entry:?}");
        // And the atlas still works for glyphs that do fit.
        assert!(!atlas.insert(key(2), &raster(10, 10)).is_empty());
    }
}
