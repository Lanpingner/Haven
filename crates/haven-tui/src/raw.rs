//! Raw mode, and getting out of it no matter how we die.
//!
//! `docs/PLAN.md` §11.1: leaving a user's terminal in a weird mode after a
//! crash is unforgivable, so the restore path runs from `Drop` *and* from a
//! signal handler, and the modes we pushed are popped in reverse.

use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use rustix::termios::{self, OptionalActions, Termios};

/// The modes a session turns on, so they can be turned off again exactly.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pushed {
    pub alt_screen: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub mouse: bool,
    pub synchronized_output: bool,
    pub kitty_keyboard: bool,
}

impl Pushed {
    /// The sequence that enables these modes.
    pub fn enter(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.alt_screen {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        if self.bracketed_paste {
            out.extend_from_slice(b"\x1b[?2004h");
        }
        if self.focus_events {
            out.extend_from_slice(b"\x1b[?1004h");
        }
        if self.mouse {
            out.extend_from_slice(b"\x1b[?1002h\x1b[?1006h");
        }
        if self.kitty_keyboard {
            // Progressive enhancement, pushed so it can be popped.
            out.extend_from_slice(b"\x1b[>1u");
        }
        out
    }

    /// The sequence that undoes [`Pushed::enter`], in reverse order.
    pub fn leave(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.kitty_keyboard {
            out.extend_from_slice(b"\x1b[<u");
        }
        if self.mouse {
            out.extend_from_slice(b"\x1b[?1006l\x1b[?1002l");
        }
        if self.focus_events {
            out.extend_from_slice(b"\x1b[?1004l");
        }
        if self.bracketed_paste {
            out.extend_from_slice(b"\x1b[?2004l");
        }
        // Show the cursor whatever happened, then leave the alt screen.
        out.extend_from_slice(b"\x1b[?25h");
        if self.alt_screen {
            out.extend_from_slice(b"\x1b[?1049l");
        }
        out
    }
}

static ORIGINAL: OnceLock<Termios> = OnceLock::new();
static PUSHED: OnceLock<Pushed> = OnceLock::new();
static RESTORED: AtomicBool = AtomicBool::new(false);

/// Put the terminal in raw mode and remember how to put it back.
#[derive(Debug)]
pub struct RawMode {
    pushed: Pushed,
}

impl RawMode {
    pub fn enter(pushed: Pushed) -> io::Result<Self> {
        let stdin = rustix::stdio::stdin();
        let original = termios::tcgetattr(stdin)?;
        let _ = ORIGINAL.set(original.clone());
        let _ = PUSHED.set(pushed);
        RESTORED.store(false, Ordering::SeqCst);

        let mut raw = original;
        raw.make_raw();
        termios::tcsetattr(stdin, OptionalActions::Flush, &raw)?;

        write_all(rustix::stdio::stdout(), &pushed.enter())?;
        install_signal_handlers();
        Ok(Self { pushed })
    }

    pub fn pushed(&self) -> Pushed {
        self.pushed
    }

    /// Idempotent: `Drop` and the signal handler may both run.
    pub fn restore(&self) {
        restore_now();
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        self.restore();
    }
}

fn restore_now() {
    if RESTORED.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(pushed) = PUSHED.get() {
        let _ = write_all(rustix::stdio::stdout(), &pushed.leave());
    }
    if let Some(original) = ORIGINAL.get() {
        let _ = termios::tcsetattr(rustix::stdio::stdin(), OptionalActions::Now, original);
    }
}

/// Restore on the signals that would otherwise kill us mid-frame, then die the
/// way we were asked to.
///
/// `tcsetattr` and `write` are syscalls, which is the practical bar for a
/// handler like this; the alternative — dying in raw mode on the alternate
/// screen — leaves the user with an unusable terminal.
extern "C" fn handle_fatal_signal(sig: i32) {
    restore_now();
    // SAFETY: restoring the default disposition and re-raising is the standard
    // way to preserve the exit status the signal implies.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

fn install_signal_handlers() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT, libc::SIGSEGV] {
            // SAFETY: installing a handler that only makes syscalls.
            unsafe {
                libc::signal(sig, handle_fatal_signal as *const () as libc::sighandler_t);
            }
        }
    });
}

pub fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match rustix::io::write(fd, bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Current terminal size, in cells and pixels.
pub fn window_size<F: AsFd>(fd: F) -> io::Result<(u16, u16, u16, u16)> {
    let ws = termios::tcgetwinsize(fd)?;
    Ok((ws.ws_col.max(1), ws.ws_row.max(1), ws.ws_xpixel, ws.ws_ypixel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_are_popped_in_reverse_order() {
        let pushed = Pushed {
            alt_screen: true,
            bracketed_paste: true,
            focus_events: true,
            mouse: true,
            synchronized_output: true,
            kitty_keyboard: true,
        };
        let enter = String::from_utf8(pushed.enter()).unwrap();
        let leave = String::from_utf8(pushed.leave()).unwrap();

        // Entering turns the alt screen on first; leaving turns it off last.
        assert!(enter.starts_with("\x1b[?1049h"));
        assert!(leave.ends_with("\x1b[?1049l"));
        // Kitty flags are pushed and popped, never left behind.
        assert!(enter.contains("\x1b[>1u"));
        assert!(leave.contains("\x1b[<u"));
        // The cursor is always made visible again.
        assert!(leave.contains("\x1b[?25h"));
    }

    #[test]
    fn nothing_pushed_means_nothing_to_undo_beyond_the_cursor() {
        let pushed = Pushed::default();
        assert!(pushed.enter().is_empty());
        assert_eq!(pushed.leave(), b"\x1b[?25h");
    }
}
