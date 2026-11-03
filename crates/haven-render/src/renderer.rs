//! The wgpu renderer. See `docs/PLAN.md` §12.
//!
//! Two instanced passes per frame: solid quads (backgrounds, decorations, the
//! cursor) and glyph quads against the atlas. A solid screen is a handful of
//! instances because same-background runs are merged, and a repeated character
//! never re-rasterizes.

use bytemuck::{Pod, Zeroable};
use haven_core::term::CursorShape;
use haven_core::{Attrs, Color, Palette, Rgb, Role, Screen, Underline};
use wgpu::util::DeviceExt;

use crate::atlas::{Atlas, GlyphKey};
use crate::font::{CellMetrics, FontError, FontStack};

/// One instance: a rectangle in pixels, a slice of the atlas, and a colour.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct Quad {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
    color: [f32; 4],
    /// Zero when the glyph carries its own colours and the foreground must
    /// not be applied to it. A float rather than a flag because a vertex
    /// attribute is cheaper to keep as one.
    tint: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport: [f32; 2],
    _pad: [f32; 2],
}

/// What the renderer draws into.
enum Target {
    /// An offscreen texture plus a readback buffer: tests and screenshots.
    Offscreen {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        readback: wgpu::Buffer,
        padded_bytes_per_row: u32,
    },
    /// A window's swapchain.
    Surface { surface: wgpu::Surface<'static>, config: wgpu::SurfaceConfiguration },
}

const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    solid_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
    atlas_texture: wgpu::Texture,
    atlas_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: u64,
    target: Target,
    width: u32,
    height: u32,
    pub atlas: Atlas,
    pub fonts: FontStack,
    cell: CellMetrics,
    /// The largest texture this device will make. A surface cannot exceed
    /// it, and asking is a validation error that aborts the process rather
    /// than something that can be caught.
    max_dim: u32,
    solid: Vec<Quad>,
    /// Background rectangles, kept apart so they can be drawn underneath the
    /// box drawing rather than on top of it.
    backdrop: Vec<Quad>,
    glyphs: Vec<Quad>,
    /// Bumped on every frame, for tests and for a frame-time HUD later.
    pub frames: u64,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("size", &(self.width, self.height))
            .field("cell", &self.cell)
            .field("atlas_glyphs", &self.atlas.len())
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum RenderError {
    NoAdapter,
    Device(String),
    Font(FontError),
    Readback(String),
    /// The session ended badly — version skew, a protocol failure. Nothing to
    /// do with the GPU, and reporting it as a device error sent me looking in
    /// the wrong place once already.
    Session(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAdapter => write!(f, "no suitable GPU adapter"),
            Self::Device(e) => write!(f, "device request failed: {e}"),
            Self::Font(e) => write!(f, "{e}"),
            Self::Readback(e) => write!(f, "pixel readback failed: {e}"),
            Self::Session(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RenderError {}

impl From<FontError> for RenderError {
    fn from(e: FontError) -> Self {
        Self::Font(e)
    }
}

/// 1024², which at RGBA is 4 MB on the GPU. Coverage alone would be a
/// quarter of that, and would mean a second texture and a second bind group
/// for colour glyphs — a pipeline switch in the middle of a line of text, to
/// save four megabytes.
const ATLAS_SIZE: u32 = 1024;

/// Which GPU and which graphics API to draw a window with.
///
/// This matters more than it should. On a hybrid laptop the discrete GPU
/// reaches an integrated-driven display through PRIME, and presenting over
/// that path while the window manager unmaps the window was measured blocking
/// the whole X server for two seconds — every client on the display, not just
/// ours. A terminal has no use for a discrete GPU anyway, so the integrated
/// one is both the fast default and the safe one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GpuPreference {
    pub backends: wgpu::Backends,
    pub power: wgpu::PowerPreference,
}

impl Default for GpuPreference {
    fn default() -> Self {
        Self { backends: wgpu::Backends::all(), power: wgpu::PowerPreference::LowPower }
    }
}

impl GpuPreference {
    /// `auto`, `gl` or `vulkan`, as the CLI spells them.
    pub fn with_backend(mut self, name: &str) -> Result<Self, String> {
        self.backends = match name {
            "auto" => wgpu::Backends::all(),
            "gl" => wgpu::Backends::GL,
            "vulkan" => wgpu::Backends::VULKAN,
            other => return Err(format!("unknown backend {other:?}; try auto, gl or vulkan")),
        };
        Ok(self)
    }

    pub fn with_power(mut self, name: &str) -> Result<Self, String> {
        self.power = match name {
            "low" => wgpu::PowerPreference::LowPower,
            "high" => wgpu::PowerPreference::HighPerformance,
            other => return Err(format!("unknown gpu power {other:?}; try low or high")),
        };
        Ok(self)
    }
}

impl Renderer {
    /// An offscreen renderer sized to a cell grid.
    ///
    /// Offscreen first, on purpose: it makes the whole renderer testable with
    /// pixel assertions and no window, which is the only way to check text
    /// rendering without a human looking at it (`docs/PLAN.md` §17).
    pub fn offscreen(
        cols: u32,
        rows: u32,
        font_size: f32,
        font: Option<&str>,
    ) -> Result<Self, RenderError> {
        let fonts = FontStack::new(font, font_size)?;
        let cell = fonts.metrics();
        let width = (cols * cell.width).max(1);
        let height = (rows * cell.height).max(1);

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(|_| RenderError::NoAdapter)?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("haven"),
            // Deliberately conservative: no bindless, no push descriptors, so
            // the GL backend and older drivers stay viable (§18).
            required_features: wgpu::Features::empty(),
            // Conservative everywhere except resolution, which must follow
            // the adapter: `downlevel_defaults` caps a texture at 2048, and a
            // window taller than that cannot be given a surface at all.
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| RenderError::Device(e.to_string()))?;

        // Same ceilings offscreen. A screenshot of a very large grid would
        // otherwise abort rather than be cropped — and the readback buffer
        // has its own, smaller limit than the texture does.
        let limits = device.limits();
        let (width, height) = clamp_offscreen(width, height, &limits);
        Self::build(device, queue, fonts, cell, width, height, TARGET_FORMAT)
    }

    /// A renderer drawing into a window's surface.
    ///
    /// Takes an already-created surface so this crate does not have to know
    /// what kind of window it is — the frontend owns that.
    pub fn for_surface(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        font_size: f32,
        font: Option<&str>,
        pref: GpuPreference,
    ) -> Result<Self, RenderError> {
        let fonts = FontStack::new(font, font_size)?;
        let cell = fonts.metrics();

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: pref.power,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|_| RenderError::NoAdapter)?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("haven"),
            required_features: wgpu::Features::empty(),
            // Conservative everywhere except resolution, which must follow
            // the adapter: `downlevel_defaults` caps a texture at 2048, and a
            // window taller than that cannot be given a surface at all.
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| RenderError::Device(e.to_string()))?;

        // A surface larger than the device's biggest texture is a validation
        // error, and a validation error aborts the process — it cannot be
        // caught. A window simply is whatever size the user made it, so the
        // size is clamped here rather than hoped about.
        let max_dim = device.limits().max_texture_dimension_2d;
        let (width, height) = (width.clamp(1, max_dim), height.clamp(1, max_dim));

        let caps = surface.get_capabilities(&adapter);
        // Match the offscreen path's colour space, so what a screenshot shows
        // is what the window shows.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == TARGET_FORMAT)
            .or_else(|| caps.formats.first().copied())
            .ok_or(RenderError::NoAdapter)?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            // Vsync: a terminal has no reason to draw faster than the display,
            // and tearing in text is miserable to read.
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // The pipelines must be built for the surface's *actual* format: a
        // window may hand us BGRA where an offscreen texture is RGBA, and the
        // mismatch is a validation error at the first draw rather than
        // anything visible. Caught by running under a software rasterizer,
        // which picks a different format than the discrete GPU does.
        let mut renderer = Self::build(device, queue, fonts, cell, width, height, format)?;
        renderer.target = Target::Surface { surface, config };
        Ok(renderer)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        device: wgpu::Device,
        queue: wgpu::Queue,
        fonts: FontStack,
        cell: CellMetrics,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<Self, RenderError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("haven shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("haven uniforms"),
            contents: bytemuck::bytes_of(&Uniforms {
                viewport: [width as f32, height as f32],
                _pad: [0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haven uniform layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let uniform_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("haven uniforms"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("haven glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Coverage only: colour emoji get their own texture when they land.
            // sRGB, so a colour glyph's bytes are converted to linear on
            // sampling exactly as the target converts back on write. A
            // coverage mask is unaffected: its colour is white either way,
            // and alpha is never gamma-encoded.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("haven atlas sampler"),
            // Nearest: glyphs are rasterized at the exact size they are drawn,
            // so filtering would only blur them.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haven atlas layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let atlas_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("haven atlas"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Quad>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x2,
                1 => Float32x2,
                2 => Float32x4,
                3 => Float32x4,
                4 => Float32,
            ],
        };

        let blend = Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        });

        let make_pipeline = |label: &str,
                             layouts: &[&wgpu::BindGroupLayout],
                             entry: &str|
         -> wgpu::RenderPipeline {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: layouts,
                push_constant_ranges: &[],
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: std::slice::from_ref(&instance_layout),
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };

        let solid_pipeline = make_pipeline("haven solid", &[&uniform_layout], "fs_solid");
        let glyph_pipeline =
            make_pipeline("haven glyph", &[&uniform_layout, &atlas_layout], "fs_glyph");

        let instance_capacity = 4096;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("haven instances"),
            size: instance_capacity * std::mem::size_of::<Quad>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let target = Self::make_offscreen(&device, width, height);
        let max_dim = device.limits().max_texture_dimension_2d;

        Ok(Self {
            device,
            queue,
            solid_pipeline,
            glyph_pipeline,
            uniform_buffer,
            uniform_group,
            atlas_texture,
            atlas_group,
            instance_buffer,
            instance_capacity,
            target,
            width,
            height,
            atlas: Atlas::new(ATLAS_SIZE),
            fonts,
            cell,
            max_dim,
            solid: Vec::new(),
            backdrop: Vec::new(),
            glyphs: Vec::new(),
            frames: 0,
        })
    }

    fn make_offscreen(device: &wgpu::Device, width: u32, height: u32) -> Target {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("haven offscreen"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Copies out of a texture need rows padded to 256 bytes.
        let unpadded = width * 4;
        let padded = unpadded.div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("haven readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Target::Offscreen { texture, view, readback, padded_bytes_per_row: padded }
    }

    /// Whether any loaded face has a glyph for this character.
    ///
    /// For tests that depend on a font being installed, so they can skip
    /// rather than fail on a machine without it.
    pub fn can_draw(&mut self, c: char) -> bool {
        self.fonts.face_for(c).is_some()
    }

    pub fn cell_metrics(&self) -> CellMetrics {
        self.cell
    }

    /// The largest texture dimension this device allows.
    pub fn max_dimension(&self) -> u32 {
        self.max_dim
    }

    /// Keep a size inside what the device can actually allocate. A window
    /// larger than the limit renders cropped, which is survivable; asking
    /// for it anyway is a validation error, which is not.
    fn clamp(&self, width: u32, height: u32) -> (u32, u32) {
        (width.clamp(1, self.max_dim), height.clamp(1, self.max_dim))
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Grid size this renderer can show.
    pub fn grid_size(&self) -> (u32, u32) {
        (self.width / self.cell.width, self.height / self.cell.height)
    }

    pub fn resize(&mut self, cols: u32, rows: u32) {
        self.resize_pixels((cols * self.cell.width).max(1), (rows * self.cell.height).max(1));
    }

    /// Resize to an exact pixel size, which is what a window gives us.
    pub fn resize_pixels(&mut self, width: u32, height: u32) {
        let (width, height) = self.clamp(width, height);
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        match &mut self.target {
            Target::Surface { surface, config } => {
                config.width = width;
                config.height = height;
                surface.configure(&self.device, config);
            }
            Target::Offscreen { .. } => {
                self.target = Self::make_offscreen(&self.device, width, height);
            }
        }
        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&Uniforms {
                viewport: [width as f32, height as f32],
                _pad: [0.0, 0.0],
            }),
        );
    }

    /// Draw a composited screen.
    pub fn render(
        &mut self,
        screen: &Screen,
        cursor: (usize, usize),
        palette: &Palette,
        cursor_shape: CursorShape,
    ) {
        self.build_quads(screen, cursor, palette, cursor_shape);
        self.upload_atlas();
        self.encode(palette);
        self.frames += 1;
    }

    fn build_quads(
        &mut self,
        screen: &Screen,
        cursor: (usize, usize),
        palette: &Palette,
        cursor_shape: CursorShape,
    ) {
        self.solid.clear();
        self.backdrop.clear();
        self.glyphs.clear();
        let ch = self.cell.height as f32;

        for (row_index, row) in screen.iter().enumerate() {
            let y = row_index as f32 * ch;
            // Merge same-background runs so a solid screen costs a handful of
            // instances rather than one per cell.
            let mut run: Option<(usize, usize, Rgb)> = None;

            for (col_index, cell) in row.iter().enumerate() {
                if cell.spacer {
                    continue;
                }
                let is_cursor = (row_index, col_index) == cursor;
                let (fg, bg) = resolve(&cell.style, palette, is_cursor, cursor_shape);

                match &mut run {
                    Some((_, end, colour)) if *colour == bg && *end == col_index => {
                        *end = col_index + cell.width();
                    }
                    _ => {
                        if let Some((start, end, colour)) = run.take() {
                            self.push_background(start, end, y, colour, palette);
                        }
                        run = Some((col_index, col_index + cell.width(), bg));
                    }
                }

                if !cell.style.attrs.contains(Attrs::HIDDEN) {
                    self.push_text(cell, col_index, row_index, fg);
                }
                self.push_decorations(cell, col_index, row_index, fg, palette);
            }
            if let Some((start, end, colour)) = run.take() {
                self.push_background(start, end, y, colour, palette);
            }
        }

        // Backgrounds are collected apart and go under everything else.
        // They cannot share a buffer with box drawing: a run is flushed when
        // it *ends*, which is after the cells it covers have been drawn, so
        // the background painted over every procedural line that had a
        // background behind it. Panes never showed it because their lines sit
        // on the default background, which emits no quad at all.
        self.backdrop.append(&mut self.solid);
        std::mem::swap(&mut self.solid, &mut self.backdrop);
    }

    fn push_background(
        &mut self,
        start: usize,
        end: usize,
        y: f32,
        colour: Rgb,
        palette: &Palette,
    ) {
        // The clear colour already covers the default background.
        if colour == palette.bg {
            return;
        }
        let (cw, ch) = (self.cell.width as f32, self.cell.height as f32);
        self.backdrop.push(Quad {
            pos: [start as f32 * cw, y],
            size: [(end - start) as f32 * cw, ch],
            uv: [0.0; 4],
            tint: 1.0,
            color: linear(colour, 1.0),
        });
    }

    fn push_text(&mut self, cell: &haven_core::ScreenCell, col: usize, row: usize, fg: Rgb) {
        let alpha = if cell.style.attrs.contains(Attrs::DIM) { 0.55 } else { 1.0 };
        let colour = linear(fg, alpha);
        let style = crate::font::FaceStyle::of(
            cell.style.attrs.contains(Attrs::BOLD),
            cell.style.attrs.contains(Attrs::ITALIC),
        );

        // Every char in the cluster is drawn at the same pen position: that is
        // what makes a base character plus combining marks land on each other
        // instead of marching across the grid.
        for ch in cell.text.chars() {
            if ch == ' ' {
                continue;
            }
            // Box drawing is drawn, not typeset: the font's glyphs do not
            // reach the cell edges, so tables would come apart.
            if let Some(rects) = crate::boxdraw::rects(ch, self.cell) {
                let (x, y) =
                    (col as f32 * self.cell.width as f32, row as f32 * self.cell.height as f32);
                for r in rects {
                    self.solid.push(Quad {
                        pos: [x + r.x, y + r.y],
                        size: [r.w, r.h],
                        uv: [0.0; 4],
                        tint: 1.0,
                        color: linear(fg, alpha * r.alpha),
                    });
                }
                continue;
            }
            let Some(styled) = self.fonts.styled_face_for(ch, style) else { continue };
            let key = GlyphKey::new(styled.face, styled.glyph, self.fonts.size_px())
                .slanted(styled.synth_italic);
            let entry = match self.atlas.get(&key) {
                Some(entry) => entry,
                None => {
                    match self.fonts.rasterize_slanted(
                        styled.face,
                        styled.glyph,
                        styled.synth_italic,
                    ) {
                        Some(raster) => self.atlas.insert(key, &raster),
                        None => continue,
                    }
                }
            };
            if entry.is_empty() {
                continue;
            }

            // A combining mark's own bearings assume the pen has already
            // advanced past its base, which we never do — so it would land
            // beside the letter instead of on it. Centre it over the cell
            // instead, which is what the mark is for.
            let x = if FontStack::is_combining(ch) {
                col as f32 * self.cell.width as f32
                    + (self.cell.width as f32 - entry.width as f32) / 2.0
            } else {
                col as f32 * self.cell.width as f32 + entry.left as f32
            };
            let y =
                row as f32 * self.cell.height as f32 + self.cell.baseline as f32 - entry.top as f32;
            // A glyph that brought its own colours keeps them; only its
            // alpha follows the cell, so DIM still dims an emoji.
            let tint = if entry.color { 0.0 } else { 1.0 };
            let colour = if entry.color { [1.0, 1.0, 1.0, alpha] } else { colour };
            self.glyphs.push(Quad {
                pos: [x, y],
                size: [entry.width as f32, entry.height as f32],
                uv: entry.uv,
                color: colour,
                tint,
            });
            // Synthetic bold is a second pass one pixel over, only where the
            // family has no bold cut to use instead. Crude next to a real
            // bold face, and honest about it — but never on a colour glyph,
            // where doubling makes a smeared emoji rather than a bold one.
            if styled.synth_bold && !entry.color {
                self.glyphs.push(Quad {
                    pos: [x + 1.0, y],
                    size: [entry.width as f32, entry.height as f32],
                    uv: entry.uv,
                    color: colour,
                    tint,
                });
            }
        }
    }

    fn push_decorations(
        &mut self,
        cell: &haven_core::ScreenCell,
        col: usize,
        row: usize,
        fg: Rgb,
        palette: &Palette,
    ) {
        let (cw, ch) = (self.cell.width as f32, self.cell.height as f32);
        let x = col as f32 * cw;
        let y = row as f32 * ch;
        let width = cw * cell.width() as f32;

        if cell.style.underline != Underline::None {
            let colour = match cell.style.underline_color {
                Color::Default => fg,
                other => palette.resolve(other, Role::Fg),
            };
            // Undercurl is drawn procedurally in the shader later; for now
            // every style shows as a line of its own thickness.
            let thickness = match cell.style.underline {
                Underline::Double => self.cell.underline_thickness * 3,
                _ => self.cell.underline_thickness,
            };
            self.solid.push(Quad {
                pos: [x, y + self.cell.underline_y as f32],
                size: [width, thickness as f32],
                uv: [0.0; 4],
                tint: 1.0,
                color: linear(colour, 1.0),
            });
        }
        if cell.style.attrs.contains(Attrs::STRIKE) {
            self.solid.push(Quad {
                pos: [x, y + self.cell.baseline as f32 * 0.65],
                size: [width, self.cell.underline_thickness as f32],
                uv: [0.0; 4],
                tint: 1.0,
                color: linear(fg, 1.0),
            });
        }
        if cell.style.attrs.contains(Attrs::OVERLINE) {
            self.solid.push(Quad {
                pos: [x, y],
                size: [width, self.cell.underline_thickness as f32],
                uv: [0.0; 4],
                tint: 1.0,
                color: linear(fg, 1.0),
            });
        }
    }

    fn upload_atlas(&mut self) {
        for upload in self.atlas.take_uploads() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: upload.x, y: upload.y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &upload.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(upload.width * 4),
                    rows_per_image: Some(upload.height),
                },
                wgpu::Extent3d {
                    width: upload.width,
                    height: upload.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn ensure_capacity(&mut self, quads: u64) {
        if quads <= self.instance_capacity {
            return;
        }
        let capacity = quads.next_power_of_two();
        self.instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("haven instances"),
            size: capacity * std::mem::size_of::<Quad>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.instance_capacity = capacity;
    }

    fn encode(&mut self, palette: &Palette) {
        let solid_count = self.solid.len() as u64;
        let glyph_count = self.glyphs.len() as u64;
        self.ensure_capacity(solid_count + glyph_count);

        let stride = std::mem::size_of::<Quad>() as u64;
        if solid_count > 0 {
            self.queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&self.solid));
        }
        if glyph_count > 0 {
            self.queue.write_buffer(
                &self.instance_buffer,
                solid_count * stride,
                bytemuck::cast_slice(&self.glyphs),
            );
        }

        // A surface frame must outlive the submit, so it is acquired here and
        // presented at the end.
        let surface_frame = match &self.target {
            Target::Surface { surface, .. } => match surface.get_current_texture() {
                Ok(frame) => Some(frame),
                // A lost or outdated surface is routine — a resize caught
                // mid-frame — and skipping one frame is the right response.
                Err(_) => return,
            },
            Target::Offscreen { .. } => None,
        };
        let surface_view = surface_frame
            .as_ref()
            .map(|f| f.texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let view: &wgpu::TextureView = match (&self.target, &surface_view) {
            (Target::Offscreen { view, .. }, _) => view,
            (Target::Surface { .. }, Some(view)) => view,
            _ => return,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("haven frame") });
        {
            let clear = linear(palette.bg, 1.0);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("haven pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // The default background is the clear colour, so the
                        // common case costs no instances at all.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0] as f64,
                            g: clear[1] as f64,
                            b: clear[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_bind_group(0, &self.uniform_group, &[]);
            if solid_count > 0 {
                pass.set_pipeline(&self.solid_pipeline);
                pass.set_vertex_buffer(0, self.instance_buffer.slice(..solid_count * stride));
                pass.draw(0..6, 0..solid_count as u32);
            }
            if glyph_count > 0 {
                pass.set_pipeline(&self.glyph_pipeline);
                pass.set_bind_group(1, &self.atlas_group, &[]);
                pass.set_vertex_buffer(0, self.instance_buffer.slice(solid_count * stride..));
                pass.draw(0..6, 0..glyph_count as u32);
            }
        }

        if let Target::Offscreen { texture, readback, padded_bytes_per_row, .. } = &self.target {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(*padded_bytes_per_row),
                        rows_per_image: Some(self.height),
                    },
                },
                wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
            );
        }
        self.queue.submit([encoder.finish()]);
        if let Some(frame) = surface_frame {
            frame.present();
        }
    }

    /// Read the last frame back as `RGBA8`, row-major, unpadded.
    pub fn read_pixels(&self) -> Result<Vec<u8>, RenderError> {
        let Target::Offscreen { readback, padded_bytes_per_row, .. } = &self.target else {
            return Err(RenderError::Readback("this renderer draws to a window".into()));
        };
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| RenderError::Readback(e.to_string()))?
            .map_err(|e| RenderError::Readback(e.to_string()))?;

        let padded = *padded_bytes_per_row as usize;
        let row_bytes = self.width as usize * 4;
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity(row_bytes * self.height as usize);
        for row in 0..self.height as usize {
            let start = row * padded;
            out.extend_from_slice(&data[start..start + row_bytes]);
        }
        drop(data);
        readback.unmap();
        Ok(out)
    }

