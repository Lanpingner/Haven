# Haven's plugins

Each directory under `useful/` is one plugin: a WebAssembly component
that Haven loads into the daemon, sandboxed, with only the permissions
its manifest asked for and the user granted.

`probes/` is not plugins. Those are fixtures, one per boundary that
`crates/haven-plugin-host` has to enforce — a plugin that loops forever,
one that tries to read `/etc/passwd`, one that refuses to start.

## Building

```console
$ cargo build --release --target wasm32-wasip2
```

from this directory. That is all: since Rust 1.82 the `wasm32-wasip2`
target emits a component directly, so there is no `cargo-component` and
no `wasm-tools` in the way. The artifacts land in
`target/wasm32-wasip2/release/<name>.wasm`, around 60–90 KB each.

`cargo test -p haven-plugin-host` from the repository root builds every
plugin here and runs them.

## Writing one

A plugin is a `cdylib` that implements one trait:

```rust
use haven_plugin_api::{plain, Plugin, Run};

haven_plugin_api::plugin!(Clock);

struct Clock;

impl Plugin for Clock {
    fn render() -> Vec<Run> {
        vec![plain("12:34")]
    }
}
```

Three entry points, all optional except that a plugin doing none of them
does nothing:

| Function | Called | For |
|---|---|---|
| `init() -> Result<(), String>` | once, before anything else | checking what you need is there. An `Err` unloads the plugin and shows the reason |
| `on_event(Event) -> Vec<Command>` | when something happens | reacting |
| `render() -> Vec<Run>` | when Haven draws | the status bar |

### The two rules

**`on_event` returns commands; it does not do things.** A plugin cannot
change Haven. It returns `Command::Notice`, `Command::Act`,
`Command::Remember` and Haven applies them. That is what makes a plugin
replayable against a recorded event stream with no terminal attached,
which is how they are tested.

**`render` is pure.** It is called when Haven is drawing, not when the
plugin feels like it, so no plugin can cause a redraw storm. Do not put
work in `render` that belongs in `on_event` — cache it in a `static` or
in the store and let `render` format what is already known.

### What you may ask of the host

```rust
host::log(text)                    // always allowed
host::panes() -> Vec<Pane>         // always allowed: ids, titles, cwd, command
host::pane_text(pane, rows)        // needs `read-pane-content`
host::run(argv) -> Result<Output>  // needs `exec`, and argv[0] must be allowlisted
host::recall(key) -> Option<String>// always allowed, scoped to this plugin
```

Everything that can be refused returns a `Result`, and a capability you
were not granted comes back as an `Err` with a sentence in it. **Show
it** — `vec![plain(why)]` — rather than returning nothing. A plugin that
silently draws nothing because it lacks a permission is a plugin nobody
can diagnose.

There is no filesystem, no network, no environment and no arguments. The
sandbox is not a list of things Haven forbids; it is a WASI context with
nothing in it, so `std::fs::read` fails like an empty disk. If you need
something from outside, it is `host::run` with an allowlisted program, or
it is not available.

`output_of(&["git", "branch"])` is the shorthand you want nine times in
ten: it runs the program, trims the trailing newline, and turns a
non-zero exit into an `Err` carrying the first line of stderr.

### Style on the status bar

Use `plain`, `bold` and `dim`, which take the theme's own colours. A
plugin that hard-codes a grey is unreadable on half the themes people
have. `coloured(text, n)` takes one of the 256 indexed colours when a
plugin genuinely needs to mean red.

Keep it short. The status bar is shared with the session name, the tabs
and the clock, so a segment is a handful of characters: `main*`, `2↑`,
`87%`. Not a sentence.

### Cost

Every byte is downloaded by whoever installs it. The release profile here
is already `opt-level = "s"`, LTO, stripped, `panic = "abort"`. Don't
pull in a dependency to do something small — formatting an hour out of a
timestamp is six lines, and `chrono` is a lot of kilobytes to learn it.

## Publishing

A plugin is distributed in a signed repository; see
[`site/src/plugins.md`](../site/src/plugins.md) for the user's side and
[`site/src/writing-plugins.md`](../site/src/writing-plugins.md) for how
to publish one. The short version: `plugins.yaml` names the artifact, its
SHA-256, its `entry`, its events and its capabilities, and a detached
GPG signature covers the lot.

Each plugin here carries a `MANIFEST.yaml` with the index entry it wants,
so building the reference repository is mechanical.

## What is not here yet

- **No configuration.** A plugin can `remember` and `recall`, but nothing
  lets a *user* set a value — so a plugin that needs a timezone or a
  threshold has to pick a sensible default. The store is the obvious place
  for this to land and it is not wired to anything yet.
- **No `net`.** The capability exists in the manifest vocabulary and the
  host refuses it, because nothing implements it.
- **Only the status bar.** `render` returns styled text for the bar.
  Overlays, pickers and panel widgets are in the plan and are not built,
  so a plugin cannot put up a window of its own.
