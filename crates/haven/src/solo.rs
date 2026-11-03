//! `haven tui`: the full mux, no daemon, drawn into the terminal we were
//! started from — the standalone-TUI corner of the run-mode matrix
//! (`docs/PLAN.md` §3).
//!
//! It composites pane grids straight into a serializer snapshot rather than
//! going through `FrameModel`, because with no daemon in the way we have every
//! pane's style table right here and therefore full colour fidelity. The
//! `FrameModel` path takes over when the daemon ships those tables (M5).

use std::io;
use std::time::Duration;

use haven_client::input::{InputEvent, KeyEncoder};
use haven_core::{Grid, Style, StyleTable};
use haven_mux::{MuxRuntime, RuntimeEvent};
use haven_proto::{CellSize, PaneId, PaneRect};
use haven_pty::{Pty, PtyConfig, PtySize};
use haven_tui::{raw, Decoder, HostCaps, Pushed, RawMode};
use haven_vtwrite::{Caps, RenderCell, Serializer, Snapshot};
use rustix::event::{poll, PollFd, PollFlags, Timespec};

use crate::keys::{self, Action};
use haven_client::bindings::{Bindings, Table};
use haven_mux::layout::Direction;

/// One row at the bottom for the status line.
const STATUS_ROWS: u16 = 1;

pub fn run(cfg: PtyConfig, host: HostCaps) -> io::Result<()> {
    let stdin = rustix::stdio::stdin();
    let stdout = rustix::stdio::stdout();
    let (cols, rows, px, py) = raw::window_size(stdin)?;

    // Raw mode first, so a failure to spawn still leaves a sane terminal.
    let _raw = RawMode::enter(Pushed {
        alt_screen: true,
        bracketed_paste: true,
        focus_events: false,
        mouse: false,
        synchronized_output: host.synchronized_output,
        kitty_keyboard: false,
    })?;
    haven_tui::install_sigwinch();

    let mut screen = CellSize { cols, rows, pixel_width: px, pixel_height: py };
    let mut mux =
        MuxRuntime::new(haven_proto::SessionId(1), "haven", pane_area(screen), move |size| {
            spawn_pane(&cfg, size)
        })?;

    let mut serializer = Serializer::new(Caps {
        depth: host.depth,
        synchronized_output: host.synchronized_output,
        underline_styles: host.underline_styles,
        utf8: host.utf8,
    });
    let mut decoder = Decoder::default();
    decoder.tier = host.key_tier();
    let mut encoder = KeyEncoder::new(host.key_tier());
    let mut prefix_armed = false;
    // Standalone reads the same config the daemon would: a key that behaved
    // one way with a server and another without one would be a trap.
    let bindings = {
        let config = haven_daemon::config::ConfigWatcher::new(
            haven_daemon::default_config_path(),
            haven_daemon::default_theme_dir(),
        );
        let table = config.keys().clone();
        Bindings::default().with(Some(&table.prefix), &table.prefix_table, false).with(
            None,
            &table.root,
            true,
        )
    };
    let mut buf = [0u8; 65536];
    let mut dirty = true;

    loop {
        if haven_tui::take_resized() {
            let (cols, rows, px, py) = raw::window_size(stdin)?;
            screen = CellSize { cols, rows, pixel_width: px, pixel_height: py };
            mux.resize(pane_area(screen))?;
            serializer.invalidate();
            dirty = true;
        }

        // Drain pane output first: a frame is drawn after draining, never per
        // chunk, so a noisy pane cannot starve input.
        for event in mux.pump(Duration::from_millis(8))? {
            match event {
                RuntimeEvent::Dirty => dirty = true,
                RuntimeEvent::Bell(_) => {
                    // Pass the bell through to the host, which knows the
                    // user's preference about sound and urgency hints.
                    raw::write_all(stdout, b"\x07")?;
                }
                RuntimeEvent::SessionEnded => return Ok(()),
            }
        }
        if mux.is_empty() {
            return Ok(());
        }

        if stdin_ready(stdin)? {
            match rustix::io::read(stdin, &mut buf) {
                Ok(0) => {}
                Ok(n) => decoder.push(&buf[..n]),
                Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {}
                Err(e) => return Err(e.into()),
            }
        }

        // Mirror the focused pane's cursor-key mode, or arrows type letters.
        encoder.app_cursor = mux
            .pane(mux.session.active_pane())
            .is_some_and(|p| p.emulator.term.modes.app_cursor_keys);

        while let Some(event) = decoder.next_event() {
            match event {
                InputEvent::Key(key) => {
                    if prefix_armed {
                        prefix_armed = false;
                        // The same table the attached client uses. Two key
                        // tables would be two behaviours for one keymap.
                        match bindings
                            .command(Table::Prefix, &key)
                            .and_then(|c| keys::parse_command(c).ok())
                        {
                            Some(Action::Detach) => return Ok(()),
                            Some(Action::SendPrefix) => {
                                mux.send(&encoder.encode(&key))?;
                            }
                            Some(action) => {
                                if apply(&mut mux, action)? {
                                    return Ok(());
                                }
                                serializer.invalidate();
                                dirty = true;
                            }
                            // An unbound key after the prefix does nothing
                            // rather than leaking into the pane.
                            None => {}
                        }
                        continue;
                    }
                    if bindings.is_prefix(&key) {
                        prefix_armed = true;
                        continue;
                    }
                    let bytes = encoder.encode(&key);
                    if !bytes.is_empty() {
                        mux.send(&bytes)?;
                    }
                }
                InputEvent::Paste(text) => {
                    let bracketed = mux
                        .pane(mux.session.active_pane())
                        .is_some_and(|p| p.emulator.term.modes.bracketed_paste);
                    if bracketed {
                        mux.send(b"\x1b[200~")?;
                        mux.send(text.as_bytes())?;
                        mux.send(b"\x1b[201~")?;
                    } else {
                        mux.send(text.as_bytes())?;
                    }
                }
                InputEvent::Resize(_) | InputEvent::Focus(_) | InputEvent::Mouse(_) => {}
                InputEvent::Quit => return Ok(()),
            }
        }

        if dirty {
            let (snapshot, cursor) = composite(&mux, screen, prefix_armed);
            let frame = serializer.frame_snapshot(snapshot, cursor);
            if !frame.is_empty() {
                raw::write_all(stdout, &frame)?;
            }
            dirty = false;
        }
    }
}

