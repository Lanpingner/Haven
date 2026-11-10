//! Free space on the filesystem holding the focused pane's directory.
//!
//! The directory comes from `host::panes()`, which needs no capability —
//! a pane's title and working directory are not secrets, its contents
//! are — and the figures come from `df`, which does. `df` is a good
//! program to put in an `exec` allowlist: it reports on a filesystem and
//! cannot be talked into reading a file, so granting it gives up very
//! little. The allowlist matches `argv[0]` only, so the arguments are
//! entirely the plugin's choice; a plugin that asked for `sh` or `cat`
//! "to run `df`" would in fact have been granted the whole disk, and the
//! user agreeing to it would have no way to tell. Ask for the program
//! that does the one thing.
//!
//! Note that `host::run` has no working-directory argument, so the
//! directory has to be passed to `df` as an argument rather than by
//! running `df` *in* it. That happens to suit `df` exactly. It would not
//! suit a program that only ever reports on where it was started.
//!
//! # Why the reading is cached
//!
//! `render` runs on every frame Haven draws, and the host kills a
//! program that takes longer than five seconds — and `df` on a mount
//! whose server has gone away is precisely the program that takes longer
//! than five seconds. Spawning it from `render` would mean a process per
//! frame and a stall in the draw path. So it runs on `Event::Started`,
//! on `Event::Tick`, and on the two events that can change the answer —
//! the focus moving to another pane and a pane's directory changing —
//! and `render` formats the figures that are already in hand.
//!
//! # Free space, not percentage used
//!
//! Both are shown, but the absolute figure leads. "92%" on a four
//! terabyte disk still leaves room for anything you were about to do,
//! while "92%" on a 16G root partition means the next build fails, so
//! the percentage on its own cannot be acted on. The free figure can be:
//! it answers "will this fit". The percentage is kept, dim, because it
//! is the better number for noticing a trend.

use std::sync::Mutex;

use haven_plugin_api::{bold, dim, focused, host, output_of, plain, Command, Event, Plugin, Run};

haven_plugin_api::plugin!(DiskFree);

struct DiskFree;

static STATE: Mutex<Option<Reading>> = Mutex::new(None);

enum Reading {
    /// Free space as `df` wrote it, and the percentage in use.
    Space { free: String, used: u8 },
    /// A filesystem that does not report a size: an autofs placeholder,
    /// some overlays, anything `df` gives `0` or `-` for. There is a
    /// filesystem there, it just has no number worth printing.
    Unmeasured,
    /// No focused pane, or one whose directory Haven does not know —
    /// `cwd` is empty until the shell reports it with `OSC 7`.
    Nowhere,
    /// A refusal, or `df` failing. Shown verbatim: a plugin that draws
    /// nothing because it lacks a permission cannot be diagnosed.
    Why(String),
}

impl Plugin for DiskFree {
    fn on_event(event: Event) -> Vec<Command> {
        match event {
            // Everything that can change the answer, and the timer for
            // the thing that changes on its own: the free space itself.
            Event::Started | Event::Tick | Event::PaneFocus(_) | Event::CwdChanged(_) => refresh(),
            _ => {}
        }
        Vec::new()
    }

    fn render() -> Vec<Run> {
        match &*STATE.lock().unwrap() {
            None => vec![dim("df …")],
            Some(Reading::Why(why)) => vec![plain(why.clone())],
            // Short, and not a sentence: there is nothing useful to say
            // about a directory nobody has told us about.
            Some(Reading::Nowhere) => vec![dim("df ?")],
            Some(Reading::Unmeasured) => vec![dim("df —")],
            Some(Reading::Space { free, used }) => {
                let space = if *used >= 90 {
                    // Nearly full is the one case worth the eye, and
                    // weight says so without fighting the theme.
                    bold(free.clone())
                } else {
                    plain(free.clone())
                };
                vec![space, dim(format!(" {used}%"))]
            }
        }
    }
}

fn refresh() {
    let reading = match focused().map(|pane| pane.cwd).filter(|cwd| !cwd.is_empty()) {
        None => Reading::Nowhere,
        Some(cwd) => match output_of(&["df", "-P", "-h", &cwd]) {
            Err(why) => Reading::Why(why),
            Ok(text) => parse(&text).unwrap_or_else(|| {
                host::log(&format!("disk-free: could not parse `{text}`"));
                Reading::Why("df ?".into())
            }),
        },
    };
    *STATE.lock().unwrap() = Some(reading);
}

/// Read the one filesystem out of `df -P -h`.
///
/// ```text
/// Filesystem      Size  Used Avail Use% Mounted on
/// /dev/nvme1n1p1  469G  239G  207G  54% /Projects
/// ```
///
/// Found by the field ending in `%` rather than by counting from the
/// left, because a device name can have a space in it — macOS prints
/// `map auto_home` — and so can a mount point. The percentage is the one
/// field that cannot be mistaken for anything else, and `Avail` and
/// `Size` are at a fixed offset from it.
fn parse(text: &str) -> Option<Reading> {
    let line = text.lines().nth(1)?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    let at = fields.iter().position(|f| f.ends_with('%'))?;
    let used: u8 = fields[at].trim_end_matches('%').parse().ok()?;
    let size = fields.get(at.checked_sub(3)?)?;
    let free = fields.get(at - 1)?;
    if matches!(*size, "-" | "0" | "0B" | "0K") {
        return Some(Reading::Unmeasured);
    }
    Some(Reading::Space { free: humanly(free), used })
}

/// `df`'s free-space field, as something to print.
///
/// `-h` means the field already reads `207G` and there is nothing to do.
/// BSD's `df` gives `-P` precedence over `-h` and prints blocks instead,
/// so a field that is all digits is taken as 1K blocks and converted
/// here. Doing it this way rather than parsing the number unconditionally
/// keeps `df`'s own rounding, which is the one the user sees everywhere
/// else.
fn humanly(field: &str) -> String {
    if !field.chars().all(|c| c.is_ascii_digit()) {
        return field.to_string();
    }
    // In tenths of a unit, and in integers throughout: `f64` parsing and
    // formatting is tens of kilobytes in a component that somebody has to
    // download, and this is two divisions.
    let Ok(blocks) = field.parse::<u64>() else { return field.to_string() };
    let units = ['K', 'M', 'G', 'T'];
    let mut tenths = blocks.saturating_mul(10);
    let mut unit = 0;
    while tenths >= 10_240 && unit + 1 < units.len() {
        tenths /= 1024;
        unit += 1;
    }
    let unit = units[unit];
    // One decimal below ten, none above: `9.4G`, `207G`. Which is what
    // `df -h` does, and the status bar has no room for more.
    if tenths < 100 {
        format!("{}.{}{unit}", tenths / 10, tenths % 10)
    } else {
        format!("{}{unit}", (tenths + 5) / 10)
    }
}
