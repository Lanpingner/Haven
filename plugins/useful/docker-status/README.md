# docker-status

How many containers are running — and nothing at all on the machines
where that question has no answer.

```text
docker 8      eight containers up
podman 2      the other engine, same segment
docker 0      dim: the daemon is answering, nothing is running
              (nothing: no engine installed, or none that will answer)
```

## What it shows

Four outcomes, told apart deliberately:

| Situation | Shown | Why |
|---|---|---|
| containers running | `docker 8` | the number is the point |
| engine up, nothing running | `docker 0`, dim | worth four characters: it says the daemon is alive, so an empty list is news about your containers rather than about Docker |
| not installed | nothing | most machines, and there is nothing to say |
| daemon not running, or refusing | nothing | there is something to say and the status bar is the wrong place to say it |

The last one is the design decision in this plugin. A segment reading
`cannot connect to the Docker daemon` forever is clutter: it is long, it
is permanent, and it is not actionable from where you are reading it. On
a machine with no Docker — which is most of them — the plugin should be
indistinguishable from not being installed. So the engine's failure to
answer is logged with `host::log` and drawn as nothing.

The one error that *is* drawn is a refusal. Being denied a capability is
something the user can fix, and the host's refusal names the program, so
it is shown as it stands. A refusal only appears when *neither* engine
produced an answer, so granting `docker` alone and leaving `podman`
ungranted shows a count rather than a complaint.

## What it runs

`docker ps --format '{{.Names}}'`, on `started` and on each `tick`.

The format argument is the whole trick. Bare `docker ps` prints a table
with a header, column alignment that depends on the longest name, and a
`STATUS` column full of English — all of which would have to be parsed
and none of which is wanted. One name per line means the count is
`lines().count()`, and that is a parser that cannot drift. `-q` would do
as well; names are kept because they cost nothing here and the next
version of this plugin might want to show one.

## Podman

Handled. If `docker` does not answer — not installed, not granted, or a
daemon that will not talk — `podman ps --format '{{.Names}}'` is tried,
and the segment then reads `podman 2`, with the engine's own name, since
on a machine running podman "docker" would be the wrong word. Podman's
`ps` takes the same `--format` and prints the same one-name-per-line, so
there is no second parser.

Both engines are in the manifest's `exec` list. A user who runs only one
can grant only that one; the other comes back as a refusal, which is
exactly why a refusal is only shown when neither engine answered.

## Limits

- **Running containers only**, which is what `docker ps` means. Stopped
  containers are not in the number, and `-a` is not passed: a count that
  included every container you have ever started would change far more
  slowly and tell you much less.
- **One engine wins.** On a machine running both Docker and podman the
  segment reports Docker's count, because `docker` is tried first. There
  is no room on a status bar for two numbers and no way for the user to
  express a preference yet.
- **Refusal is told from absence by the wording of the error.** The WIT
  world gives `run` a single `result<output, string>`, so a capability
  that was not granted and a program that is not installed arrive as the
  same kind of value and are distinguished by looking for `not granted`
  in the host's sentence. It works, and it is matching on a human-readable
  string, which is the sort of thing that breaks quietly when somebody
  improves the wording. A distinguishable error — even just a two-case
  variant, `refused` or `failed` — would make this honest.
- **The count is cached, and `render` never shells out.** `docker ps` is
  a round trip to a daemon over a socket: slow when it is busy, and five
  seconds followed by a kill when it is wedged. `render` is called on
  every frame Haven draws, so a process spawn there would be a process
  per frame and, in the bad case, a terminal that stops drawing. The
  command runs on an event and `render` prints what is already known.
- **`docker` is the least narrow of the programs these three plugins
  ask for.** The `exec` allowlist covers `argv[0]` only, so a plugin
  granted `docker` may run it with any arguments — and a daemon that can
  start containers can mount the host's filesystem. It is still a
  reasonable grant, because "this plugin may talk to Docker" is a
  sentence a user can judge; `sh` is not. But it is the grant on this
  list that deserves a moment's thought before you give it.
