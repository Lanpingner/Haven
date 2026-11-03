# A pane that is another distro

`Alt+C` lists the shells on your machine. Below them, in the same menu, it
lists containers: the ones already running, the images you have, and a handful
of distros that are not here yet. Pick `archlinux:latest` and the pane is an
Arch shell.

The model is nix-shell's, not a container runtime's. You are not administering
anything. You want a pane that is Arch for ten minutes to check whether a
build breaks there, or a Debian you keep around all week.

## One menu, because it is one decision

"Give me a shell" does not split into *which shell* and *where it runs* until
after you have seen the list. A separate key for containers would mean
choosing between the two before you knew what either offered — so there is no
separate key. The shell chooser grew a second half:

```
fish       /usr/bin/fish
bash       /bin/bash
container  web               nginx:latest · Up 2 hours
container  alpine:latest     7.8MB
container  archlinux:latest  not pulled yet
```

The order is not alphabetical and not arbitrary: shells, then containers that
are already up, then images you have, then images that would have to be
fetched. Nothing that costs a download ever ranks above something that is
already here, and nothing that costs a download is hidden either — a menu that
is empty until you have already done the thing by hand is not a feature.

### What a machine with sixty images gets

A workstation that builds things has a lot of images, and the first version of
this listed all of them: sixty-two rows, mostly tags of the same handful of
builds, with `archlinux` somewhere near the bottom. Three rules make that a
menu again, and none of them is a filter you cannot see past.

- **One line per image, not per tag.** Six tags of one build are six ways of
  saying the same thing. The name shown is the one you would have typed —
  unhosted over hosted, short over long, `:latest` over a commit — and the
  rest are counted: `2.95GB · +2 other tags`.
- **One line per repository for build tags.** `e917629-dirty`, `27803cc`,
  `v0.1-rc1-6-g182cc9a` — a commit in the shape every CI pipeline writes it.
  Those are distinct images that cannot be folded by identity, and a machine
  accumulates dozens. The newest is shown, the rest are counted. A tag
  somebody *chose* — `latest`, `22-alpine`, `16`, `release` — is never folded,
  because choosing between those is the point of the menu.
- **Distros first.** A base image ranks above a plain image, which ranks above
  one from a registry — which is nearly always something you built rather than
  something you want a shell in.

On the machine this was written on, sixty-two rows became forty-one, with
`alpine` at the top of them.

Past about a dozen entries the menu is narrowed by **typing** rather than
picked from by number: digits stop at nine, and there is no key `23`. Type
`arch` and you are looking at one line. A short list keeps the numbers printed
beside it, because for four shells a digit is faster than a word.

Anything folded is still reachable by name, which is all a menu entry is:

```sh
haven run -- new-window 'docker://run/127.0.0.1:5000/app:e917629-dirty?scratch'
```

`Ctrl+A` `g` is the same menu; `Ctrl+A` `G` opens the pick as a pane instead of
a tab.

## The second question

An image is asked one thing before it opens:

```
archlinux:latest — keep what you install in it?
  keep        packages and files survive the pane
  scratch     gone when the pane closes
```

It is asked per pane because the answer differs per pane. A scratch Arch to
try one command should leave nothing behind; a Ubuntu you are building in all
week has to remember that you installed the compiler. Both are the right
answer to different afternoons, and a config key would force one of them on
both.

- **scratch** is a container of its own, started with `--rm`, removed when the
  pane closes. Truly nix-shell: nothing you install survives, and nothing is
  left behind.
- **keep** is one lasting container per image, named `haven-<image>`, reused by
  every later pane. Close the pane, open it next week, your packages are still
  there. It is visible in `docker ps -a` under a name that says where it came
  from, so `docker rm haven-archlinux-latest` is the whole of throwing it away.

A **scratch** pane removes its container when you close it, and a **kept**
container is left running until you stop it yourself — stopping it would pull
the floor from under another pane sharing it. Either way `docker ps` tells the
truth about what is up.

A pane that is *killed* rather than closed is the awkward case, and worth
knowing about because the first implementation got it wrong. `--rm` removes a
container when it *exits*; it cannot help when the CLI is killed, because
nothing downstream ever finds out — `SIGHUP` arrives at the container's PID 1,
which the kernel does not deliver default-action signals to, so a cleanup
handler never runs. It leaked one container per killed pane, which is the sort
of thing you only see by running it.

So every scratch container is labelled with the pid of the process its pane
runs, and opening the next container pane throws away the ones whose pid is
gone. The leak heals itself, at the moment a stale container would have become
confusing anyway. `docker ps -a --filter label=haven.scratch` is the list, if
you would rather look.

A container that is *already running* is never asked: it outlives the pane
whatever the pane does, because it was not the pane that made it. Picking one
is `docker exec`, a second shell alongside whatever the container is doing.

## A pull says what it is doing

A pane that sits blank while half a gigabyte arrives is indistinguishable from
a pane that is broken. So the fetch happens in the pane, out loud:

```
haven: archlinux:latest is not here yet — pulling it once, then opening a shell
latest: Pulling from library/archlinux
a1b2c3d4: Downloading  184.2MB/412.6MB
```