/// Apply an action. Returns true when the session is over.
fn apply<F: FnMut(PtySize) -> io::Result<Pty>>(
    mux: &mut MuxRuntime<F>,
    action: Action,
) -> io::Result<bool> {
    match action {
        Action::SplitColumns | Action::SplitRows => {
            let dir = keys::split_direction(action).expect("split action");
            mux.split(dir)?;
        }
        Action::Focus(edge) => {
            mux.focus(edge)?;
        }
        Action::Resize(edge) => {
            mux.resize_active(edge, 0.05)?;
        }
        Action::ClosePane => return mux.close_active(),
        Action::ToggleZoom => mux.toggle_zoom()?,
        Action::NewWindow => {
            mux.new_window()?;
        }
        Action::NextWindow => mux.next_window()?,
        Action::PreviousWindow => mux.previous_window()?,
        Action::SelectWindow(index) => {
            mux.select_window(index)?;
        }
        Action::ApplyLayout(layout) => mux.apply_named_layout(layout)?,
        Action::LastWindow => {
            mux.last_window()?;
        }
        Action::LastPane => {
            mux.session.active_window_mut().last_pane();
        }
        Action::SwapPane(edge) => {
            mux.swap_pane(edge)?;
        }
        Action::RotateWindow { backwards } => {
            mux.rotate_window(backwards)?;
        }
        Action::BreakPane => {
            mux.break_pane()?;
        }
        Action::JoinPane { window, columns } => {
            let dir = if columns { Direction::Columns } else { Direction::Rows };
            mux.join_pane(window, dir)?;
        }
        Action::CloseWindow => return mux.close_window(),
        Action::RespawnPane => {
            mux.respawn_active()?;
        }
        Action::SendKeys { pane, bytes } => {
            let target = pane.map(haven_proto::PaneId).unwrap_or_else(|| mux.session.active_pane());
            mux.send_to(target, &bytes)?;
        }
        Action::SynchronizePanes { on } => {
            let window = mux.session.active_window_mut();
            window.synchronized = on.unwrap_or(!window.synchronized);
        }
        Action::NextLayout => mux.next_layout()?,
        Action::Rename { window, name } => {
            let id = window.then(|| mux.session.active_window().id);
            mux.session.rename(id, &name);
        }
        // Standalone mode has no command line yet; the prefix table is the
        // whole interface there.
        // Standalone mode has no command line and no session list: there is
        // exactly one session, and it is this process.
        // Standalone mode is one session, and it is this process: there is
        // nowhere for a second one to live.
        Action::Detach
        | Action::SendPrefix
        | Action::EnterCommand(_)
        | Action::Choose
        | Action::NewSession
        // Standalone mode draws straight from the grid, with no view to move
        // and no client to hold a paste buffer.
        | Action::Scroll(_)
        | Action::Copy
        | Action::Paste => {}
    }
    Ok(false)
}

/// The area available to panes: the screen minus the status line.
fn pane_area(screen: CellSize) -> CellSize {
    CellSize { rows: screen.rows.saturating_sub(STATUS_ROWS).max(1), ..screen }
}

fn spawn_pane(cfg: &PtyConfig, size: PtySize) -> io::Result<Pty> {
    Pty::spawn(&PtyConfig { size, ..cfg.clone() })
}

