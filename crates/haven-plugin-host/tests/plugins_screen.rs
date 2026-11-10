//! The three plugins that read the screen, run against screens.
//!
//! `url-hints`, `build-errors` and `secret-guard` all ask for
//! `read-pane-content`, which is the capability Haven names first in
//! every consent prompt. That makes them the most important plugins to
//! have tests for and, as it happens, the easiest: `Session::showing`
//! hands a plugin exactly the output you want it to see, so "what does
//! this plugin say about a cargo build with two errors in it" is a
//! question with a literal answer.
//!
//! Every test here comes in two halves. Without the grant, the plugin
//! must *say* that it was refused — a plugin that draws nothing because
//! of a missing permission is one nobody can diagnose — and with the
//! grant it must get the hard cases right: a URL at the end of a
//! sentence, the same URL twice, cargo's "due to 2 previous errors"
//! summary line, clean output, and a token that is named but never
//! quoted.
//!
//! If the `wasm32-wasip2` target is not installed these tests say so
//! and pass, which is why each one begins by returning on `None`.

use haven_plugin_host::testing::{plain_load, with_pane_text, Session};
use haven_plugin_host::{text_of, Command as PluginCommand, CommandEnd, Event};

/// A session with one focused pane showing some output.
fn screen(text: &str) -> Session {
    Session::new().pane(1, "fish", "/Projects/HavenTerminal").showing(1, text)
}

/// A command finishing in pane 1, which is when all three plugins look.
fn ended() -> Event {
    Event::CommandEnded(CommandEnd {
        pane: 1,
        command: "cargo build".into(),
        status: 1,
        millis: 4_200,
    })
}

