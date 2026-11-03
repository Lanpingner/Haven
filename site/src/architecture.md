# Architecture

Twelve crates, each with one job, arranged so the terminal core cannot reach a
frontend and the frontends cannot reach each other.

| Crate | Role |
|---|---|
| `haven-core` | Cells, styles, grid, VT interpretation. No rendering, pty or protocol — a pure state machine you drive with byte slices. |
| `haven-pty` | PTY allocation, child launch, job control, window sizing. |
| `haven-shell` | Shell detection and per-shell integration behind a `Shell` trait. |
| `haven-vtwrite` | Cells back to escape sequences: the inverse of the parser. |
| `haven-mux` | Sessions, windows, pane trees, the layout solver, the pane runtime. |
| `haven-proto` | Wire types, framing and transports. |
| `haven-client` | Frontend-agnostic client: frame model, input encoding, event loop. |
| `haven-tui` | The TUI frontend: ANSI output, raw mode, input decoding. |
| `haven-render` | The GPU frontend: wgpu pipelines, glyph atlas, procedural box drawing, the window. |
| `haven-daemon` | The mux server. |
| `haven` | Composition root: the CLI and the standalone mux. |
| `winit-probe` | A standalone winit/wgpu probe, kept because it is what found the X-server stall. |

## The layering is enforced, not documented

CI runs a job whose entire purpose is to fail if `haven-core` ever gains a path
to a frontend:

```yaml
- run: cargo tree -p haven-core --prefix none | grep -Eq '^(wgpu|winit)' && exit 1 || exit 0
```

Cheap to check, expensive to discover late.

## Why the daemon is not optional by omission

Standalone mode is not "the daemon code, skipped". It is the same mux core
driven in-process, which is why `haven tui` and `haven attach` cannot drift
apart in behaviour: there is one pane runtime, one layout solver and one key
table, and the only difference is whether a socket sits in the middle.

## Deliberate deviations from the plan

- **The daemon uses a poll loop, not tokio.** The pane pump was already
  poll-based and nothing here benefits from task-per-pane concurrency. Worth
  revisiting if per-pane parsing ever needs its own core.
- **The daemon was built before the plugin host** — M5 before M3.5 — because
  plugins are specified to run *in* the daemon. Building the host first would
  have meant building it in the wrong place and then moving it.
- **The status line and tab bar are built-in rather than plugins.** M3.5 inverts
  that; both are already pure functions of state, which is what makes the
  inversion cheap when the host exists.
- **Copy mode and the session tree live in `haven-client`**, not behind the key
  filter, because the client is the only place holding both halves of what they
  show — the view the frontend is drawing and the pane it belongs to. A filter
  owning them would be a second place that knows what a session is.

## One detail worth stating: coordinates in copy mode

The *cursor* is in view coordinates, because that is where it is drawn. The
*anchor* of a selection is in history coordinates, because selecting more than a
screenful means scrolling, and a view-relative anchor would slide out from under
the selection as soon as it did.

Row revisions cannot diff a moving view either: `rev` is a per-row counter, so
two unrelated rows can share one, and comparing by index is only sound while the
view sits still. The view is therefore resent whole whenever the offset moves or
history grows underneath it — which is cheap, because a client that is reading
is not a client watching a build scroll past.
