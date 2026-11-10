//! `12 errors, 3 warnings`, from the wall of output a build just left.
//!
//! A failed compile prints several hundred rows and the one thing anyone
//! wants from them first is a number. Is this two mistakes or forty — do
//! I read the top of the scrollback or start again? Scrolling to the
//! summary line answers it, except that the summary line is at the
//! bottom of output that is still arriving, and for `gcc` and for most
//! test runners there is no summary line at all. So the plugin counts,
//! and the number sits on the status bar until the next build.
//!
//! # What it reads, and why it cannot do without it
//!
//! It reads the text of the focused pane: the last 400 rows, once, when
//! a command finishes. Diagnostics are the pane's *contents* — the
//! compiler wrote them to the terminal and nowhere else that a plugin
//! can see. `panes()` offers the pane's title, its working directory and
//! the command line, and none of those says how many errors there were.
//! There is no narrower permission that would do, so this plugin asks
//! for `read-pane-content`, which is the whole of the pane's output: not
//! only the compiler's lines but everything else that scrolled past,
//! including anything secret that was printed there.
//!
//! What it does with that is count. Nothing leaves the plugin — there is
//! no `net` capability and the host refuses one — and **nothing is
//! stored**. The counts are kept in a `static` for as long as the plugin
//! is loaded; they are deliberately not kept with `Command::Remember`,
//! which writes to disk, because an error message quotes the source line
//! it is about and a remembered diagnostic is a piece of somebody's code
//! and screen copied into a file that outlives the session. Two integers
//! are not worth that, and nothing read here is worth it either.
//!
//! # Why the counting is on an event and not in `render`
//!
//! `render` runs every time Haven draws. Counting four hundred rows of
//! diagnostics per frame would be absurd — the answer changes when a
//! build finishes, which is once a minute at best, and the host would be
//! copying the pane's text across the component boundary on every
//! frame. So the count happens on `command-ended`, exactly when the
//! output has stopped arriving, and `render` formats two numbers that
//! are already known. The cache is cleared on `command-began`, so a
//! stale count does not sit beside a build that is still running, and on
//! a focus change away from the pane it was counted from, because a
//! count from a pane you are not looking at is misinformation.
//!
//! # The counting rules, and the trap
//!
//! Three shapes are recognised:
//!
//! * rustc and cargo: a line beginning `error:`, `error[E0308]:`,
//!   `warning:` or `warning[…]:`.
//! * gcc and clang: `file.c:12:5: error:`, `file.c:12: warning:` and
//!   `fatal error:`, recognised by the `path:line[:col]` before the
//!   colon — the digits are what distinguishes a diagnostic from a
//!   sentence containing the word "error:".
//! * a test runner's verdict: `FAILED: target` from ninja, `FAILED
//!   tests/x.py::y` from pytest, `--- FAIL: TestThing` from go, and
//!   `test thing ... FAILED` from Rust's harness, each counted as an
//!   error.
//!
//! The trap is the summary line, and getting it wrong is the difference
//! between a useful number and a number that is always one or two too
//! high. `error: could not compile \`haven\` (lib) due to 2 previous
//! errors` is not a thirteenth error, `warning: \`haven\` (lib)
//! generated 3 warnings` is not a fourth warning, `warning: 2 warnings
//! emitted` is not two more, `test result: FAILED. 2 passed; 1 failed`
//! is not another failure, and a bare `FAIL` on its own line is go's
//! verdict on everything above it. All of them begin exactly like the
//! real thing, so they are recognised by what follows: "due to … previous
//! error", "generated …", "… emitted", a `test result:` prefix, cargo's
//! own closing lines, and a `FAIL` with nothing after it. Each is
//! skipped before anything is counted.
//!
//! Two things it knowingly does not do. It cannot tell where the last
//! command's output began — `pane-text` gives rows, not a range since an
//! event — so a short build whose predecessor's errors are still on
//! screen counts both; the four hundred rows are a judgement about how
//! often that is wrong versus how often a long build is cut short.
//! And it does not recognise linker errors (`ld: error:` has no line
//! number, and every linker words it differently), which is why a link
//! failure shows as `0 errors` — nothing, rather than a wrong number.

use std::cell::RefCell;

use haven_plugin_api::{coloured, focused, host, plain, Command, Event, Plugin, Run};

haven_plugin_api::plugin!(BuildErrors);

struct BuildErrors;

/// How far back to count. A cargo build with a dozen errors prints a few
/// hundred rows, so this reaches the top of most of them.
const ROWS: u32 = 400;

/// Indexed 1 and 3: the terminal's own red and yellow.
///
/// `coloured` is normally the wrong choice and the README says so, but
/// it earns it here. The count means *failure*, and failure has one
/// colour in every terminal anyone has used; a count of errors in the
/// theme's foreground reads as decoration beside the clock and the
/// branch name. The two low indices are not a hard-coded red: 0–15 come
/// from the user's own palette, so this is the red their theme chose,
/// in their contrast, rather than an RGB grey that disappears on half
/// the themes people have.
const RED: u8 = 1;
const YELLOW: u8 = 3;

thread_local! {
    static TALLY: RefCell<Tally> = RefCell::new(Tally::default());
}

#[derive(Default)]
struct Tally {
    /// Which pane this was counted from, so a focus change can drop it.
    pane: u32,
    errors: usize,
    warnings: usize,
    /// The host's refusal, kept so `render` can say why it is silent.
    refused: Option<String>,
}

