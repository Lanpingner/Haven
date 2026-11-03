// One vertex shader for every quad we draw; two fragment shaders, one for
// solid fills (backgrounds, decorations, the cursor) and one that samples the
// glyph atlas. See docs/PLAN.md section 12.1.

struct Uniforms {
    viewport: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct Quad {
    // Top-left corner and size, in pixels.
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    // Atlas rect as (u0, v0, u1, v1); ignored by the solid pipeline.
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
    // 1 for a coverage mask that takes the cell's foreground; 0 for a glyph
    // that brought its own colours.
    @location(4) tint: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) tint: f32,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32, quad: Quad) -> VsOut {
    // Six vertices per instance: no vertex or index buffer needed.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let corner = corners[index];
    let pixel = quad.pos + corner * quad.size;

    var out: VsOut;
    out.clip = vec4<f32>(
        pixel.x / uniforms.viewport.x * 2.0 - 1.0,
        1.0 - pixel.y / uniforms.viewport.y * 2.0,
        0.0,
        1.0,
    );
    out.uv = vec2<f32>(
        mix(quad.uv.x, quad.uv.z, corner.x),
        mix(quad.uv.y, quad.uv.w, corner.y),
    );
    out.color = quad.color;
    out.tint = quad.tint;
    return out;
}

@fragment
fn fs_solid(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}

@group(1) @binding(0) var atlas_texture: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

@fragment
fn fs_glyph(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_texture, atlas_sampler, in.uv);
    // A coverage mask is stored white, so its rgb is 1 and taking the cell's
    // foreground is a multiply. A colour glyph keeps its own rgb and only
    // its alpha follows the cell, which is what makes DIM dim an emoji
    // without draining the colour out of it.
    let rgb = mix(texel.rgb, in.color.rgb, in.tint);
    return vec4<f32>(rgb, in.color.a * texel.a);
}
