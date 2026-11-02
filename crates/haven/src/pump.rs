//! The read/parse/reply loop: pty bytes in, grid state out.
//!
//! Replies matter as much as parsing. A shell that queries `DA1` or CPR and
//! gets silence does not degrade gracefully — it waits for its timeout, which
//! the user experiences as a hang (`docs/PLAN.md` §9.4).

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use haven_core::Emulator;
use haven_pty::Pty;

#[derive(Clone, Copy, Debug)]
pub struct PumpLimits {
    /// Stop once the child has been quiet for this long.
    pub idle: Duration,
    /// Hard ceiling, so a chatty child cannot hang a test forever.
    pub total: Duration,
}

impl Default for PumpLimits {
    fn default() -> Self {
        Self { idle: Duration::from_millis(250), total: Duration::from_secs(10) }
    }
}

#[derive(Debug, Default)]
pub struct PumpStats {
    pub bytes_read: usize,
    pub reply_bytes: usize,
    pub reads: usize,
    /// True when the child closed the pty (EOF), as opposed to going idle.
    pub eof: bool,
}

/// Drive `emu` from `pty` until EOF, idle timeout or the total ceiling.
///
/// `raw` accumulates everything the child wrote, which the compatibility tests
/// use to assert on sequences the grid deliberately does not retain (OSC 133
/// marks, for instance, until the semantic-prompt model lands in M3.7).
pub fn pump(
    pty: &mut Pty,
    emu: &mut Emulator,
    limits: PumpLimits,
    raw: &mut Vec<u8>,
) -> io::Result<PumpStats> {
    let mut stats = PumpStats::default();
    let start = Instant::now();
    let mut last_data = Instant::now();
    let mut buf = [0u8; 65536];

    loop {
        if start.elapsed() >= limits.total || last_data.elapsed() >= limits.idle {
            break;
        }
        let remaining = limits.idle.saturating_sub(last_data.elapsed());
        if !wait_readable(pty, remaining)? {
            continue;
        }
        match pty.read(&mut buf) {
            Ok(0) => {
                stats.eof = true;
                break;
            }
            Ok(n) => {
                stats.reads += 1;
                stats.bytes_read += n;
                last_data = Instant::now();
                raw.extend_from_slice(&buf[..n]);
                emu.feed(&buf[..n]);

                let reply = emu.term.take_reply();
                if !reply.is_empty() {
                    stats.reply_bytes += reply.len();
                    pty.write_all(&reply)?;
                    pty.flush()?;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(stats)
}

/// `poll` the master for readability. Returns false on timeout.
fn wait_readable(pty: &Pty, timeout: Duration) -> io::Result<bool> {
    let mut pfd = libc::pollfd { fd: pty.master().as_raw_fd(), events: libc::POLLIN, revents: 0 };
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: a single valid pollfd; poll writes only into `revents`.
    let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
    match rc {
        -1 => {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(err)
            }
        }
        0 => Ok(false),
        _ => Ok(true),
    }
}

/// Convenience for tests and the `dump` subcommand: run a program on a pty of
/// the given size, optionally typing `input` at it, and return the final screen.
///
/// `input` is the seed of the scripted-keys mechanism the app-compatibility
/// harness needs (`docs/PLAN.md` §16, M2.7).
pub fn run_to_screen(
    cfg: &haven_pty::PtyConfig,
    limits: PumpLimits,
    input: &[u8],
) -> io::Result<(Emulator, Vec<u8>, PumpStats)> {
    let mut pty = Pty::spawn(cfg)?;
    let mut emu = Emulator::new(cfg.size.cols as usize, cfg.size.rows as usize);

    if !input.is_empty() {
        // Writing immediately is safe: the pty buffers until the child reads.
        // Interactive shells still need a beat to install their line editor,
        // or the first keystrokes land before the reader exists.
        std::thread::sleep(Duration::from_millis(150));
        pty.write_all(input)?;
        pty.flush()?;
    }

    let mut raw = Vec::new();
    let stats = pump(&mut pty, &mut emu, limits, &mut raw)?;
    Ok((emu, raw, stats))
}
