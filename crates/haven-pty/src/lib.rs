//! PTY allocation, child launch, job control and window sizing.
//! See `docs/PLAN.md` §9.1.
//!
//! Everything interactive in a shell — `Ctrl+C`, `Ctrl+Z`, `fg`, `^C` inside
//! `less` — depends on this layer being right, so it is deliberately the first
//! thing built. Signals must reach the *foreground process group*, not just our
//! direct child, which requires the child to be a session leader owning the pty
//! as its controlling terminal.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
use rustix::termios::{tcsetwinsize, Winsize};

/// Cell *and* pixel dimensions. The pixel fields are not decoration: image
/// protocols and several TUIs read them out of `TIOCGWINSZ`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self { cols: 80, rows: 24, pixel_width: 0, pixel_height: 0 }
    }
}

impl From<PtySize> for Winsize {
    fn from(s: PtySize) -> Self {
        Winsize {
            ws_row: s.rows,
            ws_col: s.cols,
            ws_xpixel: s.pixel_width,
            ws_ypixel: s.pixel_height,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PtyConfig {
    /// Program to run. `None` resolves a shell (see `haven-shell`).
    pub program: Option<PathBuf>,
    pub args: Vec<OsString>,
    /// Pass `argv[0]` with a leading `-`, which is what fish and bash actually
    /// key off for login behaviour (`-l` is not equivalent).
    pub login: bool,
    pub cwd: Option<PathBuf>,
    /// Applied on top of the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// Removed from the child environment. Stale `LINES`/`COLUMNS` break
    /// resize for curses apps, so they are always scrubbed.
    pub env_remove: Vec<OsString>,
    pub size: PtySize,
}

#[derive(Debug)]
pub struct Pty {
    master: OwnedFd,
    child: Child,
    size: PtySize,
}

impl Pty {
    pub fn spawn(cfg: &PtyConfig) -> io::Result<Self> {
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        grantpt(&master)?;
        unlockpt(&master)?;
        let slave_name = ptsname(&master, Vec::new())?;
        tcsetwinsize(&master, cfg.size.into())?;

        let program = cfg.program.clone().unwrap_or_else(|| PathBuf::from("/bin/sh"));
        let mut cmd = Command::new(&program);
        cmd.args(&cfg.args);

        if cfg.login {
            let name = program.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            let mut arg0 = OsString::from("-");
            arg0.push(name);
            cmd.arg0(arg0);
        }
        if let Some(cwd) = &cfg.cwd {
            cmd.current_dir(cwd);
        }
        for key in &cfg.env_remove {
            cmd.env_remove(key);
        }
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }

        // The child's stdio is the pty slave. We open it in the parent and let
        // `pre_exec` dup it into place, so a failure to open is a normal error
        // rather than something we have to report from inside a forked child.
        let slave = rustix::fs::open(
            slave_name.as_c_str(),
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY,
            rustix::fs::Mode::empty(),
        )?;
        let slave_raw = slave.as_raw_fd();
        let master_raw = master.as_raw_fd();

        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());

        // SAFETY: `pre_exec` runs between fork and exec in the child, so only
        // async-signal-safe calls are permitted here: no allocation, no locks,
        // no Rust machinery. These are all raw syscalls.
        unsafe {
            cmd.pre_exec(move || {
                // New session, detached from our controlling terminal.
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                // Claim the pty slave as the controlling terminal. Without this
                // there is no foreground process group and job control, ^C and
                // ^Z are all silently broken.
                if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                for target in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
                    if libc::dup2(slave_raw, target) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                if slave_raw > libc::STDERR_FILENO {
                    libc::close(slave_raw);
                }
                libc::close(master_raw);

                // Rust sets SIGPIPE to SIG_IGN and may leave other handlers in
                // place; shells and everything they spawn expect defaults.
                for sig in [
                    libc::SIGPIPE,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGCHLD,
                    libc::SIGTSTP,
                    libc::SIGTTIN,
                    libc::SIGTTOU,
                ] {
                    libc::signal(sig, libc::SIG_DFL);
                }
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
                Ok(())
            });
        }

        let child = cmd.spawn()?;
        drop(slave); // The child owns its copy; holding ours would defeat EOF.
        Ok(Self { master, child, size: cfg.size })
    }

    pub fn master(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    pub fn size(&self) -> PtySize {
        self.size
    }

    /// `TIOCSWINSZ` on the master; the kernel sends `SIGWINCH` to the
    /// foreground process group for us.
    pub fn resize(&mut self, size: PtySize) -> io::Result<()> {
        tcsetwinsize(&self.master, size.into())?;
        self.size = size;
        Ok(())
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The foreground process group's pid, via `tcgetpgrp`. This is what a tab
    /// title should show and what "something is still running" must check —
    /// the direct child is usually just the shell.
    pub fn foreground_pid(&self) -> Option<i32> {
        // SAFETY: a plain query on a valid fd; no memory is shared.
        let pgrp = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        (pgrp > 0).then_some(pgrp)
    }

    /// Signal the whole foreground process group, falling back to the child.
    /// Signalling only the direct child is the classic bug that leaves
    /// `make -j` children running after a pane closes.
    pub fn signal_foreground(&self, sig: i32) -> io::Result<()> {
        let target = self.foreground_pid().unwrap_or(self.child.id() as i32);
        // SAFETY: `killpg` with a pgid we just read from the kernel.
        if unsafe { libc::killpg(target, sig) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// `SIGHUP` the group, then `SIGKILL` after a grace period.
    pub fn hangup(&mut self, grace: std::time::Duration) -> io::Result<ExitStatus> {
        let _ = self.signal_foreground(libc::SIGHUP);
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = self.child.kill();
        self.child.wait()
    }
}

impl Drop for Pty {
    /// Hang up the child when the pty goes away.
    ///
    /// Dropping the master means nothing can talk to the child any more, so
    /// leaving it running would orphan it — and `SIGHUP` is exactly the signal
    /// a process expects when its terminal disappears. Best-effort and
    /// non-blocking: `Drop` is not the place to wait on anything.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.signal_foreground(libc::SIGHUP);
        let _ = self.child.kill();
        // Reap, or we leave a zombie: `SIGKILL` is prompt, so a short bounded
        // wait is enough and `Drop` still never blocks for long.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
        while std::time::Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(2)),
            }
        }
    }
}

impl Read for Pty {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match rustix::io::read(&self.master, buf) {
            Ok(n) => Ok(n),
            // The slave closing surfaces as EIO on Linux, which means EOF here.
            Err(e) if e == rustix::io::Errno::IO => Ok(0),
            Err(e) => Err(e.into()),
        }
    }
}

