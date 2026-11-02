//! TUI frontend: the same sessions, drawn into whatever terminal we were
//! started from. See `docs/PLAN.md` §13.
//!
//! It is a first-class client, not a fallback: it draws the same
//! [`FrameModel`] the GPU frontend does, composites panes and widgets itself,
//! and hands the result to `haven-vtwrite`.

pub mod decode;
pub mod probe;
pub mod raw;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use haven_client::frame::FrameModel;
use haven_client::frontend::{Frontend, PresentError};
use haven_client::input::InputEvent;
use haven_core::Style;
use haven_proto::{CellSize, ClientCaps, FrontendKind, KeyTier};
use haven_vtwrite::{Caps, RenderCell, Serializer, Snapshot};

pub use decode::Decoder;
pub use probe::HostCaps;
pub use raw::{Pushed, RawMode};

/// Set by the `SIGWINCH` handler; drained by [`TuiFrontend::next_input`].
static RESIZED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigwinch(_: i32) {
    RESIZED.store(true, Ordering::Relaxed);
}

#[derive(Debug)]
pub struct TuiFrontend {
    caps: ClientCaps,
    serializer: Serializer,
    decoder: Decoder,
    raw: Option<RawMode>,
    /// Scratch buffer, reused so a frame does not allocate per read.
    read_buf: [u8; 4096],
}

impl TuiFrontend {
    /// Take over the terminal. The returned frontend restores it on drop.
    pub fn enter(host: HostCaps) -> io::Result<Self> {
        let (cols, rows, px, py) = raw::window_size(rustix::stdio::stdin())?;
        let pushed = Pushed {
            alt_screen: true,
            bracketed_paste: true,
            focus_events: host.focus_events,
            mouse: host.mouse_sgr,
            synchronized_output: host.synchronized_output,
            kitty_keyboard: host.kitty_keyboard,
        };
        let raw = RawMode::enter(pushed)?;
        install_sigwinch();

        let caps = ClientCaps {
            frontend: FrontendKind::Tui,
            size: CellSize { cols, rows, pixel_width: px, pixel_height: py },
            color_depth: host.proto_depth(),
            key_tier: host.key_tier(),
            synchronized_output: host.synchronized_output,
            clipboard_osc52: host.clipboard_osc52,
            hyperlinks: host.hyperlinks,
        };
        let mut decoder = Decoder::default();
        decoder.tier = caps.key_tier;

        Ok(Self {
            caps,
            serializer: Serializer::new(Caps {
                depth: host.depth,
                synchronized_output: host.synchronized_output,
                underline_styles: host.underline_styles,
                utf8: host.utf8,
            }),
            decoder,
            raw: Some(raw),
            read_buf: [0u8; 4096],
        })
    }

    /// A frontend that writes nowhere, for tests: no terminal is touched.
    pub fn detached(size: CellSize, host: HostCaps) -> Self {
        let mut decoder = Decoder::default();
        decoder.tier = host.key_tier();
        Self {
            caps: ClientCaps {
                frontend: FrontendKind::Tui,
                size,
                color_depth: host.proto_depth(),
                key_tier: host.key_tier(),
                synchronized_output: host.synchronized_output,
                clipboard_osc52: host.clipboard_osc52,
                hyperlinks: host.hyperlinks,
            },
            serializer: Serializer::new(Caps {
                depth: host.depth,
                synchronized_output: host.synchronized_output,
                underline_styles: host.underline_styles,
                utf8: host.utf8,
            }),
            decoder,
            raw: None,
            read_buf: [0u8; 4096],
        }
    }

    /// Feed bytes as if they had arrived from the terminal. Tests and the
    /// `--solo` harness use this; it is also what makes the decoder reachable
    /// without a tty.
    pub fn feed_input(&mut self, bytes: &[u8]) {
        self.decoder.push(bytes);
    }

    /// The bytes this frame would write, without writing them.
    pub fn render(&mut self, frame: &FrameModel) -> Vec<u8> {
        let (snapshot, cursor) = composite(frame);
        self.serializer.frame_snapshot(snapshot, cursor)
    }