fn stdin_ready(stdin: rustix::fd::BorrowedFd<'_>) -> io::Result<bool> {
    let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
    let zero = Timespec { tv_sec: 0, tv_nsec: 0 };
    match poll(&mut fds, Some(&zero)) {
        Ok(0) => Ok(false),
        Ok(_) => Ok(fds[0].revents().contains(PollFlags::IN)),
        Err(rustix::io::Errno::INTR) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Flatten every visible pane, the separators and the status line onto one
/// screen-sized snapshot.
pub fn composite<F>(
    mux: &MuxRuntime<F>,
    screen: CellSize,
    prefix_armed: bool,
) -> (Snapshot, (usize, usize)) {
    let cols = screen.cols as usize;
    let rows = screen.rows as usize;
    let mut out: Snapshot = vec![vec![RenderCell::blank(); cols]; rows];
    let active = mux.session.active_pane();
    let mut cursor = (0usize, 0usize);

    for (rect, pane) in mux.visible() {
        blit(&mut out, rect, pane.grid(), pane.styles(), screen);
        draw_separators(&mut out, rect, screen);
        if rect.pane == active {
            let grid_cursor = pane.grid().cursor;
            cursor = (
                (rect.row as usize + grid_cursor.row).min(rows.saturating_sub(1)),
                (rect.col as usize + grid_cursor.col).min(cols.saturating_sub(1)),
            );
        }
    }

    if rows > 0 {
        let status = status_line(mux, active, prefix_armed, cols);
        let row = rows - 1;
        for (col, cell) in status.into_iter().enumerate().take(cols) {
            out[row][col] = cell;
        }
    }
    (out, cursor)
}

/// Copy a pane's cells into the screen, clipped to its rectangle.
fn blit(out: &mut Snapshot, rect: PaneRect, grid: &Grid, styles: &StyleTable, screen: CellSize) {
    let max_col = (rect.col as usize + rect.cols as usize).min(screen.cols as usize);
    for (r, row) in grid.rows().iter().enumerate().take(rect.rows as usize) {
        let screen_row = rect.row as usize + r;
        if screen_row >= out.len() {
            break;
        }
        for (c, cell) in row.cells.iter().enumerate() {
            let col = rect.col as usize + c;
            if col >= max_col {
                break;
            }
            out[screen_row][col] = RenderCell {
                text: grid.cell_text(cell).into_owned(),
                style: styles.get(cell.style),
                wide: cell.flags.contains(haven_core::CellFlags::WIDE),
                spacer: cell.flags.contains(haven_core::CellFlags::WIDE_SPACER),
            };
        }
    }
}

fn draw_separators(out: &mut Snapshot, rect: PaneRect, screen: CellSize) {
    let right = rect.col as usize + rect.cols as usize;
    if right < screen.cols as usize {
        for r in 0..rect.rows as usize {
            let row = rect.row as usize + r;
            if row < out.len() {
                out[row][right] = RenderCell::new("│", Style::default());
            }
        }
    }
    let bottom = rect.row as usize + rect.rows as usize;
    if bottom + (STATUS_ROWS as usize) < screen.rows as usize {
        let max_col = right.min(screen.cols as usize);
        for col in rect.col as usize..max_col {
            if bottom < out.len() {
                out[bottom][col] = RenderCell::new("─", Style::default());
            }
        }
    }
}

/// Session name, window tabs, the focused pane's title, and the prefix
/// indicator. The built-in status line becomes a plugin at M3.5; it is written
/// as a pure function of state so that move is a relocation, not a rewrite.
fn status_line<F>(
    mux: &MuxRuntime<F>,
    active: PaneId,
    prefix_armed: bool,
    cols: usize,
) -> Vec<RenderCell> {
    use haven_core::{Attrs, Color};

    let normal = Style { fg: Color::Indexed(7), bg: Color::Indexed(0), ..Default::default() };
    let highlight = Style {
        fg: Color::Indexed(0),
        bg: Color::Indexed(4),
        attrs: Attrs::BOLD,
        ..Default::default()
    };

    let mut cells: Vec<RenderCell> = Vec::with_capacity(cols);
    let mut push = |text: &str, style: Style| {
        for ch in text.chars() {
            cells.push(RenderCell::new(ch.to_string(), style));
        }
    };

    push(&format!(" {} ", mux.session.name), highlight);
    for (index, window) in mux.session.windows.iter().enumerate() {
        let label = format!(
            " {}:{}{} ",
            index + 1,
            window.title.clone().unwrap_or_else(|| "shell".into()),
            if window.zoomed.is_some() { "Z" } else { "" },
        );
        let style = if index == mux.session.active { highlight } else { normal };
        push(&label, style);
    }

    if prefix_armed {
        push(" PREFIX ", highlight);
    }
    if let Some(title) = mux.pane(active).and_then(|p| p.title()) {
        push(&format!(" {title}"), normal);
    }

    cells.resize(cols, RenderCell::new(" ", normal));
    cells
}
