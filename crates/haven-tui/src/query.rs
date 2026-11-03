//! Asking the host terminal what it can do, instead of guessing from `TERM`.
//!
//! `docs/PLAN.md` §13.2 asks for three things and all three are the point:
//! never trust `TERM` alone, never hang waiting for a reply that will not
//! come, and never pay for the asking twice.
//!
//! **Never hang** is the hard one. A terminal that does not implement a query
//! usually answers nothing at all, so a per-query timeout would cost a
//! timeout for every capability the host lacks — on the slowest terminals,
//! which are the ones most likely to lack them. Instead every query is
//! written in one batch ending with `DA1`, which *every* terminal since the
//! VT100 answers. When the `DA1` reply arrives, every other reply that was
//! going to come already has, and the deadline is only reached by a terminal
//! that answers nothing whatsoever.
//!
//! Replies are matched by what they are, not by the order they arrive in:
//! terminals are free to answer out of order and some do.

use std::io::Read;
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

/// How long to wait for `DA1`. Generous for a remote terminal, and only ever
/// paid in full by one that does not answer `DA1` at all.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(250);

/// What the host said about itself. `None` means it did not say.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Answers {
    pub kitty_keyboard: Option<bool>,
    pub synchronized_output: Option<bool>,
    pub underline_styles: Option<bool>,
    pub truecolor: Option<bool>,
    pub focus_events: Option<bool>,
    pub mouse_sgr: Option<bool>,
    /// The host answered `DA1`, so silence on everything else is an answer
    /// in itself rather than a timeout.
    pub answered: bool,
}

/// The batch, in the order it is written.
///
/// `DA1` is last and is the sentinel. Everything before it is a question a
/// terminal is entitled to ignore.
pub fn queries() -> Vec<u8> {
    let mut out = Vec::new();
    // Kitty keyboard flags.
    out.extend_from_slice(b"\x1b[?u");
    // DECRQM for synchronized output, focus events and SGR mouse.
    out.extend_from_slice(b"\x1b[?2026$p");
    out.extend_from_slice(b"\x1b[?1004$p");
    out.extend_from_slice(b"\x1b[?1006$p");
    // XTGETTCAP for the two terminfo capabilities that matter here, as hex:
    // `Smulx` (styled underlines) and `RGB` (direct colour).
    out.extend_from_slice(b"\x1bP+q536D756C78\x1b\\");
    out.extend_from_slice(b"\x1bP+q524742\x1b\\");
    // The sentinel. Every terminal answers this.
    out.extend_from_slice(b"\x1b[c");
    out
}

/// Read replies until `DA1` arrives or the deadline passes.
///
/// The fd must already be in raw mode, or the replies are line-buffered and
/// echoed and nothing works.
pub fn read_answers(mut input: impl Read, timeout: Duration) -> (Answers, Vec<u8>) {
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut answers = Answers::default();

    while Instant::now() < deadline {
        match input.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if has_da1(&buf) {
                    answers.answered = true;
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }

    parse_into(&buf, &mut answers);
    // Anything that was not a reply is the user typing, and must be given
    // back rather than eaten: a keystroke during startup is still a
    // keystroke.
    (answers, leftovers(&buf))
}

/// `CSI ? … c` or `CSI … c`: the DA1 reply.
fn has_da1(buf: &[u8]) -> bool {
    find_csi(buf, b'c').is_some()
}

/// Find a CSI sequence ending in `final_byte`, returning its whole span.
fn find_csi(buf: &[u8], final_byte: u8) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == 0x1b && buf[i + 1] == b'[' {
            let mut j = i + 2;
            while j < buf.len() && !(0x40..=0x7e).contains(&buf[j]) {
                j += 1;
            }
            if j < buf.len() {
                if buf[j] == final_byte {
                    return Some((i, j + 1));
                }
                i = j + 1;
                continue;
            }
            return None;
        }
        i += 1;
    }
    None
}

fn parse_into(buf: &[u8], answers: &mut Answers) {
    // Kitty keyboard: `CSI ? <flags> u`. The flags do not matter — a reply at
    // all means the protocol is understood.
    if let Some((start, end)) = find_csi(buf, b'u') {
        answers.kitty_keyboard = Some(buf[start + 2] == b'?' && end > start + 3);
    }

    // DECRPM: `CSI ? <mode> ; <value> $ y`. 1 is set, 2 is reset, and both
    // mean the mode is *known*; 0 and 4 mean it is not.
    for (mode, field) in [
        (2026u16, &mut answers.synchronized_output),
        (1004, &mut answers.focus_events),
        (1006, &mut answers.mouse_sgr),
    ] {
        if let Some(value) = decrpm(buf, mode) {
            *field = Some(matches!(value, 1 | 2));
        }
    }

    // XTGETTCAP: `DCS 1 + r <hex>=<hex> ST` for yes, `DCS 0 + r …` for no.
    if let Some(ok) = xtgettcap(buf, "536D756C78") {
        answers.underline_styles = Some(ok);
    }
    if let Some(ok) = xtgettcap(buf, "524742") {
        answers.truecolor = Some(ok);
    }
}

