# Plugin fixtures

Not plugins. Each directory under `probes/` exists so that a boundary
`crates/haven-plugin-host` has to enforce has something to test against:

| Probe | What it is for |
|---|---|
| `exec_probe` | runs a program, or reports being refused one |
| `pane_probe` | reads a pane, or reports being refused |
| `event_probe` | turns events into commands, counts ticks, logs |
| `spin_probe` | never returns, so the fuel limit has something to stop |
| `fail_probe` | refuses to start, so a load failure has a reason to report |
| `sandbox_probe` | tries to read `/etc/passwd`, and must not manage it |

`cargo test -p haven-plugin-host` from the repository root builds these
and runs them.

## The plugins themselves

They live in **[haven-terminal/plugins](https://github.com/haven-terminal/plugins)**,
with the script that publishes them as a signed repository and the tests
that run them against this runtime. See
[`site/src/writing-plugins.md`](../site/src/writing-plugins.md) for how
to write one and
[`site/src/plugins.md`](../site/src/plugins.md) for how to install one.

Keeping them apart keeps each repository answering one question. This
one answers "does the runtime hold its boundaries"; the other answers
"what can you install, and does it work".

## Building a probe

```console
$ cargo build --release --target wasm32-wasip2
```

from this directory, exactly as a real plugin is built. The components
land in `target/wasm32-wasip2/release/`.
