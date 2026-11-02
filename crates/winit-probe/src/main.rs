//! A standalone winit/wgpu probe, to find out what stalls a window manager.
//!
//! Haven's window appears to freeze AwesomeWM for seconds when switching
//! tags. There are at least three candidates and they are indistinguishable
//! from the outside:
//!
//! 1. winit itself — its X11 event handling, or the `_NET_WM_SYNC_REQUEST`
//!    handshake it performs on resize.
//! 2. wgpu's surface — with `PresentMode::Fifo`, acquiring a frame waits for a
//!    vblank, and an unmapped window never gets one. That wait happens *inside*
//!    the event loop, so while it blocks nothing answers the X server.
//! 3. Something else entirely: a compositor, the driver, the application's own
//!    work.
//!
//! So this program does nothing but open a window, and lets each of those be
//! turned off independently. `--no-gpu` removes wgpu from the picture
//! completely: if the stall survives that, winit is the culprit and Haven's
//! renderer is innocent.
//!
//! The important output is not the window. It is the watchdog: a second thread
//! reports whenever the main thread has not reached the event loop for a
//! while, and every blocking call is timed separately, so a stall names
//! itself instead of being inferred.
//!
//! Run it, switch tags a few times, come back, and read what it printed.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const USAGE: &str = "\
winit-probe — is it winit, or is it the surface?

usage:
  winit-probe [options]

options:
  --no-gpu              open a window and nothing else; no wgpu, no surface,
                        no presenting. If tag switching still stalls, the
                        cause is winit or X, not the renderer.
  --present-mode <m>    fifo (default), mailbox, immediate, auto-vsync,
                        auto-no-vsync. `fifo` is vsync and the suspect.
  --backend <b>         vulkan, gl, or all (default). On X11 a Vulkan
                        swapchain can block for seconds on a window the WM
                        has unmapped; GL goes through a different path.
  --power <p>           high (default) or low. On a hybrid laptop this picks
                        the discrete or the integrated GPU. Without it,
                        changing backend also changes GPU, and a result
                        cannot tell you which one mattered.
  --skip-occluded       stop drawing while the window is hidden, which is what
                        a fixed application should do.
  --continuous          redraw as fast as possible instead of only when asked,
                        which is the worst case for a hidden window.
  --poll                use ControlFlow::Poll instead of Wait.
  --stall-ms <n>        report a stall longer than this (default 100).
  --seconds <n>         quit after n seconds, so it can be run unattended.
  -h, --help            this message

Switch away to another tag, wait, come back. Then read the STALL lines.
";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PresentArg {
    Fifo,
    Mailbox,
    Immediate,
    AutoVsync,
    AutoNoVsync,
}