    fn poll_stdin(&mut self) -> io::Result<()> {
        use rustix::event::{poll, PollFd, PollFlags, Timespec};
        let stdin = rustix::stdio::stdin();
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        // Zero timeout: input polling must never block a render.
        let zero = Timespec { tv_sec: 0, tv_nsec: 0 };
        match poll(&mut fds, Some(&zero)) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        match rustix::io::read(stdin, &mut self.read_buf) {
            Ok(0) => Ok(()),
            Ok(n) => {
                let bytes: Vec<u8> = self.read_buf[..n].to_vec();
                self.decoder.push(&bytes);
                Ok(())
            }
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl Frontend for TuiFrontend {
    fn caps(&self) -> ClientCaps {
        self.caps
    }

    fn next_input(&mut self) -> Option<InputEvent> {
        if RESIZED.swap(false, Ordering::Relaxed) {
            if let Ok((cols, rows, px, py)) = raw::window_size(rustix::stdio::stdin()) {
                self.caps.size = CellSize { cols, rows, pixel_width: px, pixel_height: py };
                // A resize invalidates everything we believe about the screen.
                self.serializer.invalidate();
                return Some(InputEvent::Resize(self.caps.size));
            }
        }
        if self.raw.is_some() {
            let _ = self.poll_stdin();
        }
        self.decoder.next_event()
    }

    fn present(&mut self, frame: &FrameModel) -> Result<(), PresentError> {
        let bytes = self.render(frame);
        if bytes.is_empty() {
            return Ok(());
        }
        if self.raw.is_none() {
            return Ok(());
        }
        raw::write_all(rustix::stdio::stdout(), &bytes).map_err(|e| PresentError(e.to_string()))
    }
}

/// True once since the last call: the `SIGWINCH` flag, drained.
pub fn take_resized() -> bool {
    RESIZED.swap(false, Ordering::Relaxed)
}

/// Install the `SIGWINCH` handler. Idempotent.
pub fn install_sigwinch() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        // SAFETY: the handler only sets an atomic flag.
        unsafe {
            libc::signal(libc::SIGWINCH, handle_sigwinch as *const () as libc::sighandler_t);
        }
    });
}

/// Flatten panes and widgets onto one screen-sized grid of cells.
///
/// This is the TUI frontend's half of what the GPU frontend does with a
/// rasterizer: the daemon ships pane rectangles and rows, and placing them is
/// presentation, not mux state.
pub fn composite(frame: &FrameModel) -> (Snapshot, (usize, usize)) {
    let cols = frame.size.cols as usize;
    let rows = frame.size.rows as usize;
    let mut screen: Snapshot = vec![vec![RenderCell::blank(); cols]; rows];
    let mut cursor = (0usize, 0usize);

    let place = |screen: &mut Snapshot,
                 rect: &haven_proto::PaneRect,
                 pane_rows: &[haven_client::frame::FrameRow]| {
        for (r, row) in pane_rows.iter().enumerate() {
            let screen_row = rect.row as usize + r;
            if screen_row >= screen.len() {
                break;
            }
            let right_edge = (rect.col as usize + rect.cols as usize).min(cols);
            for span in &row.spans {
                let start = rect.col as usize + span.col as usize;
                for (col, ch) in (start..right_edge).zip(span.text.chars()) {
                    screen[screen_row][col] = RenderCell::new(ch.to_string(), span.style);
                }
            }
        }
    };

    for pane in frame.panes.values() {
        place(&mut screen, &pane.rect, &pane.rows);
        // Separators: a vertical rule to the right of every pane that does not
        // reach the screen edge.
        let right = pane.rect.col as usize + pane.rect.cols as usize;
        if right < cols {
            for r in 0..pane.rect.rows as usize {
                let screen_row = pane.rect.row as usize + r;
                if screen_row < rows {
                    screen[screen_row][right] = RenderCell::new("│", Style::default());
                }
            }
        }
        if Some(pane.id) == frame.focus {
            cursor = (
                (pane.rect.row as usize + pane.cursor.row).min(rows.saturating_sub(1)),
                (pane.rect.col as usize + pane.cursor.col).min(cols.saturating_sub(1)),
            );
        }
    }

    for widget in &frame.widgets {
        place(&mut screen, &widget.rect, &widget.rows);
    }

    (screen, cursor)
}

/// The key tier a host reporting these capabilities gives us.
pub fn tier_for(kitty_keyboard: bool, modify_other_keys: bool) -> KeyTier {
    match (kitty_keyboard, modify_other_keys) {
        (true, _) => KeyTier::Full,
        (false, true) => KeyTier::Extended,
        _ => KeyTier::Legacy,
    }
}
