//! GPU frontend: wgpu pipelines, glyph atlas, text and decorations.
//! See `docs/PLAN.md` §12.

pub mod atlas;
pub mod boxdraw;
pub mod font;
pub mod renderer;

pub use atlas::{Atlas, GlyphEntry, GlyphKey, Upload};
pub use font::{CellMetrics, FaceId, FontError, FontStack, Raster};
pub use renderer::{Quad, RenderError, Renderer};