fn notices(commands: &[PluginCommand]) -> Vec<String> {
    commands
        .iter()
        .filter_map(|c| match c {
            PluginCommand::Notice(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------- url-hints

#[test]
fn url_hints_without_the_grant_says_which_permission_it_wanted() {
    let Some(mut p) = plain_load("url_hints", screen("see https://example.com/a")) else { return };
    p.on_event(&ended()).expect("event");
    let drawn = text_of(&p.render().expect("render"));
    assert!(drawn.contains("read-pane-content"), "expected the refusal, got {drawn:?}");
    assert!(!drawn.contains("link"), "it cannot know whether there were links: {drawn:?}");
}

#[test]
fn url_hints_counts_the_distinct_links_and_keeps_the_last() {
    // A realistic screen: a note printing the same documentation URL
    // twice, one in brackets, a bare `www.`, and one with a full stop
    // after it.
    let output = "\
$ cargo doc --open
warning: unused import: `std::fmt`
  = note: see https://doc.rust-lang.org/error_codes/E0432.html for more
See https://doc.rust-lang.org/error_codes/E0432.html for more
Opened (https://docs.rs/haven/latest/haven/index.html) in the browser.
Mirror at www.example.org/haven, or http://localhost:8080/docs.
";
    let Some(mut p) = with_pane_text("url_hints", screen(output)) else { return };
    p.on_event(&ended()).expect("event");
    let drawn = text_of(&p.render().expect("render"));
    // Four distinct links, not five: the documentation URL appears
    // twice and a count that said five would be a lie.
    assert!(drawn.starts_with("4 links"), "{drawn:?}");
    // And the latest is the last one on the screen, shortened to a host.
    assert!(drawn.contains("localhost:8080/…"), "{drawn:?}");
}

#[test]
fn url_hints_knows_where_a_url_stops() {
    // The trailing full stop belongs to the sentence. If it did not come
    // off, the segment would read `example.com.` — which is the whole
    // reason the scanner is written by hand rather than as a match on
    // "non-space characters".
    let Some(mut p) = with_pane_text("url_hints", screen("Read the notes at https://example.com."))
    else {
        return;
    };
    p.on_event(&ended()).expect("event");
    let drawn = text_of(&p.render().expect("render"));
    assert_eq!(drawn, "1 link example.com", "{drawn:?}");
}

#[test]
fn url_hints_keeps_the_brackets_that_are_part_of_the_url() {
    let output = "see (https://en.wikipedia.org/wiki/Terminal_(typewriter)) for the history";
    let Some(mut p) = with_pane_text("url_hints", screen(output)) else { return };
    p.on_event(&ended()).expect("event");
    let drawn = text_of(&p.render().expect("render"));
    // One link, and it still has a path: the outer bracket came off and
    // the inner pair did not.
    assert_eq!(drawn, "1 link en.wikipedia.org/…", "{drawn:?}");
}

#[test]
fn url_hints_is_silent_when_nothing_on_the_screen_is_a_link() {
    // None of these is one: a bare `www.` with a single dot is prose, a
    // scheme with no host is not somewhere to go, and a version number
    // is not a domain.
    let output = "\
$ ls
Cargo.toml  src  target
checking www.foo and http:// and haven 0.1.0
";
    let Some(mut p) = with_pane_text("url_hints", screen(output)) else { return };
    p.on_event(&ended()).expect("event");
    assert_eq!(text_of(&p.render().expect("render")), "", "something was taken for a link");
}

// ------------------------------------------------------------- build-errors

#[test]
fn build_errors_without_the_grant_says_which_permission_it_wanted() {
    let Some(mut p) = plain_load("build_errors", screen("error: something went wrong")) else {
        return;
    };
    p.on_event(&ended()).expect("event");
    let drawn = text_of(&p.render().expect("render"));
    assert!(drawn.contains("read-pane-content"), "expected the refusal, got {drawn:?}");
}

#[test]
fn build_errors_counts_a_cargo_wall() {
    let output = "\
   Compiling haven v0.1.0 (/Projects/HavenTerminal)
warning: unused variable: `rows`
  --> src/lib.rs:12:9
   |
12 |     let rows = 200;
   |         ^^^^ help: if this is intentional, prefix it with an underscore
error[E0308]: mismatched types
  --> src/lib.rs:30:21
   |
30 |     let text: u32 = pane_text();
   |               ^^^ expected `u32`, found `String`
error: cannot find value `pane` in this scope
  --> src/lib.rs:44:9
warning: `haven` (lib) generated 1 warning
error: could not compile `haven` (lib) due to 2 previous errors; 1 warning emitted
";
    let Some(mut p) = with_pane_text("build_errors", screen(output)) else { return };
    p.on_event(&ended()).expect("event");
    let drawn = text_of(&p.render().expect("render"));
    // Two errors and one warning. The `generated 1 warning` line and the
    // `due to 2 previous errors` line are totals, not diagnostics.
    assert_eq!(drawn, "2 errors, 1 warning", "{drawn:?}");
}

#[test]
fn build_errors_does_not_count_the_summary_line_twice() {
    // The trap, on its own. One real error, and two lines that begin
    // exactly like one. A plugin that counted them would say `3 errors`
    // and nobody would trust the number again.
    let output = "\
error[E0425]: cannot find value `x` in this scope
 --> src/main.rs:2:5
error: aborting due to 1 previous error
error: could not compile `demo` (bin \"demo\") due to 1 previous error
";
    let Some(mut p) = with_pane_text("build_errors", screen(output)) else { return };
    p.on_event(&ended()).expect("event");
    assert_eq!(text_of(&p.render().expect("render")), "1 error");
}

#[test]
fn build_errors_counts_gcc_and_clang_by_the_line_number() {
    let output = "\
$ make
cc -c -o main.o main.c
main.c:12:5: error: expected ';' before '}' token
main.c:18:1: warning: unused variable 'x' [-Wunused-variable]
util.c:3:10: fatal error: missing.h: No such file or directory
compilation terminated.
make: *** [Makefile:7: main.o] Error 1
";
    let Some(mut p) = with_pane_text("build_errors", screen(output)) else { return };
    p.on_event(&ended()).expect("event");
    assert_eq!(text_of(&p.render().expect("render")), "2 errors, 1 warning");
}

#[test]
fn build_errors_counts_a_test_runners_failures_but_not_its_verdict() {
    let output = "\
test urls::a_url_at_the_end_of_a_sentence ... ok
test urls::a_url_wrapped_across_two_rows ... FAILED
FAILED: build/haven.o
test result: FAILED. 3 passed; 1 failed; 0 ignored
";
    let Some(mut p) = with_pane_text("build_errors", screen(output)) else { return };
    p.on_event(&ended()).expect("event");
    // Two failures. `test result: FAILED.` is the verdict on those two.
    assert_eq!(text_of(&p.render().expect("render")), "2 errors");
}

#[test]
fn build_errors_says_nothing_about_a_clean_build() {
    let output = "\
   Compiling haven v0.1.0 (/Projects/HavenTerminal)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.41s
     Running unittests src/lib.rs (target/debug/deps/haven-9f86d0818)
test result: ok. 128 passed; 0 failed; 0 ignored; 0 measured
";
    let Some(mut p) = with_pane_text("build_errors", screen(output)) else { return };
    p.on_event(&ended()).expect("event");
    let runs = p.render().expect("render");
    assert!(runs.is_empty(), "a clean build should draw nothing at all: {runs:?}");
}

#[test]
fn build_errors_forgets_the_count_when_the_next_command_starts() {
    // A number from the last build sitting beside a build that is still
    // running is worse than no number.
    let output = "error: cannot find value `x` in this scope\n";
    let Some(mut p) = with_pane_text("build_errors", screen(output)) else { return };
    p.on_event(&ended()).expect("event");
    assert_eq!(text_of(&p.render().expect("render")), "1 error");
    p.on_event(&Event::CommandBegan(haven_plugin_host::CommandStart {
        pane: 1,
        command: "cargo build".into(),
    }))
    .expect("began");
    assert!(p.render().expect("render").is_empty(), "the old count should be gone");
}

// ------------------------------------------------------------- secret-guard

#[test]
fn secret_guard_without_the_grant_says_so_rather_than_guarding_nothing() {
    // The worst failure mode for this plugin specifically: silence that
    // looks exactly like "nothing was found".
    let Some(mut p) = plain_load("secret_guard", screen("AKIAIOSFODNN7EXAMPLE")) else { return };
    let commands = p.on_event(&ended()).expect("event");
    assert!(notices(&commands).is_empty(), "it cannot have found anything: {commands:?}");
    let drawn = text_of(&p.render().expect("render"));
    assert!(drawn.contains("read-pane-content"), "expected the refusal, got {drawn:?}");
}

#[test]
fn secret_guard_names_the_kind_and_never_the_secret() {
    // The assertion that matters most in this file. A notice carrying
    // the token would print it a second time, onto the status bar, where
    // it can be logged and screenshotted — which would make the plugin
    // the leak it is warning about.
    let token = "ghp_16C7e42F292c6912E7710c838347Ae178B4a";
    let output = format!(
        "$ gh auth token\n{token}\n$ aws configure get aws_access_key_id\nAKIAIOSFODNN7EXAMPLE\n"
    );
    let Some(mut p) = with_pane_text("secret_guard", screen(&output)) else { return };
    let commands = p.on_event(&ended()).expect("event");
    let said = notices(&commands);
    assert_eq!(said.len(), 1, "one notice, whatever was found: {commands:?}");
    let notice = &said[0];

    assert!(notice.contains("AWS access key"), "{notice:?}");
    assert!(notice.contains("GitHub token"), "{notice:?}");
    assert!(notice.contains("pane 1"), "it should say where: {notice:?}");

    // Not the secret, nor any piece of it long enough to be useful.
    assert!(!notice.contains(token), "the notice printed the token: {notice:?}");
    assert!(!notice.contains("16C7e42F292c6912"), "part of the token leaked: {notice:?}");
    assert!(!notice.contains("AKIA"), "the notice printed the key: {notice:?}");
    assert!(!notice.contains("IOSFODNN7EXAMPLE"), "part of the key leaked: {notice:?}");

    // And it does not put anything on the status bar either.
    assert!(p.render().expect("render").is_empty(), "nothing read should be drawn");
}

#[test]
fn secret_guard_recognises_a_private_key_and_a_bearer_header() {
    let output = "\
$ cat deploy.log
> POST /v1/deploy HTTP/1.1
> Authorization: Bearer eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkEifQ
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2
-----END OPENSSH PRIVATE KEY-----
";
    let Some(mut p) = with_pane_text("secret_guard", screen(output)) else { return };
    let said = notices(&p.on_event(&ended()).expect("event"));
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("private key"), "{said:?}");
    assert!(said[0].contains("bearer token"), "{said:?}");
    assert!(!said[0].contains("eyJzdWIiOiIx"), "the header's value leaked: {said:?}");
    assert!(!said[0].contains("b3BlbnNzaC1rZXk"), "the key's body leaked: {said:?}");
}

#[test]
fn secret_guard_keeps_quiet_about_what_only_looks_suspicious() {
    // Every line here would be caught by a plugin that guessed: a usage
    // message full of the word "password", a path to a key rather than a
    // key, an indirection through another variable, a short `sk-`
    // placeholder, the English word "bearer", and a git object id, which
    // is the shape every entropy heuristic fires on.
    let output = "\
$ ./deploy --help
      --password=<password>   the password to use
  -t, --token=TOKEN           an API token
Reading the deploy key from ~/.ssh/id_ed25519
export SSH_KEY=~/.ssh/id_ed25519
export GITHUB_TOKEN=$GH_PAT
model: sk-test
Bearer token authentication is supported.
HEAD is now at 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
";
    let Some(mut p) = with_pane_text("secret_guard", screen(output)) else { return };
    let commands = p.on_event(&ended()).expect("event");
    assert!(
        notices(&commands).is_empty(),
        "a false positive a day gets the plugin uninstalled: {commands:?}"
    );
    assert!(p.render().expect("render").is_empty(), "and nothing on the bar either");
}

#[test]
fn secret_guard_does_not_say_the_same_thing_twice() {
    // The token is still on the screen after the next command, because
    // scrollback is what scrollback is. Warning again every time
    // anything finishes would train people to ignore it.
    let output = "$ env | grep AWS\nAWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n";
    let Some(mut p) = with_pane_text("secret_guard", screen(output)) else { return };
    assert_eq!(notices(&p.on_event(&ended()).expect("event")).len(), 1);
    assert!(
        notices(&p.on_event(&ended()).expect("event")).is_empty(),
        "the same find on an unchanged screen is not news"
    );
}

#[test]
fn secret_guard_notices_an_exported_secret_but_not_a_path_to_one() {
    let output = "$ cat .env\nexport OPENAI_API_KEY=hunter2hunter2hunter2\n";
    let Some(mut p) = with_pane_text("secret_guard", screen(output)) else { return };
    let said = notices(&p.on_event(&ended()).expect("event"));
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("export"), "{said:?}");
    assert!(!said[0].contains("hunter2"), "the value leaked: {said:?}");
}
