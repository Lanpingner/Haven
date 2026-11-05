//! Reading a shell's history file.
//!
//! The other half of a pane's history. What Haven saw through `OSC 133` is
//! this pane's, in order, with how each command ended — but it starts when
//! the pane does, and it needs integration loaded. The file holds months,
//! from every terminal, with no notion of panes and no statuses. Neither is
//! the whole answer, so both are offered and the caller merges them.
//!
//! Three formats, because three shells never agreed on one:
//!
//! * **bash** — one command per line, optionally preceded by `#<unix time>`
//!   when `HISTTIMEFORMAT` was set. A command containing a newline is
//!   written across several lines and cannot be told from several commands,
//!   which is bash's problem and now ours.
//! * **zsh** — `: <unix time>:<elapsed>;<command>` in extended mode, bare
//!   lines otherwise, with `\` continuing a line.
//! * **fish** — a YAML-ish list: `- cmd: <command>` with indented `when:`
//!   and `paths:` beneath it.
//!
//! Everything here is read-only and best effort. A history file that cannot
//! be read, or that is in a format this does not know, gives an empty list
//! rather than an error: the modal still has the pane's own commands.

use std::path::{Path, PathBuf};

/// The most recent entries, newest last. Capped because a history file is
/// unbounded and a client is going to be sent this.
pub fn recent(shell: &str, limit: usize) -> Vec<String> {
    let Some(path) = path_for(shell) else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let mut all = parse(shell, &text);
    if all.len() > limit {
        all.drain(..all.len() - limit);
    }
    all
}

/// Where a shell keeps its history, by the rules that shell actually uses.
pub fn path_for(shell: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    Some(match shell {
        // Both are honoured by fish; the XDG one is where it writes now.
        "fish" => {
            let xdg = data.join("fish/fish_history");
            if xdg.exists() {
                xdg
            } else {
                home.join(".config/fish/fish_history")
            }
        }
        "zsh" => {
            // `$HISTFILE` is the only authority, and it is commonly moved.
            if let Some(f) = std::env::var_os("HISTFILE") {
                PathBuf::from(f)
            } else if home.join(".zsh_history").exists() {
                home.join(".zsh_history")
            } else {
                home.join(".histfile")
            }
        }
        "bash" | "sh" => std::env::var_os("HISTFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".bash_history")),
        _ => return None,
    })
}

/// Parse a history file's text, oldest first.
pub fn parse(shell: &str, text: &str) -> Vec<String> {
    match shell {
        "fish" => parse_fish(text),
        "zsh" => parse_zsh(text),
        _ => parse_bash(text),
    }
}

fn parse_bash(text: &str) -> Vec<String> {
    text.lines()
        // A `#1699999999` line is a timestamp, not a command. A real
        // command can start with `#` too — it is a comment the user typed —
        // but one that is nothing but digits after the hash is bash's.
        .filter(|l| !is_bash_timestamp(l))
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

fn is_bash_timestamp(line: &str) -> bool {
    line.strip_prefix('#')
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn parse_zsh(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;
    // zsh writes history in its own metafied encoding; the bytes above 0x7f
    // are rare in commands and lossy UTF-8 already handled them, so this
    // works on the text.
    for line in text.lines() {
        let body = match line.strip_prefix(": ") {
            // `: <when>:<elapsed>;<command>`
            Some(rest) => rest.split_once(';').map(|(_, cmd)| cmd).unwrap_or(rest),
            None => line,
        };
        let continued = body.ends_with('\\');
        let body = body.strip_suffix('\\').unwrap_or(body);
        match &mut pending {
            Some(acc) => {
                acc.push('\n');
                acc.push_str(body);
            }
            None => pending = Some(body.to_string()),
        }
        if !continued {
            if let Some(done) = pending.take() {
                if !done.trim().is_empty() {
                    out.push(done);
                }
            }
        }
    }
    if let Some(done) = pending {
        if !done.trim().is_empty() {
            out.push(done);
        }
    }
    out
}

fn parse_fish(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        // Only `- cmd:` starts an entry. `when:` and the `paths:` list are
        // indented beneath it, so an unindented `- cmd:` is the one marker
        // that cannot be part of something else.
        let Some(cmd) = line.strip_prefix("- cmd: ") else { continue };
        out.push(unescape_fish(cmd));
    }
    out
}

/// fish escapes a backslash and a newline in its history file.
fn unescape_fish(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            // Anything else was not an escape: keep both characters, since
            // a command is more likely to contain a literal backslash than
            // fish is to have written one it did not mean.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Which shell a program is, for choosing a history file.
pub fn shell_name(program: Option<&Path>) -> String {
    program
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .or_else(|| {
            std::env::var_os("SHELL")
                .map(PathBuf::from)
                .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
        })
        .unwrap_or_else(|| "bash".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_timestamps_are_not_commands() {
        let text = "#1699999999\nls -la\n#1700000000\ncargo test\n\n";
        assert_eq!(parse("bash", text), vec!["ls -la", "cargo test"]);
        // A comment someone typed is a command they typed.
        assert_eq!(parse("bash", "# note to self\nls\n"), vec!["# note to self", "ls"]);
    }

    #[test]
    fn zsh_extended_entries_lose_their_metadata_and_keep_their_command() {
        let text = ": 1699999999:0;git status\n: 1700000000:12;cargo build --release\n";
        assert_eq!(parse("zsh", text), vec!["git status", "cargo build --release"]);
        // And the plain form, which is what zsh writes without EXTENDED_HISTORY.
        assert_eq!(parse("zsh", "ls\ncd /tmp\n"), vec!["ls", "cd /tmp"]);
    }

    #[test]
    fn a_zsh_command_split_over_lines_comes_back_as_one() {
        let text = ": 1:0;for f in *; do\\\necho $f\\\ndone\n: 2:0;ls\n";
        let got = parse("zsh", text);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0], "for f in *; do\necho $f\ndone");
        assert_eq!(got[1], "ls");
    }

    #[test]
    fn fish_entries_are_the_cmd_lines_and_nothing_else() {
        let text = "\
- cmd: git log --oneline
  when: 1699999999
- cmd: echo hello
  when: 1700000000
  paths:
    - some/file
";
        assert_eq!(parse("fish", text), vec!["git log --oneline", "echo hello"]);
    }

    #[test]
    fn fish_unescapes_what_it_escaped_and_leaves_what_it_did_not() {
        assert_eq!(parse("fish", "- cmd: echo a\\nb\n"), vec!["echo a\nb"]);
        assert_eq!(parse("fish", "- cmd: grep '\\\\d'\n"), vec!["grep '\\d'"]);
        // A backslash before anything else was never an escape.
        assert_eq!(parse("fish", "- cmd: echo \\t\n"), vec!["echo \\t"]);
    }

    #[test]
    fn an_unreadable_or_unknown_history_is_empty_rather_than_an_error() {
        // A modal with the pane's own commands in it is still worth having.
        assert!(recent("nushell", 10).is_empty());
        assert!(parse("elvish", "whatever\n").len() <= 1);
    }

    #[test]
    fn the_newest_entries_are_the_ones_kept() {
        let text: String = (0..50).map(|i| format!("echo {i}\n")).collect();
        let all = parse("bash", &text);
        assert_eq!(all.len(), 50);
        assert_eq!(all.last().unwrap(), "echo 49");
    }
}
