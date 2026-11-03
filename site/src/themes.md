# Themes and configuration

The daemon owns the configuration, so one session looks the same through a
window and through a terminal, and an edit reaches every attached client
without reattaching — the file is re-read when its timestamp moves.

That is the part worth noticing: the client on the other end of an SSH
connection repaints with your new theme without being restarted.

## config.toml

`$XDG_CONFIG_HOME/haven/config.toml`, or `~/.config/haven/config.toml`:

```toml
theme = "nord"    # haven, nord, gruvbox, daylight, slate, ansi, plain
```

## A theme of your own

As a table in the config, or as `~/.config/haven/themes/<name>.toml` named the
same way:

```toml
[theme]
base = "gruvbox"      # start here and change only what follows
name = "mine"
bar_session = { fg = "#1d2021", bg = "#83a598", bold = true }
selection   = { fg = 0, bg = 12 }     # palette indices work too
glyphs      = { here = "*" }
```

Every colour and drawing character the interface uses comes from the theme;
nothing below it hardcodes one. Colours are `"#rrggbb"`, a palette index (`12`
or `"12"`), or `"default"` to leave it to the terminal.

`plain` uses no colour and no box drawing, for a terminal that cannot manage
either — so a pane rule becomes `+` rather than a hole.

A broken theme is reported on the daemon's stderr and the interface keeps the
default rather than refusing to paint.

## Six colours is all it takes

Each built-in theme is one line of palette and nothing else, so adding another
is a matter of taste rather than of plumbing. The default:

| Swatch | Value | Used for |
|---|---|---|
| `ink` | `#1b1e28` | text on the accent, the deepest background |
| `surface` | `#242836` | the bar, panels |
| `raised` | `#2e3344` | the command line |
| `text` | `#c8d0e0` | panel text, the active tab |
| `faint` | `#7a849c` | the bar's own text, pane rules |
| `accent` | `#6c9cf0` | the session name, panel borders, selection |
| `warm` | `#f0b46c` | alerts |

## A theme is data

Plugins — code that computes what a status line says — are a separate thing and
are [not built yet](./status.md). When they are, a plugin will supply one of
these same tables. The theme did not have to wait for the plugin host precisely
because it is data rather than code.

Two types carry it, not one: the wire `Theme` is a fixed struct of plain
`Color`s, and the daemon's `ThemeFile` is all-optional with string colours.
They were briefly the same type, which does not work — postcard is not
self-describing and cannot deserialize the untagged colour that makes a config
file pleasant to write.