impl Plugin for BuildErrors {
    fn on_event(event: Event) -> Vec<Command> {
        match event {
            Event::CommandEnded(end) => count_in(end.pane),
            // Output is about to be replaced; the old number is no
            // longer about anything.
            Event::CommandBegan(_) => clear(),
            Event::PaneFocus(id) => {
                let stale = TALLY.with(|cell| {
                    let tally = cell.borrow();
                    tally.refused.is_none() && tally.pane != id
                });
                if stale {
                    clear();
                }
            }
            _ => {}
        }
        // It draws; it does not ask Haven for anything.
        Vec::new()
    }

    fn render() -> Vec<Run> {
        TALLY.with(|cell| {
            let tally = cell.borrow();
            // Say why rather than drawing nothing: a plugin that is
            // blank because of a missing permission cannot be diagnosed.
            if let Some(why) = &tally.refused {
                return vec![plain(why.clone())];
            }
            // Clean output is the usual case, and silence is the right
            // answer to it. A `0 errors` segment would be noise on the
            // bar every day for the sake of the days it is not.
            if tally.errors == 0 && tally.warnings == 0 {
                return Vec::new();
            }
            let mut runs = Vec::new();
            if tally.errors > 0 {
                runs.push(coloured(
                    format!("{} {}", tally.errors, plural(tally.errors, "error")),
                    RED,
                ));
            }
            if tally.warnings > 0 {
                if !runs.is_empty() {
                    runs.push(plain(", "));
                }
                runs.push(coloured(
                    format!("{} {}", tally.warnings, plural(tally.warnings, "warning")),
                    YELLOW,
                ));
            }
            runs
        })
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

fn clear() {
    TALLY.with(|cell| *cell.borrow_mut() = Tally::default());
}

/// Read a pane once and count what the compiler said.
fn count_in(pane: u32) {
    // The event names the pane, but a command can finish in a pane
    // nobody is looking at, and this plugin is about the pane in front
    // of the user. The focused one wins when there is one.
    let pane = focused().map(|p| p.id).unwrap_or(pane);
    let text = match host::pane_text(pane, ROWS) {
        Ok(text) => text,
        Err(why) => {
            TALLY
                .with(|cell| *cell.borrow_mut() = Tally { refused: Some(why), ..Tally::default() });
            return;
        }
    };
    let (errors, warnings) = counted(&text);
    TALLY.with(|cell| *cell.borrow_mut() = Tally { pane, errors, warnings, refused: None });
}

/// Errors and warnings in some rows of build output.
fn counted(text: &str) -> (usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || is_summary(line) {
            continue;
        }
        // At most one count per line, so a gcc diagnostic that happens
        // to quote the word `error:` is not counted twice.
        match rust_kind(line).or_else(|| cc_kind(line)) {
            Some(true) => errors += 1,
            Some(false) => warnings += 1,
            None => {
                if is_failure(line) {
                    errors += 1;
                }
            }
        }
    }
    (errors, warnings)
}

/// A total rather than a diagnostic. The whole trap lives here.
fn is_summary(line: &str) -> bool {
    if line.starts_with("test result:") {
        return true;
    }
    // Cargo's own closing lines. Each is a report that the build failed,
    // which the errors above it already said.
    for closer in ["error: build failed", "error: test failed", "error: process didn't exit"] {
        if line.starts_with(closer) {
            return true;
        }
    }
    if line.starts_with("error") || line.starts_with("warning") {
        let lower = line.to_ascii_lowercase();
        // `error: aborting due to 2 previous errors`, and cargo's
        // `could not compile … due to 1 previous error`.
        if lower.contains("due to ") && lower.contains("previous error") {
            return true;
        }
        // `warning: `haven` (lib) generated 3 warnings`.
        if lower.contains(" generated ") {
            return true;
        }
        // `warning: 2 warnings emitted`.
        if lower.contains(" emitted") {
            return true;
        }
    }
    false
}

/// rustc and cargo. `Some(true)` is an error, `Some(false)` a warning.
fn rust_kind(line: &str) -> Option<bool> {
    for (word, error) in [("error", true), ("warning", false)] {
        let Some(rest) = line.strip_prefix(word) else { continue };
        // `error[E0308]:` as well as `error:`.
        let rest = if let Some(after) = rest.strip_prefix('[') {
            match after.split_once(']') {
                Some((_, tail)) => tail,
                None => continue,
            }
        } else {
            rest
        };
        if rest.starts_with(':') {
            return Some(error);
        }
    }
    None
}

/// gcc and clang: `file.c:12:5: error: …`.
fn cc_kind(line: &str) -> Option<bool> {
    for (needle, error) in [(": fatal error:", true), (": error:", true), (": warning:", false)] {
        if let Some(at) = line.find(needle) {
            if is_a_place(&line[..at]) {
                return Some(error);
            }
        }
    }
    None
}

/// Whether what came before the diagnostic is a `path:line[:col]`.
///
/// The digits are the whole test. Without them, any line of prose
/// containing `: error:` — a log message, a quoted diagnostic, somebody's
/// `echo` — would be counted.
fn is_a_place(before: &str) -> bool {
    let last = before.rsplit(':').next().unwrap_or("");
    !last.is_empty() && last.chars().all(|c| c.is_ascii_digit())
}

/// A test runner's failure, in the several spellings runners use.
fn is_failure(line: &str) -> bool {
    // Rust's harness: `test tests::thing ... FAILED`.
    if line.ends_with("... FAILED") || line.ends_with("... FAIL") {
        return true;
    }
    // go's `--- FAIL: TestThing (0.00s)`.
    let line = line.strip_prefix("--- ").unwrap_or(line);
    for word in ["FAILED", "FAIL"] {
        let Some(rest) = line.strip_prefix(word) else { continue };
        let rest = rest.trim_start_matches([':', '.', ' ', '\t']);
        // A bare `FAIL` with nothing after it is go's verdict on the
        // package, not another failing test.
        return rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '/' || c == '_');
    }
    false
}
