//! The running side of the model: a pty and a terminal per pane.
//!
//! Separate from [`crate::session`] on purpose — the model stays a pure data
//! structure that tests can drive without spawning anything, and this module
//! is the part that owns file descriptors and children. It is also the part
//! the daemon will keep when the socket arrives (`docs/PLAN.md` §14).

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::process::ExitStatus;
use std::time::Duration;

use haven_core::{Emulator, Grid, StyleTable};
use haven_proto::{CellSize, PaneId, PaneRect, SessionId};
use haven_pty::{Pty, PtySize};
use rustix::event::{poll, PollFd, PollFlags, Timespec};

use crate::layout::{Direction, Edge};
use crate::session::{Session, SessionEvent};
use crate::window::NamedLayout;

/// One pane: its child process and its screen.
#[derive(Debug)]
pub struct PaneRuntime {
    pub pty: Pty,
    pub emulator: Emulator,
    pub exited: Option<ExitStatus>,
    /// The size we last told the child about, to avoid redundant `SIGWINCH`.
    size: PtySize,
}

impl PaneRuntime {
    pub fn grid(&self) -> &Grid {
        &self.emulator.term.grid
    }

    pub fn styles(&self) -> &StyleTable {
        &self.emulator.term.styles
    }

    /// The geometry the child was last told about.
    pub fn size(&self) -> PtySize {
        self.size
    }

    pub fn title(&self) -> Option<&str> {
        self.emulator.term.title.as_deref()
    }

    /// What the shell last said it was running, and whether it still is.
    /// Needs shell integration (`OSC 133`); without it there is nothing to
    /// report and the caller falls back to the title.
    pub fn command(&self) -> Option<(&str, bool)> {
        let term = &self.emulator.term;
        term.command.as_deref().map(|c| (c, term.command_running))
    }
}

/// What the caller should do about something that happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuntimeEvent {
    Dirty,
    Bell(PaneId),
    SessionEnded,
}

/// Everything a session needs to actually run, minus any presentation.
pub struct MuxRuntime<F> {
    pub session: Session,
    panes: BTreeMap<u32, PaneRuntime>,
    size: CellSize,
    spawn: F,
}

impl<F> std::fmt::Debug for MuxRuntime<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MuxRuntime")
            .field("session", &self.session)
            .field("panes", &self.panes.keys().collect::<Vec<_>>())
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// Read-only access needs no knowledge of how panes are spawned, so it lives
/// outside the bounded impl: a frontend compositing a frame should not have to
/// name the pty factory's type.
impl<F> MuxRuntime<F> {
    pub fn size(&self) -> CellSize {
        self.size
    }

    pub fn pane(&self, id: PaneId) -> Option<&PaneRuntime> {
        self.panes.get(&id.0)
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut PaneRuntime> {
        self.panes.get_mut(&id.0)
    }

    pub fn rects(&self) -> Vec<PaneRect> {
        self.session.active_window().rects(self.size)
    }

    /// Visible panes and their rectangles, in draw order.
    pub fn visible(&self) -> Vec<(PaneRect, &PaneRuntime)> {
        self.rects()
            .into_iter()
            .filter(|r| r.cols > 0 && r.rows > 0)
            .filter_map(|rect| self.panes.get(&rect.pane.0).map(|pane| (rect, pane)))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }
}

impl<F: FnMut(PtySize) -> io::Result<Pty>> MuxRuntime<F> {
    /// Start a session with one pane.
    pub fn new(id: SessionId, name: &str, size: CellSize, mut spawn: F) -> io::Result<Self> {
        let (session, first) = Session::new(id, name);
        let mut panes = BTreeMap::new();
        let pane_size = PtySize {
            cols: size.cols,
            rows: size.rows,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        };
        panes.insert(first.0, make_pane(&mut spawn, pane_size)?);
        let mut runtime = Self { session, panes, size, spawn };
        runtime.sync_sizes()?;
        Ok(runtime)
    }

