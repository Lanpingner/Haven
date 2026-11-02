//! `haven attach`: the TUI client against a daemon.
//!
//! The same `Client` loop the tests drive, with a real terminal on one side
//! and a socket on the other (`docs/PLAN.md` §3).

use std::io;
use std::os::fd::AsFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use haven_client::input::KeyEvent;
use haven_client::{Client, Intercepted, Tick};
use haven_proto::transport::SocketTransport;
use haven_proto::{ClientMsg, SessionId, Transport};
use haven_tui::{HostCaps, TuiFrontend};
use rustix::event::{poll, PollFd, PollFlags, Timespec};

use crate::keys::{self, Action};

/// Connect, starting a daemon if there is not one already.
///
/// Autostart rather than an error: a terminal that refuses to open because no
/// server is running would be a strange thing to ship.
pub fn connect_or_start(socket: &Path) -> io::Result<SocketTransport> {
    if let Ok(transport) = SocketTransport::connect(socket) {
        return Ok(transport);
    }
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .arg("server")
        .arg("run")
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    // Wait for the socket to answer rather than sleeping a fixed amount.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(transport) = SocketTransport::connect(socket) {
            return Ok(transport);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "daemon did not start"))
}

/// The prefix key table, as a filter the client consults per keystroke.
fn key_filter() -> impl FnMut(&KeyEvent) -> Intercepted {
    let mut armed = false;
    move |event: &KeyEvent| {
        if armed {
            armed = false;
            return match keys::prefix_table(event) {
                Some(Action::Detach) => Intercepted::Detach,
                Some(Action::SendPrefix) => Intercepted::Pass,
                Some(action) => match keys::to_proto(action) {
                    Some(wire) => Intercepted::Act(wire),
                    None => Intercepted::Consumed,
                },
                // An unbound key after the prefix does nothing rather than
                // leaking into the pane.
                None => Intercepted::Consumed,
            };
        }
        if keys::is_prefix(event) {
            armed = true;
            return Intercepted::Consumed;
        }
        Intercepted::Pass
    }
}

/// Attach with the GPU frontend: a window instead of the host terminal.
///
/// Same client loop, same key table, same daemon — only the `Frontend` impl
/// differs, which is the whole point of the seam (`docs/PLAN.md` §3).
pub fn run_gpu(
    socket: &Path,
    session: Option<SessionId>,
    config: haven_render::WindowConfig,
) -> io::Result<()> {
    let transport = connect_or_start(socket)?;
    // The event loop blocks until something happens; without a descriptor to
    // watch, it would never notice the daemon. Duplicating it lets the
    // watcher thread wait on the same socket without owning the transport.
    let wake = rustix::io::dup(transport.as_fd()).ok();
    haven_render::window::run(transport, session, config, wake)
        .map_err(|e| io::Error::other(e.to_string()))
}

pub fn run(socket: &Path, session: Option<SessionId>, host: HostCaps) -> io::Result<()> {
    let transport = connect_or_start(socket)?;
    let frontend = TuiFrontend::enter(host)?;
    let mut client = Client::new(frontend, transport);
    client.key_filter = Some(Box::new(key_filter()));
    client.attach(session).map_err(to_io)?;

    let stdin = rustix::stdio::stdin();
    loop {
        // Block on either the terminal or the daemon, so an idle client uses
        // no CPU and wakes with no timer.
        {
            let socket_fd = client.transport.as_fd();
            let input = stdin.as_fd();
            let mut fds =
                [PollFd::new(&socket_fd, PollFlags::IN), PollFd::new(&input, PollFlags::IN)];
            let ts = Timespec { tv_sec: 0, tv_nsec: 50_000_000 };
            match poll(&mut fds, Some(&ts)) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
        match client.tick().map_err(to_io)? {
            Tick::Finished => break,
            Tick::Idle | Tick::Redraw => {}
        }
    }
    Ok(())
}

/// Ask the daemon for its session list.
pub fn list(socket: &Path) -> io::Result<Vec<haven_proto::SessionInfo>> {
    let mut transport = match SocketTransport::connect(socket) {
        Ok(t) => t,
        // No socket means no sessions, which is an answer rather than a failure.
        Err(e)
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            return Ok(Vec::new())
        }
        Err(e) => return Err(e),
    };
    transport.send(ClientMsg::ListSessions).map_err(to_io)?;
    wait_for_sessions(&mut transport)
}

pub fn kill_session(socket: &Path, id: SessionId) -> io::Result<Vec<haven_proto::SessionInfo>> {
    let mut transport = SocketTransport::connect(socket)?;
    transport.send(ClientMsg::KillSession(id)).map_err(to_io)?;
    wait_for_sessions(&mut transport)
}

pub fn shutdown(socket: &Path) -> io::Result<()> {
    let mut transport = match SocketTransport::connect(socket) {
        Ok(t) => t,
        Err(_) => return Ok(()), // Already stopped.
    };
    transport.send(ClientMsg::Shutdown).map_err(to_io)?;
    // Give the daemon a moment to act before we drop the socket.
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}

fn wait_for_sessions(transport: &mut SocketTransport) -> io::Result<Vec<haven_proto::SessionInfo>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match transport.try_recv() {
            Ok(Some(haven_proto::ServerMsg::Sessions(sessions))) => return Ok(sessions),
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(to_io(e)),
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "no reply from the daemon"))
}

fn to_io(e: haven_proto::TransportError) -> io::Error {
    io::Error::other(e.to_string())
}