/// The value a `DECRPM` reply gives for one mode, if it gave one.
fn decrpm(buf: &[u8], mode: u16) -> Option<u8> {
    let text = String::from_utf8_lossy(buf);
    let needle = format!("\x1b[?{mode};");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    let end = rest.find('$')?;
    rest[..end].parse().ok()
}

/// Whether an `XTGETTCAP` reply said this capability exists.
fn xtgettcap(buf: &[u8], name_hex: &str) -> Option<bool> {
    let text = String::from_utf8_lossy(buf);
    // The name is echoed back in the reply, which is how a reply is matched
    // to its question when several are in flight.
    let at = text.to_uppercase().find(name_hex)?;
    // Walk back to the `DCS <0|1> + r` that introduced it.
    let head = &text[..at];
    let start = head.rfind("\x1bP")?;
    Some(text.as_bytes().get(start + 2) == Some(&b'1'))
}

/// Bytes that were not part of a reply: the user typing during startup.
fn leftovers(buf: &[u8]) -> Vec<u8> {
    let text = buf.to_vec();
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if text[i] == 0x1b && text.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < text.len() && !(0x40..=0x7e).contains(&text[j]) {
                j += 1;
            }
            i = (j + 1).min(text.len());
            continue;
        }
        if text[i] == 0x1b && text.get(i + 1) == Some(&b'P') {
            // DCS runs to ST (`ESC \`) or BEL.
            let mut j = i + 2;
            while j < text.len() {
                if text[j] == 0x07 {
                    j += 1;
                    break;
                }
                if text[j] == 0x1b && text.get(j + 1) == Some(&b'\\') {
                    j += 2;
                    break;
                }
                j += 1;
            }
            i = j.min(text.len());
            continue;
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

/// Ask the terminal on this descriptor, in raw mode already.
pub fn ask(
    output: BorrowedFd<'_>,
    input: impl Read,
    timeout: Duration,
) -> std::io::Result<(Answers, Vec<u8>)> {
    crate::raw::write_all(output, &queries())?;
    Ok(read_answers(input, timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers_for(reply: &[u8]) -> Answers {
        let (answers, _) = read_answers(reply, Duration::from_millis(50));
        answers
    }

    #[test]
    fn a_kitty_terminal_is_recognised() {
        let a = answers_for(b"\x1b[?1u\x1b[?62;c");
        assert_eq!(a.kitty_keyboard, Some(true));
        assert!(a.answered, "DA1 should have ended the read");
    }

    #[test]
    fn a_terminal_that_answers_only_da1_says_no_to_everything_else() {
        // The common case, and the one a per-query timeout would make slow:
        // silence after a DA1 reply is an answer, not a wait.
        let a = answers_for(b"\x1b[?62;1;6c");
        assert!(a.answered);
        assert_eq!(a.kitty_keyboard, None);
        assert_eq!(a.synchronized_output, None);
    }

    #[test]
    fn decrpm_replies_are_read_per_mode() {
        // 2026 set, 1004 reset — both mean "known" — and 1006 not recognised.
        let a = answers_for(b"\x1b[?2026;1$y\x1b[?1004;2$y\x1b[?1006;0$y\x1b[?62;c");
        assert_eq!(a.synchronized_output, Some(true));
        assert_eq!(a.focus_events, Some(true), "reset still means supported");
        assert_eq!(a.mouse_sgr, Some(false));
    }

    #[test]
    fn replies_are_matched_by_what_they_are_not_by_their_order() {
        // Terminals are free to answer out of order, and some do.
        let a = answers_for(b"\x1b[?1006;1$y\x1b[?2026;1$y\x1b[?1u\x1b[?62;c");
        assert_eq!(a.kitty_keyboard, Some(true));
        assert_eq!(a.synchronized_output, Some(true));
        assert_eq!(a.mouse_sgr, Some(true));
    }

    #[test]
    fn xtgettcap_is_matched_to_its_question_by_the_name_it_echoes() {
        // `Smulx` yes, `RGB` no, in one stream.
        let a = answers_for(b"\x1bP1+r536D756C78=1B5B3A6D\x1b\\\x1bP0+r524742\x1b\\\x1b[?62;c");
        assert_eq!(a.underline_styles, Some(true));
        assert_eq!(a.truecolor, Some(false));
    }

    #[test]
    fn a_terminal_that_says_nothing_at_all_is_not_an_answer() {
        let a = answers_for(b"");
        assert!(!a.answered, "silence must not be read as DA1");
        assert_eq!(a.kitty_keyboard, None);
    }

    #[test]
    fn typing_during_the_probe_is_handed_back_rather_than_eaten() {
        // A keystroke that arrives between the queries and the replies is
        // still a keystroke.
        let (_, rest) =
            read_answers(b"\x1b[?2026;1$yhello\x1b[?62;c".as_slice(), Duration::from_millis(50));
        assert_eq!(rest, b"hello".to_vec());
    }

    #[test]
    fn the_batch_ends_with_the_one_query_every_terminal_answers() {
        let batch = queries();
        assert!(batch.ends_with(b"\x1b[c"), "DA1 must be last, or it cannot be the sentinel");
    }
}
