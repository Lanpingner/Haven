//! Opening what a hyperlink points at.
//!
//! On the **client**, never the daemon. The daemon may be on another
//! machine entirely — that is the whole point of attaching over SSH — and
//! the browser belongs beside the screen you are looking at, not beside the
//! shell that printed the link.
//!
//! What may be opened is deliberately narrow. An `OSC 8` target is written
//! by whatever is running in the pane: `cat` a file from anywhere and it can
//! put any link it likes on your screen, under any text it likes. The text
//! says `the docs` and the target is invisible until something shows it to
//! you. Handing that to `xdg-open`, which will start whichever application
//! claims the scheme, turns printed output into one click from running a
//! program.
//!
//! So: the three schemes a link is nearly always one of are opened, and
//! anything else is *shown* instead — the status bar names the target and
//! nothing is launched. Seeing where a link goes and deciding for yourself
//! is a worse experience than clicking it, and a much better one than
//! finding out afterwards.

/// Schemes safe enough to hand to the desktop without showing them first.
///
/// `file://` is not among them, and the omission is the point: it is the
/// scheme that reaches a `.desktop` entry or a script, and it is also the
/// one a build tool emits most — which is exactly the combination that gets
/// clicked without being read.
const OPENABLE: &[&str] = &["http://", "https://", "mailto:"];

/// What happened, for the status bar to say.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Opened {
    /// Handed to the desktop.
    Launched(String),
    /// Shown rather than launched: the scheme is not on the list.
    Shown(String),
    Failed(String),
}

pub fn open(target: &str) -> Opened {
    let lower = target.to_ascii_lowercase();
    if !OPENABLE.iter().any(|scheme| lower.starts_with(scheme)) {
        return Opened::Shown(target.to_string());
    }
    // A target with a control character in it has no business reaching a
    // shell-adjacent launcher, whatever its scheme says.
    if target.chars().any(|c| c.is_control()) {
        return Opened::Shown(target.escape_debug().to_string());
    }
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    match std::process::Command::new(opener)
        .arg(target)
        // Detached: a browser that takes a second to start must not hold up
        // the frame, and its output is not ours to collect.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => Opened::Launched(target.to_string()),
        Err(e) => Opened::Failed(format!("{opener}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_ordinary_schemes_are_launched() {
        // Nothing here starts a process: `Shown` is decided before the
        // launcher is reached.
        assert!(matches!(open("file:///etc/passwd"), Opened::Shown(_)));
        assert!(matches!(open("vscode://file/tmp/x"), Opened::Shown(_)));
        assert!(matches!(open("javascript:alert(1)"), Opened::Shown(_)));
        // A scheme that merely begins like a safe one is not one.
        assert!(matches!(open("https-evil://x"), Opened::Shown(_)));
        assert!(matches!(open("not-really-https://x"), Opened::Shown(_)));
    }

    #[test]
    fn a_control_character_disqualifies_an_otherwise_safe_target() {
        // A link whose target contains a newline is not a link anyone
        // wrote by accident.
        assert!(matches!(open("https://example.com/\nrm -rf"), Opened::Shown(_)));
    }

    #[test]
    fn the_scheme_test_ignores_case() {
        // `HTTPS://` is legal and a case-sensitive check would quietly send
        // it down the "show, do not open" path.
        let target = "HTTPS://example.com/";
        let lower = target.to_ascii_lowercase();
        assert!(OPENABLE.iter().any(|s| lower.starts_with(s)));
    }
}