    pub fn resize(&mut self, size: CellSize) -> io::Result<()> {
        self.size = size;
        self.sync_sizes()
    }

    /// Tell every pane the size of its own rectangle.
    ///
    /// Each pane is its own terminal, so a split means two children each with
    /// their own geometry — not one child that happens to be drawn twice.
    fn sync_sizes(&mut self) -> io::Result<()> {
        for rect in self.rects() {
            let Some(pane) = self.panes.get_mut(&rect.pane.0) else { continue };
            let target = PtySize {
                cols: rect.cols.max(1),
                rows: rect.rows.max(1),
                // Pixel geometry scales with the pane's share of the screen.
                pixel_width: scale(self.size.pixel_width, rect.cols, self.size.cols),
                pixel_height: scale(self.size.pixel_height, rect.rows, self.size.rows),
            };
            if pane.size == target {
                continue;
            }
            pane.emulator.term.resize(target.cols as usize, target.rows as usize);
            pane.pty.resize(target)?;
            pane.size = target;
        }
        Ok(())
    }

    /// Read from every pane that has output, answer its queries, and report
    /// whether anything changed.
    ///
    /// All panes are polled in one call: a frame is drawn after draining, not
    /// per chunk, so a noisy pane cannot starve the others or the input path.
    pub fn pump(&mut self, timeout: Duration) -> io::Result<Vec<RuntimeEvent>> {
        let mut events = Vec::new();
        let ids: Vec<u32> = self.panes.keys().copied().collect();

        let ready: Vec<u32> = {
            let masters: Vec<_> =
                ids.iter().filter_map(|id| self.panes.get(id).map(|p| p.pty.master())).collect();
            let mut fds: Vec<PollFd> =
                masters.iter().map(|m| PollFd::new(m, PollFlags::IN)).collect();
            if fds.is_empty() {
                return Ok(events);
            }
            let ts = Timespec {
                tv_sec: timeout.as_secs() as i64,
                tv_nsec: timeout.subsec_nanos() as i64,
            };
            match poll(&mut fds, Some(&ts)) {
                Ok(_) => ids
                    .iter()
                    .zip(fds.iter())
                    .filter(|(_, fd)| fd.revents().intersects(PollFlags::IN | PollFlags::HUP))
                    .map(|(id, _)| *id)
                    .collect(),
                Err(rustix::io::Errno::INTR) => Vec::new(),
                Err(e) => return Err(e.into()),
            }
        };

        let mut buf = [0u8; 65536];
        let mut finished: Vec<PaneId> = Vec::new();
        for id in ready {
            let Some(pane) = self.panes.get_mut(&id) else { continue };
            match pane.pty.read(&mut buf) {
                Ok(0) => finished.push(PaneId(id)),
                Ok(n) => {
                    let before = pane.emulator.term.bell_count;
                    pane.emulator.feed(&buf[..n]);
                    let reply = pane.emulator.term.take_reply();
                    if !reply.is_empty() {
                        pane.pty.write_all(&reply)?;
                    }
                    events.push(RuntimeEvent::Dirty);
                    if pane.emulator.term.bell_count > before {
                        events.push(RuntimeEvent::Bell(PaneId(id)));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => finished.push(PaneId(id)),
            }
        }

        // Children that exited independently of any user action.
        for id in self.panes.keys().copied().collect::<Vec<_>>() {
            let Some(pane) = self.panes.get_mut(&id) else { continue };
            if pane.exited.is_none() {
                if let Ok(Some(status)) = pane.pty.try_wait() {
                    pane.exited = Some(status);
                    finished.push(PaneId(id));
                }
            }
        }

        for pane in finished {
            if self.close(pane)? {
                events.push(RuntimeEvent::SessionEnded);
            }
            events.push(RuntimeEvent::Dirty);
        }
        Ok(events)
    }

    /// Send bytes to the focused pane.
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        let active = self.session.active_pane();
        if let Some(pane) = self.panes.get_mut(&active.0) {
            pane.pty.write_all(bytes)?;
        }
        Ok(())
    }

    /// Send bytes to a specific pane, regardless of focus. The daemon needs
    /// this for `send-keys`; tests need it to interrogate one child.
    pub fn send_to(&mut self, pane: PaneId, bytes: &[u8]) -> io::Result<()> {
        if let Some(runtime) = self.panes.get_mut(&pane.0) {
            runtime.pty.write_all(bytes)?;
        }
        Ok(())
    }

    pub fn split(&mut self, dir: Direction) -> io::Result<Option<PaneId>> {
        let Some((pane, _)) = self.session.split_active(dir, 0.5) else { return Ok(None) };
        let runtime =
            make_pane(&mut self.spawn, PtySize { cols: 1, rows: 1, ..Default::default() })?;
        self.panes.insert(pane.0, runtime);
        // The new pane's real size comes from the solver, not from the caller.
        self.sync_sizes()?;
        Ok(Some(pane))
    }

    pub fn new_window(&mut self) -> io::Result<PaneId> {
        let (_, pane, _) = self.session.new_window();
        let runtime =
            make_pane(&mut self.spawn, PtySize { cols: 1, rows: 1, ..Default::default() })?;
        self.panes.insert(pane.0, runtime);
        self.sync_sizes()?;
        Ok(pane)
    }

    /// Close a pane and its child. Returns whether the session is over.
    pub fn close(&mut self, pane: PaneId) -> io::Result<bool> {
        let events = self.session.close_pane(pane);
        if events.is_empty() {
            return Ok(false);
        }
        if let Some(mut runtime) = self.panes.remove(&pane.0) {
            if runtime.exited.is_none() {
                // Hang up the whole process group, not just the shell.
                let _ = runtime.pty.hangup(Duration::from_millis(100));
            }
        }
        let ended = events.contains(&SessionEvent::SessionEnded);
        if !ended {
            self.sync_sizes()?;
        }
        Ok(ended)
    }

    pub fn close_active(&mut self) -> io::Result<bool> {
        let active = self.session.active_pane();
        self.close(active)
    }

    pub fn focus(&mut self, edge: Edge) -> io::Result<bool> {
        Ok(self.session.focus(edge, self.size))
    }

    pub fn toggle_zoom(&mut self) -> io::Result<()> {
        self.session.toggle_zoom();
        // Zoom changes every pane's geometry, so children must be told.
        self.sync_sizes()
    }

    pub fn select_window(&mut self, index: usize) -> io::Result<bool> {
        let ok = self.session.select_window(index);
        if ok {
            self.sync_sizes()?;
        }
        Ok(ok)
    }

    pub fn next_window(&mut self) -> io::Result<()> {
        self.session.next_window();
        self.sync_sizes()
    }

    pub fn previous_window(&mut self) -> io::Result<()> {
        self.session.previous_window();
        self.sync_sizes()
    }

    pub fn resize_active(&mut self, edge: Edge, fraction: f32) -> io::Result<bool> {
        let ok = self.session.resize_active(edge, fraction);
        if ok {
            self.sync_sizes()?;
        }
        Ok(ok)
    }

    pub fn apply_named_layout(&mut self, layout: NamedLayout) -> io::Result<()> {
        self.session.apply_named_layout(layout);
        self.sync_sizes()
    }
}

fn make_pane<F: FnMut(PtySize) -> io::Result<Pty>>(
    spawn: &mut F,
    size: PtySize,
) -> io::Result<PaneRuntime> {
    let pty = spawn(size)?;
    Ok(PaneRuntime {
        pty,
        emulator: Emulator::new(size.cols as usize, size.rows as usize),
        exited: None,
        size,
    })
}

fn scale(total_pixels: u16, part: u16, whole: u16) -> u16 {
    if whole == 0 || total_pixels == 0 {
        return 0;
    }
    ((total_pixels as u32 * part as u32) / whole as u32) as u16
}
