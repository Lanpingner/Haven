# ssh-marker

Marks the status bar when the focused pane is on another machine.

```
ssh romeo@box.example.com   →   ⇥ box
mosh 10.0.0.5               →   ⇥ 10.0.0.5
kubectl -n prod exec api-7  →   ⇥ api-7
distrobox enter arch        →   ⇥ arch
ssh                         →   ⇥
cargo test                  →   (nothing)
```

Bold, always. The point is to see it before typing the next command,
and `rm -rf` in the wrong machine is the mistake it exists to prevent —
so it prefers a visible marker to a precise one. Anything remote whose
destination cannot be read out of its arguments still gets a bare `⇥`.

A host keeps its first label (`box.example.com` → `box`) unless every
label is numeric, since `10` is not a useful way to say `10.0.0.5`.

## What it needs

No capabilities. It reads `pane.command` and `pane.title` from
`host::panes()`, and never what the pane is showing.

## What it cannot do

- **See a session it did not watch begin.** `command` is filled by shell
  integration (`OSC 133`). Without it this falls back to the pane title,
  and a title is a weak signal: a local shell commonly titles itself
  `romeo@arch: ~`, so an `@` is not treated as evidence. Only a launcher
  named in the title counts.
- **Follow a jump.** `ssh box` then `ssh` again from there shows the
  first host, because the second `ssh` is a command on the far machine
  and Haven never sees it.
- **Know about a launcher it has not heard of.** It knows `ssh`, `mosh`,
  `et`, `autossh`, `kubectl exec`, `docker exec`, `podman exec`,
  `distrobox enter` and `toolbox enter`. A wrapper script of your own
  looks local.
- **Avoid marking something harmless.** A command with the word `ssh` in
  it is marked. That is the direction to be wrong in.
