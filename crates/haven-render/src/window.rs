//! The GPU frontend: a window, a surface, and the winit event loop.
//!
//! `docs/PLAN.md` §3 makes the frontend a trait so the mux never learns which
//! one it is talking to. The awkward part is that winit *pushes* events while
//! [`Frontend`] *pulls* them, so this module owns the loop and feeds a queue
//! the client drains — the inversion lives here and nowhere else.

use std::collections::VecDeque;
use std::sync::Arc;

use haven_client::frame::{composite, FrameModel};
use haven_client::frontend::{Frontend, PresentError};
use haven_client::input::{InputEvent, Key, KeyEvent, KeyState, Modifiers, NamedKey};
use haven_client::{Client, Tick};
use haven_core::term::CursorShape;
use haven_core::Palette;
use haven_proto::{CellSize, ClientCaps, ColorDepth, FrontendKind, KeyTier, SessionId, Transport};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey as WinitNamed};
use winit::window::{Window, WindowId};

use crate::renderer::{GpuPreference, RenderError, Renderer};

/// A nudge from the transport-watching thread: there is something to read.
#[derive(Clone, Copy, Debug)]
pub struct Wake;

/// The frontend half: a queue winit fills and the client drains, plus the
/// renderer once the window exists.
#[derive(Debug, Default)]
pub struct GpuFrontend {
    renderer: Option<Renderer>,
    window: Option<Arc<Window>>,
    queue: VecDeque<InputEvent>,
    size: CellSize,
    /// The window manager has the window hidden — on another AwesomeWM tag,
    /// another workspace, or fully covered. Presenting anyway is not merely
    /// wasted work: with `PresentMode::Fifo` the acquire waits on a vblank
    /// that an unmapped window never gets, and that wait is inside the event
    /// loop, so the X server's own requests go unanswered too.
    occluded: bool,
    /// State moved on while we were hidden, so a redraw is owed on return.
    needs_redraw: bool,
    pub palette: Palette,
    pub cursor_shape: CursorShape,
}

impl GpuFrontend {
    pub fn new() -> Self {
        Self { size: CellSize::default(), ..Default::default() }
    }

    pub fn push(&mut self, event: InputEvent) {
        self.queue.push_back(event);
    }