Then the shell opens. The "once" is the part worth saying: the next pane from
that image starts instantly, and people who do not know that assume every
Arch pane will cost four minutes.

## Nothing goes wrong invisibly

Every way this can fail ends in a working pane and a line saying why:

```
haven: docker is not installed, so this pane is a local shell instead
haven: could not pull archlinux:latest, so this pane is a local shell instead
haven: container web is not running, so this pane is a local shell
```

That is not politeness, it is the only honest option. The pane program is a
short shell script rather than `docker` itself, precisely so that these
branches have somewhere to be. A pane that opened and closed again before you
could read it would take the explanation with it — and a spawn failure handed
back to the daemon would mean the event loop learning to care whether Docker
is installed.

The last line is the one to read twice: asking for a container that has since
stopped gets you a local shell, not a started container. Starting it is a
different thing from what you asked for, and Haven does not quietly do
different things.

## Your files are not in there

A container pane sees nothing of this machine until you say so. There is no
default mount, because a distro pane is most often opened precisely to be
*away* from your filesystem.

```
:container-mount cwd         # the directory the pane is opened in
:container-mount /Projects   # that path, at the same path inside
:container-mount /srv:/work  # that path, somewhere else inside
:container-mounts            # say what they are
:container-unmount /srv      # stop handing it over
```

`mount`, `unmount` and `mounts` are the short names. A mount can only be
decided before a container starts, so this belongs to the act of opening a
pane rather than to the session — the list applies to container panes you open
from then on, and the status bar says what it is each time you change it.

Two deliberate details. `cwd` is a word, not a path: it means whatever
directory the pane turns out to be opened in, resolved where the pane is
spawned rather than where you typed it. And a path with no destination lands
at *the same path* inside, so a filename in an error message means one thing in
both halves of your screen. The working directory follows the first mount.

## Save and restore

A container pane is recorded in the save file as what it is:

```toml
[[session.window.pane]]
cwd = "/Projects/HavenTerminal"
program = "docker://run/archlinux:latest?keep"
```

On restore the line is drawn exactly where [sessions](./sessions.md) draws it:
a pane rejoins a container that is **already running**, and a restore never
starts one. A save file decides what a machine runs when it comes back, which
is the same reason `restore_programs` is an allowlist rather than "re-run
whatever was in the foreground". A container that is gone leaves an ordinary
shell in the pane — which is what every other restored pane is anyway.

Only a container spec is ever written to that field. An arbitrary program is
not, however it was launched.

## Integration, offered rather than installed

Prompt marks, `OSC 7` directories and `$HAVEN_*` work in a container pane when
they can. The integration script is **offered** to the container through a
read-only mount at `/haven`, and the shell inside loads it if it is there:

- bash gets the real thing, through `--rcfile`, after the container's own
  `/etc/bash.bashrc` and `~/.bashrc` — a distro's bashrc is most of what makes
  its shell feel like that distro.
- anything else gets the environment and no marks.

Nothing inside the container is modified, nothing is installed in it, and no
pane fails because integration did not happen. A pane with no prompt marks is
less capable, not broken.

Which shell runs is decided *inside*, by `command -v`, trying bash, zsh, ash
and then sh. It cannot be decided outside: alpine has ash and no bash, Arch
has bash, and guessing from an image name would be wrong for every image that
is not on a list someone maintained.

## Configuration

```toml
[containers]
enabled = true
cli = "docker"
images = ["archlinux:latest", "ubuntu:latest", "debian:latest",
          "fedora:latest", "alpine:latest", "opensuse/tumbleweed:latest",
          "rockylinux:9", "nixos/nix:latest"]
```

`images` is the list offered when it is *not* pulled; it replaces the built-in
list rather than adding to it, so a list you wrote is the list you get.
Everything you have pulled is offered regardless.

`cli` looks like the least important line and is the most. `podman` takes every
argument Haven passes here unchanged, so pointing this at it is the whole of
podman support. Nothing in the code spells `docker` except this default — a
test asserts it, by building a launch with `cli = "podman"` and failing if the
word `docker` appears anywhere in the result.

## Where it is written

Containers are one implementation of a `Backend` trait, which is deliberately
the shape [`Shell`](./architecture.md) already has: one impl per thing, a
registry, and nothing else branching on which one it is. The next one — a VM,
an ssh host, a namespace — is an impl and a line in the registry.

The feature crosses the existing seams rather than opening new ones. A virtual
shell is named by a string, `docker://run/archlinux:latest?keep`, which travels
as the pane's **program**: the field that already existed, that the daemon
already interprets on its own machine, that the save file already records and
that the command line already accepts. Which is why this works without a line
of container code anywhere near it:

```sh
haven run -- new-window 'docker://run/alpine:latest?scratch'
```

Nothing in the protocol, the mux or the terminal emulator knows what a
container is. The daemon resolves the spec when it spawns the pane, because
that is where the containers are — a client's idea of which images are pulled
means nothing once a session is on another host, for exactly the reason its
idea of where `fish` lives means nothing.
