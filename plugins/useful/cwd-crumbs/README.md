# cwd-crumbs

The focused pane's working directory, short enough to live on a status
bar next to the session name and the clock.

```
/home/romeo/Projects/HavenTerminal/crates/haven-client   →   ~/P/H/c/haven-client
/usr/local/share/fonts                                   →   /u/l/s/fonts
/home/romeo/.config/fish                                 →   ~/.c/fish
/tmp                                                     →   /tmp
/                                                        →   /
/home/romeo                                              →   ~
```

The ancestors are drawn dim and the last component plain, so the eye
lands on the part that changes.

## The rules

- Every component but the last is cut to its first character. A
  component starting with `.` keeps two, because a lone dot distinguishes
  nothing.
- The last component is kept whole, up to 20 characters, after which it
  is cut with `…`.
- A path under a home directory starts `~`.
- `/` stays `/`, and a single-component path is already as short as it
  goes.
- An unknown directory draws nothing at all.

## What it needs

No capabilities. It reads `host::panes()`, which needs no grant, every
time Haven draws.

## What it cannot do

- **Find your home directory properly.** A plugin has no environment, so
  there is no `$HOME` to read. Home is recognised by shape:
  `/home/<user>`, `/Users/<user>`, `/var/home/<user>` and `/root`. A
  system that keeps homes anywhere else gets the plain abbreviation,
  which is right, only longer.
- **Know where you are without shell integration.** The directory comes
  from `OSC 7`. Without it `cwd` is empty and this segment is blank.
- **Disambiguate.** `~/P/H` is `~/Projects/HavenTerminal` and also
  `~/Pictures/Holiday`. It is a reminder of a path you already know, not
  a way to read one you do not.
