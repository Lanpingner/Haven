//! What can the host terminal actually do?
//!
//! `docs/PLAN.md` §13.2: never trust `TERM` alone, and never hang waiting for
//! a reply that will not come. This module holds the capability model and the
//! environment-based guesses; the interactive probe (with timeouts, and
//! caching per `TERM`) lands with M6.

use haven_proto::{ColorDepth as ProtoDepth, KeyTier};
use haven_vtwrite::ColorDepth;

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
    fn tiers_follow_the_protocols_the_host_speaks() {
        assert_eq!(crate::tier_for(true, true), KeyTier::Full);
        assert_eq!(crate::tier_for(true, false), KeyTier::Full);
        assert_eq!(crate::tier_for(false, true), KeyTier::Extended);
        assert_eq!(crate::tier_for(false, false), KeyTier::Legacy);
    }
}