impl PresentArg {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "fifo" => Self::Fifo,
            "mailbox" => Self::Mailbox,
            "immediate" => Self::Immediate,
            "auto-vsync" => Self::AutoVsync,
            "auto-no-vsync" => Self::AutoNoVsync,
            _ => return None,
        })
    }

    fn to_wgpu(self) -> wgpu::PresentMode {
        match self {
            Self::Fifo => wgpu::PresentMode::Fifo,
            Self::Mailbox => wgpu::PresentMode::Mailbox,
            Self::Immediate => wgpu::PresentMode::Immediate,
            Self::AutoVsync => wgpu::PresentMode::AutoVsync,
            Self::AutoNoVsync => wgpu::PresentMode::AutoNoVsync,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Options {
    gpu: bool,
    present: PresentArg,
    skip_occluded: bool,
    continuous: bool,
    poll: bool,
    stall: Duration,
    seconds: Option<u64>,
    backends: wgpu::Backends,
    power: wgpu::PowerPreference,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            gpu: true,
            present: PresentArg::Fifo,
            skip_occluded: false,
            continuous: false,
            poll: false,
            stall: Duration::from_millis(100),
            seconds: None,
            backends: wgpu::Backends::all(),
            power: wgpu::PowerPreference::HighPerformance,
        }
    }
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-gpu" => o.gpu = false,
            "--skip-occluded" => o.skip_occluded = true,
            "--continuous" => o.continuous = true,
            "--poll" => o.poll = true,
            "--present-mode" => {
                let v = it.next().ok_or("--present-mode needs a value")?;
                o.present = PresentArg::parse(&v).ok_or(format!("unknown present mode {v:?}"))?;
            }
            "--backend" => {
                let v = it.next().ok_or("--backend needs a value")?;
                o.backends = match v.as_str() {
                    "vulkan" => wgpu::Backends::VULKAN,
                    "gl" => wgpu::Backends::GL,
                    "all" => wgpu::Backends::all(),
                    other => return Err(format!("unknown backend {other:?}")),
                };
            }
            "--power" => {
                let v = it.next().ok_or("--power needs a value")?;
                o.power = match v.as_str() {
                    "high" => wgpu::PowerPreference::HighPerformance,
                    "low" => wgpu::PowerPreference::LowPower,
                    other => return Err(format!("unknown power preference {other:?}")),
                };
            }
            "--seconds" => {
                let v = it.next().ok_or("--seconds needs a value")?;
                o.seconds = Some(v.parse().map_err(|_| "bad --seconds")?);
            }
            "--stall-ms" => {
                let v = it.next().ok_or("--stall-ms needs a value")?;
                o.stall = Duration::from_millis(v.parse().map_err(|_| "bad --stall-ms")?);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(o)
}

/// Shared with the watchdog thread: when the main thread was last alive, as
/// milliseconds since start.
#[derive(Debug)]
struct Beat {
    /// When the main thread entered a call it is expected to return from
    /// promptly, or `0` when it is legitimately idle. Blocking in the event
    /// loop waiting for events is correct; blocking inside `present()` is
    /// not, and only the second is a stall.
    busy_since: AtomicU64,
    running: AtomicBool,
    /// What the main thread is doing, so a stall can name the call it is in.
    doing: std::sync::Mutex<&'static str>,
    frames: AtomicU64,
    worst_ms: AtomicU64,
    worst_what: std::sync::Mutex<&'static str>,
}

fn millis(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

/// The whole point of the program: notice that the main thread is wedged, from
/// a thread that cannot itself be wedged by it.
fn watchdog(beat: Arc<Beat>, start: Instant, limit: Duration) {
    let mut reported = 0u64;
    let mut next_tick = Duration::from_secs(5);
    let limit_ms = limit.as_millis() as u64;
    while beat.running.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
        // A periodic line, so a run that is killed rather than closed still
        // leaves evidence behind.
        if start.elapsed() >= next_tick {
            next_tick += Duration::from_secs(5);
            eprintln!(
                "[{:>7}ms] alive: {} frames, worst blocking call {}ms in {}",
                millis(start),
                beat.frames.load(Ordering::Relaxed),
                beat.worst_ms.load(Ordering::Relaxed),
                beat.worst_what.lock().unwrap(),
            );
        }
        let busy = beat.busy_since.load(Ordering::Relaxed);
        if busy == 0 {
            continue; // Idle in the event loop, which is where it should be.
        }
        let now = millis(start);
        let gap = now.saturating_sub(busy);
        if gap >= limit_ms && busy != reported {
            let doing = *beat.doing.lock().unwrap();
            eprintln!("[{now:>7}ms] STALL: still inside {doing} after {gap}ms");
            reported = busy;
        }
    }
}

struct App {
    opts: Options,
    start: Instant,
    beat: Arc<Beat>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    occluded: bool,
    frames: u64,
    worst: (u64, &'static str),
}

#[derive(Debug)]
struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App").field("opts", &self.opts).field("frames", &self.frames).finish()
    }
}

impl App {
    fn new(opts: Options, start: Instant, beat: Arc<Beat>) -> Self {
        Self {
            opts,
            start,
            beat,
            window: None,
            gpu: None,
            occluded: false,
            frames: 0,
            worst: (0, "nothing"),
        }
    }

    fn log(&self, msg: impl std::fmt::Display) {
        eprintln!("[{:>7}ms] {msg}", millis(self.start));
    }

    /// What the window itself claims. `Occluded` is driven by X11's
    /// `VisibilityNotify`, which an *unmapped* window never sends — so under a
    /// tag-switching WM it never fires and cannot be the signal for "stop
    /// drawing". If `is_visible()` tracks the unmap, it can be.
    fn visibility(&self) -> String {
        match self.window.as_ref().and_then(|w| w.is_visible()) {
            Some(v) => format!("is_visible={v}"),
            None => "is_visible=unknown".to_string(),
        }
    }