    /// Visibility, as the compositor sees it.
    pub fn set_occluded(&mut self, occluded: bool) {
        self.occluded = occluded;
        if !occluded && self.needs_redraw {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    pub fn is_occluded(&self) -> bool {
        self.occluded
    }

    pub fn is_ready(&self) -> bool {
        self.renderer.is_some()
    }

    pub fn grid_size(&self) -> CellSize {
        self.size
    }

    fn adopt(&mut self, window: Arc<Window>, renderer: Renderer) {
        let (cols, rows) = renderer.grid_size();
        let physical = window.inner_size();
        self.size = CellSize {
            cols: cols.max(1) as u16,
            rows: rows.max(1) as u16,
            pixel_width: physical.width as u16,
            pixel_height: physical.height as u16,
        };
        self.window = Some(window);
        self.renderer = Some(renderer);
    }

    fn resized(&mut self, width: u32, height: u32) -> CellSize {
        if let Some(renderer) = &mut self.renderer {
            renderer.resize_pixels(width, height);
            let (cols, rows) = renderer.grid_size();
            self.size = CellSize {
                cols: cols.max(1) as u16,
                rows: rows.max(1) as u16,
                pixel_width: width as u16,
                pixel_height: height as u16,
            };
        }
        self.size
    }
}

impl Frontend for GpuFrontend {
    fn caps(&self) -> ClientCaps {
        ClientCaps {
            frontend: FrontendKind::Gpu,
            size: self.size,
            color_depth: ColorDepth::TrueColor,
            // A window gets every keycode, so nothing has to be given up
            // (`docs/PLAN.md` §11.1).
            key_tier: KeyTier::Full,
            synchronized_output: true,
            clipboard_osc52: true,
            hyperlinks: true,
        }
    }

    fn next_input(&mut self) -> Option<InputEvent> {
        self.queue.pop_front()
    }

    fn present(&mut self, frame: &FrameModel) -> Result<(), PresentError> {
        let Some(renderer) = &mut self.renderer else { return Ok(()) };
        if self.occluded {
            self.needs_redraw = true;
            return Ok(());
        }
        self.needs_redraw = false;
        let (screen, cursor) = composite(frame);
        renderer.render(&screen, cursor, &self.palette, self.cursor_shape);
        Ok(())
    }
}

/// Translate a key press into ours.
///
/// Takes the fields rather than winit's `KeyEvent`, which cannot be built
/// outside winit on every platform — and a mapping table with this many
/// entries is exactly the thing that needs tests.
pub fn translate_key(
    logical: &WinitKey,
    state: ElementState,
    repeat: bool,
    mods: ModifiersState,
) -> Option<InputEvent> {
    let state = match (state, repeat) {
        (ElementState::Pressed, false) => KeyState::Press,
        (ElementState::Pressed, true) => KeyState::Repeat,
        (ElementState::Released, _) => KeyState::Release,
    };

    let key = match logical {
        WinitKey::Named(named) => Key::Named(match named {
            WinitNamed::Enter => NamedKey::Enter,
            WinitNamed::Tab => NamedKey::Tab,
            WinitNamed::Backspace => NamedKey::Backspace,
            WinitNamed::Escape => NamedKey::Escape,
            WinitNamed::ArrowUp => NamedKey::Up,
            WinitNamed::ArrowDown => NamedKey::Down,
            WinitNamed::ArrowLeft => NamedKey::Left,
            WinitNamed::ArrowRight => NamedKey::Right,
            WinitNamed::Home => NamedKey::Home,
            WinitNamed::End => NamedKey::End,
            WinitNamed::PageUp => NamedKey::PageUp,
            WinitNamed::PageDown => NamedKey::PageDown,
            WinitNamed::Insert => NamedKey::Insert,
            WinitNamed::Delete => NamedKey::Delete,
            WinitNamed::Space => return Some(key_event(Key::Char(' '), mods, state)),
            WinitNamed::F1 => NamedKey::Function(1),
            WinitNamed::F2 => NamedKey::Function(2),
            WinitNamed::F3 => NamedKey::Function(3),
            WinitNamed::F4 => NamedKey::Function(4),
            WinitNamed::F5 => NamedKey::Function(5),
            WinitNamed::F6 => NamedKey::Function(6),
            WinitNamed::F7 => NamedKey::Function(7),
            WinitNamed::F8 => NamedKey::Function(8),
            WinitNamed::F9 => NamedKey::Function(9),
            WinitNamed::F10 => NamedKey::Function(10),
            WinitNamed::F11 => NamedKey::Function(11),
            WinitNamed::F12 => NamedKey::Function(12),
            // Modifier keys on their own are not input: at Full fidelity we
            // could report them, but nothing downstream wants them yet.
            _ => return None,
        }),
        WinitKey::Character(text) => Key::Char(text.chars().next()?),
        _ => return None,
    };
    Some(key_event(key, mods, state))
}

fn key_event(key: Key, mods: ModifiersState, state: KeyState) -> InputEvent {
    let mut modifiers = Modifiers::empty();
    if mods.shift_key() {
        modifiers |= Modifiers::SHIFT;
    }
    if mods.alt_key() {
        modifiers |= Modifiers::ALT;
    }
    if mods.control_key() {
        modifiers |= Modifiers::CTRL;
    }
    if mods.super_key() {
        modifiers |= Modifiers::SUPER;
    }
    InputEvent::Key(KeyEvent { key, mods: modifiers, state })
}

/// How to open the window.
#[derive(Clone, Debug)]
pub struct WindowConfig {
    pub title: String,
    pub cols: u16,
    pub rows: u16,
    pub font: Option<String>,
    pub font_size: f32,
    pub gpu: GpuPreference,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Haven".into(),
            cols: 100,
            rows: 30,
            font: None,
            font_size: 16.0,
            gpu: GpuPreference::default(),
        }
    }
}

struct App<T: Transport> {
    client: Client<GpuFrontend, T>,
    session: Option<SessionId>,
    config: WindowConfig,
    instance: wgpu::Instance,
    modifiers: ModifiersState,
    error: Option<RenderError>,
}