impl Write for Pty {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(rustix::io::write(&self.master, buf)?)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> PtyConfig {
        PtyConfig {
            program: Some("/bin/sh".into()),
            args: vec!["-c".into(), script.into()],
            size: PtySize { cols: 40, rows: 10, pixel_width: 320, pixel_height: 160 },
            ..Default::default()
        }
    }

    fn read_to_end(pty: &mut Pty) -> String {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn child_output_reaches_the_master() {
        let mut pty = Pty::spawn(&sh("printf 'hello pty'")).unwrap();
        assert!(read_to_end(&mut pty).contains("hello pty"));
        assert!(pty.wait().unwrap().success());
    }

    #[test]
    fn child_sees_our_window_size_in_cells_and_pixels() {
        // `stty size` proves the child's controlling terminal is the pty and
        // that TIOCSWINSZ landed before exec.
        let mut pty = Pty::spawn(&sh("stty size")).unwrap();
        assert!(read_to_end(&mut pty).contains("10 40"));
    }

    #[test]
    fn child_is_a_session_leader_with_a_foreground_group() {
        let mut pty = Pty::spawn(&sh("sleep 5")).unwrap();
        // Give the child a moment to become the session leader.
        std::thread::sleep(std::time::Duration::from_millis(120));
        let fg = pty.foreground_pid().expect("pty has a foreground process group");
        assert_eq!(fg as u32, pty.pid(), "the child should lead its own group");
        pty.signal_foreground(libc::SIGTERM).unwrap();
        let status = pty.wait().unwrap();
        assert!(!status.success());
    }

    #[test]
    fn env_is_set_and_scrubbed() {
        let mut cfg = sh("printf '%s|%s' \"$TERM\" \"${COLUMNS:-unset}\"");
        cfg.env.push(("TERM".into(), "xterm-256color".into()));
        cfg.env_remove.push("COLUMNS".into());
        let mut pty = Pty::spawn(&cfg).unwrap();
        let out = read_to_end(&mut pty);
        assert!(out.contains("xterm-256color|unset"), "{out:?}");
    }

    #[test]
    fn dropping_the_pty_hangs_up_the_child() {
        // Otherwise a closed pane leaves its shell running forever, which is
        // how a terminal ends up with orphaned processes.
        let pty = Pty::spawn(&sh("sleep 30")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let pid = pty.pid() as i32;
        drop(pty);

        // The child is gone: signal 0 only checks existence.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut alive = true;
        while std::time::Instant::now() < deadline && alive {
            // SAFETY: signal 0 is a pure existence check.
            alive = unsafe { libc::kill(pid, 0) } == 0;
            if alive {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        assert!(!alive, "child survived the pty being dropped");
    }

    #[test]
    fn hangup_terminates_a_stubborn_child() {
        let mut pty = Pty::spawn(&sh("trap '' HUP; sleep 30")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let status = pty.hangup(std::time::Duration::from_millis(200)).unwrap();
        assert!(!status.success());
    }
}
