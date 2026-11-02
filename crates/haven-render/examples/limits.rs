//! What texture size the renderer is actually allowed.
fn main() {
    match haven_render::Renderer::offscreen(10, 4, 16.0, None) {
        Ok(r) => println!("max_texture_dimension_2d = {}", r.max_dimension()),
        Err(e) => println!("no renderer: {e}"),
    }
}