impl<T: Transport> ApplicationHandler<Wake> for App<T> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.client.frontend.is_ready() {
            return;
        }
        // Size the window from the cell metrics, so the grid starts at
        // exactly the requested columns and rows.
        let probe =
            match crate::font::FontStack::new(self.config.font.as_deref(), self.config.font_size) {
                Ok(fonts) => fonts.metrics(),
                Err(e) => {
                    self.error = Some(RenderError::Font(e));
                    event_loop.exit();
                    return;
                }
            };
        let width = probe.width * self.config.cols as u32;
        let height = probe.height * self.config.rows as u32;

        let attributes = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(winit::dpi::PhysicalSize::new(width, height));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                self.error = Some(RenderError::Device(e.to_string()));
                event_loop.exit();
                return;
            }
        };

        let surface = match self.instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(e) => {
                self.error = Some(RenderError::Device(e.to_string()));
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        match Renderer::for_surface(
            &self.instance,
            surface,
            size.width,
            size.height,
            self.config.font_size,
            self.config.font.as_deref(),
            self.config.gpu,
        ) {
            Ok(renderer) => {
                self.client.frontend.adopt(window, renderer);
                let caps = self.client.frontend.caps();
                // Attach only once the real geometry is known, so the daemon
                // never sizes a session to a placeholder.
                if self.client.attach(self.session).is_err() {
                    event_loop.exit();
                    return;
                }
                let _ = caps;
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.client.frontend.push(InputEvent::Quit);
            }
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::KeyboardInput { event, is_synthetic, .. } => {
                // A deliberate hook: the window's key path cannot be checked
                // by reading it — the mapping was correct while the window
                // was still ignoring every binding — and this is how that was
                // finally seen.
                if std::env::var_os("HAVEN_DEBUG_KEYS").is_some() {
                    eprintln!(
                        "key: logical={:?} state={:?} synthetic={} mods={:?}",
                        event.logical_key, event.state, is_synthetic, self.modifiers
                    );
                }
                if is_synthetic {
                    return;
                }
                if let Some(input) =
                    translate_key(&event.logical_key, event.state, event.repeat, self.modifiers)
                {
                    self.client.frontend.push(input);
                }
            }
            WindowEvent::Resized(size) => {
                // A window manager may report 0x0 while unmapping. Taking that
                // literally would resize the session to a single cell and
                // reflow every pane for a window nobody is looking at.
                if size.width > 0 && size.height > 0 {
                    let cells = self.client.frontend.resized(size.width, size.height);
                    self.client.frontend.push(InputEvent::Resize(cells));
                }
            }
            WindowEvent::Occluded(occluded) => {
                self.client.frontend.set_occluded(occluded);
            }
            WindowEvent::Focused(focused) => {
                self.client.frontend.push(InputEvent::Focus(focused));
            }
            WindowEvent::RedrawRequested if !self.client.frontend.is_occluded() => {
                // Re-present what we already have: a redraw request is the
                // compositor asking, not new state. Borrowing the two fields
                // separately keeps this from needing a clone of the frame.
                let client = &mut self.client;
                let _ = client.frontend.present(&client.frame);
            }
            _ => {}
        }
        self.pump(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _: Wake) {
        self.pump(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.pump(event_loop);
        // Idle means idle: no timer, no polling. The watcher thread wakes us
        // when the daemon has something to say (`docs/PLAN.md` §12.4).
        event_loop.set_control_flow(ControlFlow::Wait);
    }
}

impl<T: Transport> App<T> {
    fn pump(&mut self, event_loop: &ActiveEventLoop) {
        match self.client.tick() {
            Ok(Tick::Finished) => event_loop.exit(),
            Ok(_) => {}
            Err(_) => event_loop.exit(),
        }
    }
}

/// Open a window and run until it closes.
///
/// `wake` is a file descriptor to watch for readability — the transport's
/// socket. Without it an attached client would sit in `ControlFlow::Wait` and
/// never notice the daemon; with it, idle costs nothing at all.
/// `key_filter` is what makes the prefix key mean anything. It is passed in
/// rather than built here because the key table lives in the composition
/// root, above this crate — and omitting it was a real bug: the window ran
/// without one for its whole first life, so `Ctrl+A` went straight to the
/// shell and no binding in the window did anything at all.
pub fn run<T: Transport + 'static>(
    transport: T,
    session: Option<SessionId>,
    config: WindowConfig,
    wake: Option<std::os::fd::OwnedFd>,
    key_filter: Option<haven_client::KeyFilter>,
) -> Result<(), RenderError> {
    let event_loop = EventLoop::<Wake>::with_user_event()
        .build()
        .map_err(|e| RenderError::Device(e.to_string()))?;

    if let Some(fd) = wake {
        let proxy = event_loop.create_proxy();
        std::thread::spawn(move || watch(fd, proxy));
    }

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: config.gpu.backends,
        ..Default::default()
    });
    let mut client = Client::new(GpuFrontend::new(), transport);
    client.key_filter = key_filter;
    let mut app =
        App { client, session, config, instance, modifiers: ModifiersState::empty(), error: None };
    event_loop.run_app(&mut app).map_err(|e| RenderError::Device(e.to_string()))?;
    match app.error.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Wake the event loop whenever the daemon's socket has data.