    /// Run `f`, timing it, and say so if it took long enough to be a freeze.
    fn timed<T>(&mut self, what: &'static str, f: impl FnOnce(&mut Self) -> T) -> T {
        *self.beat.doing.lock().unwrap() = what;
        let t = Instant::now();
        self.beat.busy_since.store(millis(self.start).max(1), Ordering::Relaxed);
        let out = f(self);
        let took = t.elapsed();
        self.beat.busy_since.store(0, Ordering::Relaxed);
        let ms = took.as_millis() as u64;
        if took >= self.opts.stall {
            let vis = self.visibility();
            self.log(format!("SLOW: {what} took {ms}ms ({vis})"));
        }
        if ms > self.worst.0 {
            self.worst = (ms, what);
            self.beat.worst_ms.store(ms, Ordering::Relaxed);
            *self.beat.worst_what.lock().unwrap() = what;
        }
        out
    }

    fn init_gpu(&mut self, window: Arc<Window>) {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: self.opts.backends,
            ..Default::default()
        });
        let surface = match instance.create_surface(window) {
            Ok(s) => s,
            Err(e) => {
                self.log(format!("create_surface failed: {e}"));
                return;
            }
        };
        let adapter =
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: self.opts.power,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })) {
                Ok(a) => a,
                Err(e) => {
                    self.log(format!("no adapter: {e}"));
                    return;
                }
            };
        self.log(format!("adapter: {:?}", adapter.get_info()));

        let (device, queue) =
            match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("winit-probe"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                trace: wgpu::Trace::Off,
            })) {
                Ok(v) => v,
                Err(e) => {
                    self.log(format!("no device: {e}"));
                    return;
                }
            };

        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.first().copied().unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb);
        let supported = caps.present_modes.clone();
        let asked = self.opts.present.to_wgpu();
        // Configuring an unsupported present mode is a validation error, not
        // a warning: NVIDIA on X11 offers only [Fifo, Immediate], so asking
        // for Mailbox aborted the process instead of running the test.
        let wanted = if supported.contains(&asked) {
            asked
        } else {
            let fallback = if supported.contains(&wgpu::PresentMode::Immediate) {
                wgpu::PresentMode::Immediate
            } else {
                wgpu::PresentMode::Fifo
            };
            self.log(format!(
                "present mode {asked:?} is NOT supported here (supported: {supported:?}) \
                 — falling back to {fallback:?}"
            ));
            fallback
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wanted,
            alpha_mode: caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        self.log(format!("surface: {format:?}, present {wanted:?}"));
        self.gpu = Some(Gpu { surface, device, queue, config });
    }

    /// One frame: clear to a colour that changes, so a frozen window is
    /// visibly frozen rather than merely suspected of it.
    fn draw(&mut self) {
        if self.gpu.is_none() {
            return;
        }
        if self.opts.skip_occluded && self.occluded {
            return;
        }
        self.frames += 1;
        self.beat.frames.store(self.frames, Ordering::Relaxed);
        let tint = (self.frames % 120) as f64 / 120.0;

        // Each of these three can block, and they block for different
        // reasons, so they are timed apart. On a hidden window with `fifo`,
        // the wait shows up in acquire or in present, never in submit.
        let frame = self.timed("surface.get_current_texture()", |me| {
            let gpu = me.gpu.as_ref().expect("checked above");
            gpu.surface.get_current_texture()
        });
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                self.log(format!("acquire failed: {e:?} (reconfiguring)"));
                if let Some(gpu) = &mut self.gpu {
                    gpu.surface.configure(&gpu.device, &gpu.config);
                }
                return;
            }
        };

        self.timed("queue.submit()", |me| {
            let gpu = me.gpu.as_ref().expect("checked above");
            let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("probe") });
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: tint,
                            g: 0.1,
                            b: 1.0 - tint,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            gpu.queue.submit(Some(encoder.finish()));
        });

        self.timed("frame.present()", |_| frame.present());
    }

    fn report(&self) {
        eprintln!();
        eprintln!("--- summary ---------------------------------------------");
        eprintln!("frames drawn:   {}", self.frames);
        eprintln!("worst blocking call: {}ms in {}", self.worst.0, self.worst.1);
        if !self.opts.gpu {
            eprintln!();
            eprintln!("This run had no wgpu at all. If the window manager still");
            eprintln!("stalled, the cause is winit or X — not the renderer.");
        } else if self.worst.0 >= 100 {
            eprintln!();
            eprintln!("A blocking call of {}ms sits inside the event loop, so", self.worst.0);
            eprintln!("nothing answered the X server for that long. Try");
            eprintln!("`--skip-occluded`, and `--present-mode mailbox` to see");
            eprintln!("whether vsync is what is waiting.");
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("winit-probe")
            .with_inner_size(winit::dpi::PhysicalSize::new(800, 500));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.log(format!("create_window failed: {e}"));
                event_loop.exit();
                return;
            }
        };
        self.log("window created");
        if self.opts.gpu {
            self.init_gpu(window.clone());
        } else {
            self.log("running with no wgpu: winit only");
        }
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Handling an event should be quick. If winit itself is what stalls —
        // the `_NET_WM_SYNC_REQUEST` handshake on reconfigure, say — the delay
        // lands here rather than in any call this program makes, which is
        // exactly the distinction `--no-gpu` is for.
        *self.beat.doing.lock().unwrap() = "a window event";
        self.beat.busy_since.store(millis(self.start).max(1), Ordering::Relaxed);
        match event {
            WindowEvent::CloseRequested => {
                self.log("close requested");
                event_loop.exit();
            }
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                let vis = self.visibility();
                self.log(format!(
                    "Occluded({occluded}) {vis}{}",
                    if self.opts.skip_occluded { " — will stop drawing" } else { "" }
                ));
                if !occluded {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::Focused(f) => {
                let vis = self.visibility();
                self.log(format!("Focused({f}) {vis}"));
            }
            WindowEvent::Resized(size) => {
                self.log(format!("Resized({}x{})", size.width, size.height));
                if let Some(gpu) = &mut self.gpu {
                    if size.width > 0 && size.height > 0 {
                        gpu.config.width = size.width;
                        gpu.config.height = size.height;
                        gpu.surface.configure(&gpu.device, &gpu.config);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.draw();
                if self.opts.continuous && !(self.opts.skip_occluded && self.occluded) {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            _ => {}
        }
        self.beat.busy_since.store(0, Ordering::Relaxed);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.beat.busy_since.store(0, Ordering::Relaxed);
        if let Some(limit) = self.opts.seconds {
            if self.start.elapsed() >= Duration::from_secs(limit) {
                self.log("time limit reached");
                event_loop.exit();
                return;
            }
            // Waiting forever would never notice the deadline.
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                std::time::Instant::now() + Duration::from_millis(250),
            ));
            return;
        }
        event_loop.set_control_flow(if self.opts.poll {
            ControlFlow::Poll
        } else {
            ControlFlow::Wait
        });
    }
}

fn main() {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("winit-probe: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let start = Instant::now();
    let beat = Arc::new(Beat {
        busy_since: AtomicU64::new(0),
        running: AtomicBool::new(true),
        doing: std::sync::Mutex::new("startup"),
        frames: AtomicU64::new(0),
        worst_ms: AtomicU64::new(0),
        worst_what: std::sync::Mutex::new("nothing"),
    });

    eprintln!("winit-probe: {opts:?}");
    eprintln!("Switch to another tag, wait a few seconds, switch back.");
    eprintln!("Ctrl+C or close the window when done.\n");

    let dog = {
        let beat = Arc::clone(&beat);
        std::thread::spawn(move || watchdog(beat, start, opts.stall))
    };

    let event_loop = match EventLoop::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("winit-probe: cannot create an event loop: {e}");
            std::process::exit(1);
        }
    };
    let mut app = App::new(opts, start, Arc::clone(&beat));
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("winit-probe: event loop: {e}");
    }

    beat.running.store(false, Ordering::Relaxed);
    let _ = dog.join();
    app.report();
}