    /// The pixel at a position, as `RGBA8`.
    pub fn pixel(&self, pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
        let index = (y as usize * self.width as usize + x as usize) * 4;
        pixels.get(index..index + 4).map(|p| [p[0], p[1], p[2], p[3]]).unwrap_or([0; 4])
    }

    /// How many instances the last frame used, for the perf tests.
    pub fn instance_counts(&self) -> (usize, usize) {
        (self.solid.len(), self.glyphs.len())
    }
}

/// Resolve a cell's colours, applying reverse video and the cursor.
///
/// The order matters and is defined once, here: reverse first, then the
/// cursor on top of it. Every terminal gets some cell of this matrix wrong
/// (`docs/PLAN.md` §7.4).
fn resolve(
    style: &haven_core::Style,
    palette: &Palette,
    is_cursor: bool,
    shape: CursorShape,
) -> (Rgb, Rgb) {
    let mut fg = palette.resolve(style.fg, Role::Fg);
    let mut bg = palette.resolve(style.bg, Role::Bg);
    if style.attrs.contains(Attrs::REVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if is_cursor && shape == CursorShape::Block {
        // A block cursor inverts the cell: the glyph takes the background
        // colour so it stays readable inside the block.
        std::mem::swap(&mut fg, &mut bg);
        bg = palette.cursor;
    }
    (fg, bg)
}

/// sRGB bytes to linear floats. The target is an sRGB texture, so the shader's
/// output is treated as linear and converted on write; skipping this is what
/// makes text look too heavy.
/// Shrink an offscreen size until both the texture and the buffer it is read
/// back through are within the device's limits. The aspect ratio is kept, so
/// a huge grid comes out scaled down rather than cropped to a corner.
fn clamp_offscreen(width: u32, height: u32, limits: &wgpu::Limits) -> (u32, u32) {
    let max_dim = limits.max_texture_dimension_2d;
    let (mut w, mut h) = (width.clamp(1, max_dim), height.clamp(1, max_dim));
    // Readback rows are padded to 256 bytes, so the buffer is bigger than
    // the obvious width * height * 4.
    let bytes = |w: u32, h: u32| ((w * 4).div_ceil(256) * 256) as u64 * h as u64;
    while bytes(w, h) > limits.max_buffer_size && (w > 1 || h > 1) {
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    (w, h)
}

fn linear(rgb: Rgb, alpha: f32) -> [f32; 4] {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [f(rgb.r), f(rgb.g), f(rgb.b), alpha]
}