fn watch(fd: std::os::fd::OwnedFd, proxy: winit::event_loop::EventLoopProxy<Wake>) {
    use rustix_lite::poll_readable;
    loop {
        if !poll_readable(&fd) {
            return;
        }
        if proxy.send_event(Wake).is_err() {
            return;
        }
        // The main thread has not drained the socket yet, so polling again
        // immediately would spin. A short pause bounds that without adding a
        // timer to the idle path.
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

/// A single `poll` call, kept here so this crate does not depend on rustix
/// for one function.
mod rustix_lite {
    use std::os::fd::{AsRawFd, OwnedFd};

    pub fn poll_readable(fd: &OwnedFd) -> bool {
        let mut pfd = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd; poll only writes `revents`.
        let rc = unsafe { libc::poll(&mut pfd, 1, -1) };
        rc >= 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    fn press(logical: WinitKey, mods: ModifiersState) -> Option<InputEvent> {
        translate_key(&logical, ElementState::Pressed, false, mods)
    }

    #[test]
    fn characters_carry_their_modifiers() {
        let event = press(WinitKey::Character(SmolStr::new("c")), ModifiersState::CONTROL);
        match event {
            Some(InputEvent::Key(key)) => {
                assert_eq!(key.key, Key::Char('c'));
                assert_eq!(key.mods, Modifiers::CTRL);
                assert_eq!(key.state, KeyState::Press);
            }
            other => panic!("expected a key, got {other:?}"),
        }
    }

    #[test]
    fn named_keys_map_across() {
        let cases = [
            (WinitNamed::Enter, NamedKey::Enter),
            (WinitNamed::Tab, NamedKey::Tab),
            (WinitNamed::Escape, NamedKey::Escape),
            (WinitNamed::ArrowUp, NamedKey::Up),
            (WinitNamed::PageDown, NamedKey::PageDown),
            (WinitNamed::F5, NamedKey::Function(5)),
            (WinitNamed::F12, NamedKey::Function(12)),
        ];
        for (winit_key, ours) in cases {
            match press(WinitKey::Named(winit_key), ModifiersState::empty()) {
                Some(InputEvent::Key(key)) => assert_eq!(key.key, Key::Named(ours)),
                other => panic!("{winit_key:?} did not translate: {other:?}"),
            }
        }
    }

    #[test]
    fn space_is_a_character_not_a_named_key() {
        // Otherwise ctrl+space, which sends NUL, has nowhere to go.
        match press(WinitKey::Named(WinitNamed::Space), ModifiersState::CONTROL) {
            Some(InputEvent::Key(key)) => {
                assert_eq!(key.key, Key::Char(' '));
                assert_eq!(key.mods, Modifiers::CTRL);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn modifier_keys_alone_are_not_input() {
        assert!(press(WinitKey::Named(WinitNamed::Shift), ModifiersState::empty()).is_none());
        assert!(press(WinitKey::Named(WinitNamed::Control), ModifiersState::empty()).is_none());
    }

    #[test]
    fn repeats_and_releases_are_distinguishable() {
        let a = WinitKey::Character(SmolStr::new("a"));
        let repeat = translate_key(&a, ElementState::Pressed, true, ModifiersState::empty());
        let release = translate_key(&a, ElementState::Released, false, ModifiersState::empty());
        match (repeat, release) {
            (Some(InputEvent::Key(r)), Some(InputEvent::Key(u))) => {
                assert_eq!(r.state, KeyState::Repeat);
                assert_eq!(u.state, KeyState::Release);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_gpu_frontend_reports_full_key_fidelity() {
        let frontend = GpuFrontend::new();
        assert_eq!(frontend.caps().key_tier, KeyTier::Full);
        assert_eq!(frontend.caps().frontend, FrontendKind::Gpu);
        assert!(!frontend.is_ready(), "no window yet");
    }

    #[test]
    fn queued_input_comes_back_in_order() {
        let mut frontend = GpuFrontend::new();
        frontend.push(InputEvent::Quit);
        frontend.push(InputEvent::Focus(true));
        assert_eq!(frontend.next_input(), Some(InputEvent::Quit));
        assert_eq!(frontend.next_input(), Some(InputEvent::Focus(true)));
        assert_eq!(frontend.next_input(), None);
    }
}
