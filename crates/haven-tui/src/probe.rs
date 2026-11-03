//! What can the host terminal actually do?
//!
//! `docs/PLAN.md` §13.2: never trust `TERM` alone, and never hang waiting for
//! a reply that will not come. This module holds the capability model, the
//! environment guesses it starts from, and the cache; the asking itself is
//! in [`crate::query`].

use std::path::PathBuf;

use haven_proto::{ColorDepth as ProtoDepth, KeyTier};
use haven_vtwrite::ColorDepth;

/// Bumped when the set of capabilities changes, so an old cache is ignored
/// rather than read as a newer one with everything missing.
const CACHE_VERSION: u32 = 1;

/// `$XDG_CACHE_HOME/haven/caps/<TERM>`.
///
/// Per `TERM` because that is the one thing that reliably changes when the
/// terminal does. It is a cache: deleting it costs one probe.
pub fn cache_path(term: &str) -> PathBuf {
    // A `TERM` is not guaranteed to be a safe filename.
    let safe: String = term
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("haven")
        .join("caps")
        .join(if safe.is_empty() { "unknown".to_string() } else { safe })
}

/// Reads from the real standard input, non-blocking in raw mode.
struct Stdin;

impl std::io::Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use rustix::event::{poll, PollFd, PollFlags, Timespec};
        let fd = rustix::stdio::stdin();
        // Poll rather than block: the whole point is never to hang on a
        // terminal that is not going to answer.
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let wait = Timespec { tv_sec: 0, tv_nsec: 5_000_000 };
        match poll(&mut fds, Some(&wait)) {
            Ok(0) => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            Ok(_) => rustix::io::read(fd, buf).map_err(std::io::Error::from),
            Err(rustix::io::Errno::INTR) => {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            }
            Err(e) => Err(std::io::Error::from(e)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HostCaps {
    pub depth: ColorDepth,
    pub synchronized_output: bool,
    pub underline_styles: bool,
    pub kitty_keyboard: bool,
    pub modify_other_keys: bool,
    pub clipboard_osc52: bool,
    pub hyperlinks: bool,
    pub focus_events: bool,
    pub mouse_sgr: bool,
    pub utf8: bool,
}

impl Default for HostCaps {
    /// The conservative floor: what essentially every terminal can do.
    fn default() -> Self {
        Self {
            depth: ColorDepth::Indexed256,
            synchronized_output: false,
            underline_styles: false,
            kitty_keyboard: false,
            modify_other_keys: false,
            clipboard_osc52: false,
            hyperlinks: false,
            focus_events: true,
            mouse_sgr: true,
            utf8: true,
        }
    }
}

impl HostCaps {
    /// Guess from the environment. Deliberately pessimistic: claiming a
    /// capability the host lacks produces visible garbage, while missing one
    /// merely costs polish.
    pub fn from_env() -> Self {
        let get = |key: &str| std::env::var(key).unwrap_or_default();
        let term = get("TERM");
        let program = get("TERM_PROGRAM");
        let colorterm = get("COLORTERM");

        let truecolor = colorterm.contains("truecolor") || colorterm.contains("24bit");
        let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .map(|k| get(k))
            .find(|v| !v.is_empty())
            .map(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("utf-8") || v.contains("utf8")
            })
            .unwrap_or(false);

        // Terminals known to implement the richer protocols. A list like this
        // is a starting point, not the answer: M6 replaces it with a real
        // probe and keeps this as the fallback.
        let modern = matches!(program.as_str(), "ghostty" | "WezTerm" | "kitty")
            || term.contains("kitty")
            || term.contains("ghostty")
            || term.contains("wezterm")
            || term.contains("foot");

        Self {
            depth: if truecolor { ColorDepth::TrueColor } else { ColorDepth::Indexed256 },
            synchronized_output: modern,
            underline_styles: modern || term.contains("256color"),
            kitty_keyboard: modern,
            modify_other_keys: term.starts_with("xterm"),
            clipboard_osc52: modern || term.starts_with("xterm"),
            hyperlinks: modern,
            focus_events: true,
            mouse_sgr: true,
            utf8,
        }
    }

    /// Overlay what the host actually said on what the environment implied.
    ///
    /// An explicit answer always wins. Silence is the interesting case, and
    /// it is not treated the same way for everything.
    ///
    /// For the keyboard protocol, silence from a host that answered `DA1` is
    /// a no: it proved it was listening, and claiming a protocol it does not
    /// speak means keys that do nothing. Every terminal that implements it
    /// answers `CSI ? u`, so there are no false negatives to worry about.
    ///
    /// For everything else, silence keeps the guess. Plenty of terminals
    /// answer `DA1` and implement no other query at all — `XTGETTCAP` is far
    /// from universal — and reading that as "no styled underlines" would
    /// quietly take a working feature away from, say, VTE. The cost of being
    /// wrong the other way is a mode string those terminals ignore.
    pub fn with_answers(mut self, answers: crate::query::Answers) -> Self {
        let kept = |guess: bool, said: Option<bool>| said.unwrap_or(guess);

        // Only ever added: `COLORTERM` is usually right and a terminal that
        // does not answer `XTGETTCAP` for `RGB` is usually still truecolor.
        if let Some(true) = answers.truecolor {
            self.depth = ColorDepth::TrueColor;
        }
        self.kitty_keyboard = match answers.kitty_keyboard {
            Some(value) => value,
            None if answers.answered => false,
            None => self.kitty_keyboard,
        };
        self.synchronized_output = kept(self.synchronized_output, answers.synchronized_output);
        self.underline_styles = kept(self.underline_styles, answers.underline_styles);
        self.focus_events = kept(self.focus_events, answers.focus_events);
        self.mouse_sgr = kept(self.mouse_sgr, answers.mouse_sgr);
        // Nothing queryable says whether OSC 52 or hyperlinks work, so those
        // keep the environment's guess whatever the host answered.
        self
    }

    /// Everything the cache file holds, as `key=value` lines.
    ///
    /// Hand-rolled rather than serde: six booleans and a depth do not justify
    /// a dependency in the one crate that has none, and a format this small
    /// is easier to delete by hand when it is wrong.
    pub fn to_cache(&self) -> String {
        let depth = match self.depth {
            ColorDepth::Ansi16 => "ansi16",
            ColorDepth::Indexed256 => "indexed256",
            ColorDepth::TrueColor => "truecolor",
        };
        format!(
            "version={CACHE_VERSION}\ndepth={depth}\nsynchronized_output={}\n\
             underline_styles={}\nkitty_keyboard={}\nmodify_other_keys={}\n\
             clipboard_osc52={}\nhyperlinks={}\nfocus_events={}\nmouse_sgr={}\nutf8={}\n",
            self.synchronized_output,
            self.underline_styles,
            self.kitty_keyboard,
            self.modify_other_keys,
            self.clipboard_osc52,
            self.hyperlinks,
            self.focus_events,
            self.mouse_sgr,
            self.utf8,
        )
    }

    /// Read a cache file, or `None` if it is from another version or is not
    /// a cache file at all. A bad cache re-probes rather than failing.
    pub fn from_cache(text: &str) -> Option<Self> {
        let mut caps = Self::default();
        let mut version = None;
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            let flag = value == "true";
            match key {
                "version" => version = value.parse::<u32>().ok(),
                "depth" => {
                    caps.depth = match value {
                        "ansi16" => ColorDepth::Ansi16,
                        "truecolor" => ColorDepth::TrueColor,
                        _ => ColorDepth::Indexed256,
                    }
                }
                "synchronized_output" => caps.synchronized_output = flag,
                "underline_styles" => caps.underline_styles = flag,
                "kitty_keyboard" => caps.kitty_keyboard = flag,
                "modify_other_keys" => caps.modify_other_keys = flag,
                "clipboard_osc52" => caps.clipboard_osc52 = flag,
                "hyperlinks" => caps.hyperlinks = flag,
                "focus_events" => caps.focus_events = flag,
                "mouse_sgr" => caps.mouse_sgr = flag,
                "utf8" => caps.utf8 = flag,
                _ => return None,
            }
        }
        (version == Some(CACHE_VERSION)).then_some(caps)
    }

    /// What this terminal can do: the cache, or a probe, or the guess.
    ///
    /// The probe needs raw mode, which `haven dump` and the tests do not
    /// have and a pipe cannot give — so a descriptor that is not a terminal
    /// skips straight to the guess rather than writing escape sequences into
    /// whatever is reading.
    ///
    /// Returns anything the user typed during the probe, which must be fed
    /// back into the decoder: a keystroke at startup is still a keystroke.
    pub fn detect(input: rustix::fd::BorrowedFd<'_>) -> (Self, Vec<u8>) {
        let term = std::env::var("TERM").unwrap_or_default();
        let path = cache_path(&term);
        if let Some(caps) = std::fs::read_to_string(&path).ok().and_then(|t| Self::from_cache(&t)) {
            return (caps, Vec::new());
        }
        if !rustix::termios::isatty(input) {
            return (Self::from_env(), Vec::new());
        }
        let Ok((answers, typed)) = crate::query::ask(input, Stdin, crate::query::DEFAULT_TIMEOUT)
        else {
            return (Self::from_env(), Vec::new());
        };
        let caps = Self::from_env().with_answers(answers);
        // Only a host that answered is worth remembering. Caching the guess
        // would pin a wrong answer in place for as long as the file lives.
        if answers.answered {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, caps.to_cache());
        }
        (caps, typed)
    }

    pub fn key_tier(&self) -> KeyTier {
        crate::tier_for(self.kitty_keyboard, self.modify_other_keys)
    }

    pub fn proto_depth(&self) -> ProtoDepth {
        match self.depth {
            ColorDepth::Ansi16 => ProtoDepth::Ansi16,
            ColorDepth::Indexed256 => ProtoDepth::Indexed256,
            ColorDepth::TrueColor => ProtoDepth::TrueColor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_floor_claims_nothing_fancy() {
        let caps = HostCaps::default();
        assert_eq!(caps.key_tier(), KeyTier::Legacy);
        assert!(!caps.synchronized_output);
        assert!(!caps.kitty_keyboard);
    }

    #[test]
    fn the_cache_round_trips_and_a_stale_one_is_ignored() {
        let caps = HostCaps { kitty_keyboard: true, ..HostCaps::from_env() };
        let text = caps.to_cache();
        assert_eq!(HostCaps::from_cache(&text), Some(caps));

        // A file from another version must re-probe, not be read as this
        // version with everything it did not mention left at the floor.
        let stale = text.replace("version=1", "version=999");
        assert_eq!(HostCaps::from_cache(&stale), None);
        assert_eq!(HostCaps::from_cache("not a cache file at all"), None);
    }

    #[test]
    fn an_answer_overrides_the_guess_and_silence_does_not() {
        let guess =
            HostCaps { kitty_keyboard: true, synchronized_output: true, ..Default::default() };
        // A host that said nothing at all leaves the guess alone: ignoring a
        // query is not the same as answering "no".
        let untouched = guess.with_answers(crate::query::Answers::default());
        assert!(untouched.kitty_keyboard && untouched.synchronized_output);

        // A host that answered DA1 proved it was listening, so silence on a
        // feature query really is a no.
        let listening = crate::query::Answers { answered: true, ..Default::default() };
        let settled = guess.with_answers(listening);
        assert!(!settled.kitty_keyboard, "silence from a listening host means no");

        // And an explicit answer beats both.
        let said = crate::query::Answers {
            answered: true,
            kitty_keyboard: Some(true),
            ..Default::default()
        };
        assert!(HostCaps::default().with_answers(said).kitty_keyboard);
    }

    #[test]
    fn truecolor_is_only_ever_added_by_an_answer_never_taken_away() {
        // COLORTERM is usually right and XTGETTCAP for RGB is usually
        // missing, so a "no" here would downgrade terminals that are fine.
        let caps = HostCaps { depth: ColorDepth::TrueColor, ..Default::default() };
        let denied =
            crate::query::Answers { answered: true, truecolor: Some(false), ..Default::default() };
        assert_eq!(caps.with_answers(denied).depth, ColorDepth::TrueColor);

        let granted =
            crate::query::Answers { answered: true, truecolor: Some(true), ..Default::default() };
        assert_eq!(HostCaps::default().with_answers(granted).depth, ColorDepth::TrueColor);
    }

    #[test]
    fn a_term_that_is_not_a_filename_still_gets_a_cache_path() {
        // The property that matters is that the name stays one component:
        // dots are harmless, separators are not.
        let path = cache_path("screen.xterm-256color/../../etc");
        assert_eq!(path.parent(), cache_path("xterm").parent(), "{}", path.display());
        assert_eq!(path.components().count(), cache_path("xterm").components().count());
        assert!(cache_path("").to_string_lossy().ends_with("unknown"));
    }

    #[test]
    fn tiers_follow_the_protocols_the_host_speaks() {
        assert_eq!(crate::tier_for(true, true), KeyTier::Full);
        assert_eq!(crate::tier_for(true, false), KeyTier::Full);
        assert_eq!(crate::tier_for(false, true), KeyTier::Extended);
        assert_eq!(crate::tier_for(false, false), KeyTier::Legacy);
    }
}
