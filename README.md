# hvelf

<img src="src-tauri/icons/icon-512.png" width="120" align="right" alt="an arcade of vault doorways, the centre one lit" />

*Old Norse **hvelfa**: to vault, to arch.*

A hotkey-summoned tile board for your [Obsidian](https://obsidian.md) vaults.
Press a key, see your vaults as tiles, hit a number or click. The vault opens,
or its existing window comes to front, and the board gets out of your way.

Obsidian's own vault switcher is modal and slow, and the tray flyout is an
unlabelled list. If you run many vaults, switching should cost one keystroke
and one glance. That is all hvelf does.

The board, and the full description: **[pyatsysh.github.io/hvelf](https://pyatsysh.github.io/hvelf/)**

## The name and the mark

*Hvelfa* is the Old Norse verb for vaulting: to arch over something (and, said
of a boat, to capsize, which feels right for a tool you reach for when you are
drowning in windows). Its descendants still carry the meaning today. A
Norwegian *hvelv* is a bank vault; an Icelandic *hvelfing* is a vaulted
ceiling. One old word spans the same pun Obsidian built on: strongrooms that
keep valuables, and arches that hold up a roof.

The icon is the app doing its job: an arcade of vault doorways, the centre one
lit. Summon the board and that is what you see, a row of your vaults with the
open ones glowing.

## Features

Window observation, hotkeys and window control depend on the desktop. The
[status matrix](#status) lists the available behaviour for each platform.

- **Summoned on a hotkey**: `` Alt+` `` by default toggles the board; `Esc` or
  focus loss dismisses it. A tray icon gives a mouse path, the hidden vaults
  and a quit; there is no taskbar clutter.
- **Quick launch**: an optional second hotkey opens your most recent vault
  directly, skipping the board entirely.
- **Keep**: highlight text in any app, press `Alt+K`, and it is filed
  verbatim as a markdown note in a vault's `keeps/` folder. See below.
- **Focus or launch**: a tile raises the vault's window if it is already open,
  and opens the vault through Obsidian's `obsidian://` URI if it is not.
- **Live state**: tiles show which vaults are open right now, read from the
  actual windows rather than from session data that may be stale.
- **Close from the board**: hovering an open tile reveals an `×` that closes
  that vault's window. It is an ordinary close, so Obsidian saves state, and
  the renderer gives back the RAM it was holding.
- **Remove from the board**: on a vault that is not open, the same `×` takes
  its tile off the board, for vaults that have gone stale. The first click
  asks and the second removes. The vault's path goes into `hide` in the config
  and Obsidian is left alone. **Hidden vaults**, in the tray menu, lists
  every vault taken off the board; click one and its tile comes back.
- **Recency order**: tiles sort most-recently-used first, so `1` is always
  the vault you were last in. While hvelf runs it tracks which vault window
  you actually have focused, which beats Obsidian's own timestamps: those
  only update when a vault is opened, not while you work in it.
- **Keyboard first**: `1`–`9` launch tiles directly, typing filters them,
  `Enter` opens the first match, `Ctrl+Q` quits.
- **Customisable tiles**: groups, ordering, hidden vaults and per-vault deep
  links, all in one JSON config. A deep link opens a vault straight onto a
  named note.
- **Same-named vaults kept apart**: Obsidian names a vault after its folder,
  so a copy of `research-notes` somewhere else is a second vault with the
  same name. hvelf labels the two by their parent folder and acts on
  Obsidian's vault id, so the tile you press is the vault you get.
- **Light**: one small binary on Tauri 2 (WebView2 on Windows, WebKitGTK on
  Linux), with no Electron and no background CPU.

Vaults come from Obsidian's own registry (`obsidian.json`), so there is nothing
to set up and the board always matches what Obsidian knows.

## Build

Requires Rust, with the MSVC toolchain on Windows and the Xcode command line
tools on macOS. A binary-only build needs no Node or npm:

    cargo build --release --locked --manifest-path src-tauri/Cargo.toml

The binary lands in `src-tauri/target/release/`: `hvelf.exe` on Windows or
`hvelf` on Linux and macOS. For installers, use the pinned npm tooling in
[RELEASING.md](RELEASING.md).

On Linux (Ubuntu 24.04 names) the build needs the WebKitGTK 4.1 stack:

    sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev \
      libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev

and at run time `xdg-utils` (for `xdg-open`/`xdg-mime`), a D-Bus session and
`libayatana-appindicator3-1` for the tray.

## Configuration

`%APPDATA%\hvelf\config.json` on Windows, `${XDG_CONFIG_HOME:-~/.config}/hvelf/config.json`
on Linux and `~/.config/hvelf/config.json` on macOS, created with defaults on
first run:

```json
{
  "hotkey": "alt+grave",
  "quickLaunch": "alt+1",
  "keep": "alt+k",
  "exocortex": "D:\\Dropbox\\Vaults\\Operational\\registry\\exocortex.toml",
  "hideOnBlur": true,
  "hide": ["OldVault"],
  "groups": [
    { "name": "research", "vaults": ["orbit", "cFD", "optics"] },
    { "name": "admin", "vaults": ["taxes"] }
  ],
  "deepLinks": { "orbit": "index" }
}
```

- Hotkeys combine `ctrl`, `alt`, `shift`, `win` with a letter, a digit, an
  f-key, or `grave`. The name `grave` binds the physical key under Esc on any
  keyboard layout: hvelf registers keys natively by scancode rather than
  through a US-mapped name table, so it works the same on UK, DE or FR
  layouts. `quickLaunch` may be empty to disable it.
- `groups` order the board. Vaults you do not list fall into a trailing group.
- `hide` removes a vault's tile without touching Obsidian. The `×` on a tile
  adds the vault here for you, by its path, and Hidden vaults in the tray
  menu takes it out again. Where no tray shows (GNOME without the
  AppIndicator extension), `hvelf --hidden` lists the hidden vaults and
  `hvelf --unhide <name or path>` puts them back: a path its own entry, a
  name every entry whose vault has that name. A name that matches nothing
  is refused with exit status 1.
- `deepLinks` make a tile open a specific note, via `&file=` in the URI.
- `keep` is the capture hotkey (default `alt+k`, empty to disable) and
  `exocortex` the registry it reads, see below.
- `vaultMap` is a command printing Lab-OS's vault map (`labos-vault-map/v1`).
  With one, the board shows one tile per vault of the map and the keep chord
  files by the map's ids, see below; empty, the board is Obsidian's registry
  alone.
- `keepCommand` is the command that files a capture: Lab-OS's `keep`, handed
  the text on stdin, so the checks `keep` makes before writing apply to
  captures too, see below. Empty, hvelf writes the note itself, without those
  checks: for a machine without Lab-OS.
- On Linux the defaults differ: `hotkey` is `ctrl+alt+grave`, since GNOME
  binds Alt and the key above Tab to switching an application's windows
  (`alt+grave` still works once that binding is freed); `quickLaunch` is
  `ctrl+alt+1`, since GNOME Terminal takes `alt+1` for its first tab (set
  `alt+1` to take it from the terminal); `vaultMap` is
  `["exo", "vaults", "--json"]`; `keepCommand` is `["keep"]`; `exocortex` is
  Lab-OS's `/opt/labos/current/kernel/instance/exocortex.toml`.
- Entries in `hide`, `groups` and `deepLinks` name a vault by its name, or by
  its full path, which is the only way to name one of two vaults that share a
  name (`"hide": ["D:\\delme\\research-notes"]`).
- The config is read at startup, so restart hvelf after editing it by hand. A
  tile removed from the board, or put back from the tray, changes at once.

## Keep

The capture half of a personal exocortex: the text under your mouse goes
into a vault without a model, a dialog, or a round trip through Obsidian.

Highlight text anywhere (browser, PDF reader, terminal, Word, Obsidian) and
press the `keep` chord. On Windows hvelf reads the selection through UI Automation,
which costs no clipboard and no injected keys, and falls back to a
clipboard round trip (Ctrl+C, read, restore) for apps that do not expose
their selection. Terminals never get the fallback, because Ctrl+C with
nothing selected would interrupt whatever is running there. With nothing
selected a small toast says so and nothing else happens.

The board then opens in keep mode. A banner shows the first line and the
character count, and the vault it will go to: the one named in the window
title if any, else the one you last kept into. `Enter` or the chord again
files it there, `1`–`9` or a click files it into another vault, `Esc` parks
it in the fallback vault's `keeps/unrouted/` to decide later. A capture is
never discarded: losing the board also parks it. Focus returns to the app
you were in and a toast confirms `kept → vault`. Filed through Lab-OS's
`keep` instead, as on Linux by default, `Esc` and refusals work as described
under the vault map below.

On Linux (an X11 session) hvelf reads the PRIMARY selection, the text last
highlighted on the display, from the program holding it, as any X client
pastes it: no keys are sent and the clipboard is not touched, so terminals
work too. The keep is credited to the window in front; text highlighted in
another program is refused rather than credited to the wrong one. Browser
addresses are not captured on Linux. `hvelf --keep` does what the chord
does, for a desktop shortcut. Wayland gives no application another one's
selection, so there capture is unsupported and `hvelf --capabilities` says
so.

Where keeps go comes from Lab-OS's vault map when `vaultMap` names one (the
Linux default). Its vaults are identified by their path under the vault root
once links resolve, such as `Research/AI/ML`; a keep lands at
`<root>/<id>/keeps/<YYYY-MM>/<keep id>.md` and records `vault: <id>`. A
name the map lists for a vault (a link such as `ML`, an old name) selects
it from a window title like its own name. Bounded vaults are marked, the
fallback is the map's, and exocortex.toml is not read. A map that cannot be
read leaves nothing to file into, and the chord says why.

With a `keepCommand` (the Linux default, Lab-OS's `keep`), hvelf writes no
note itself. The choice goes to `keep --stdin --vault <id> --source-note
<program and window title>`, which applies its own checks and writes the note
in its own format (`source: selection`, `app: terminal`, where it came from in
`source_note`). `Esc` files into the fallback vault the same way, since `keep`
files nothing outside a vault. What `keep` refuses, such as a passage naming
private territory on its way into a vault in a synced folder pair, stays on
the board with `keep`'s reason, which gives counts and line numbers, never the
names: pick another permitted vault, or press `Esc`, which
now drops it. Nothing is written around a refusal, not even to
`keeps/unrouted/`, and the text stays where it was selected. A `keepCommand`
that is not on PATH makes the chord capture nothing, and
`hvelf --capabilities` reports the route as `keep-filing`.

Bounded retrieval does not by itself determine cloud placement. The configured
map and `keep` guard decide the permitted destination.

Without a map, vaults are the subfolders of a root named in a small registry
file:

```toml
[vaults]
root = "/mnt/d/Dropbox/Vaults/Operational/research"
fallback = "exo"
```

On Windows WSL paths are translated (`/mnt/d/...` to `D:\...`), and with no
registry the root defaults to `D:\Dropbox\Vaults\Operational\research` and
the fallback to `exo`. Elsewhere the registry defaults to Lab-OS's own
`/opt/labos/current/kernel/instance/exocortex.toml`, the file the `keep`
command reads, and the root is used as written. Links to folders at the root
count as vaults; `excluded` vaults are never offered. A keep is only written
into a vault folder that exists: with no usable root or fallback vault the
chord says so and takes nothing. Each note lands at `<root>/<vault>/keeps/<YYYY-MM>/<id>.md` with a
YAML frontmatter (`id`, `ts`, `source`, `vault`, `app`, `title`, optional
`url`, `sha256` of the body) and the selected text below it, in the format
Lab-OS's own `keep` writes: LF line endings, empty values left out, plain
values bare and others quoted as JSON. Files are written to a `.part` name
and renamed, so neither a sync client nor the indexer sees a half note. The last vault used is remembered in `%APPDATA%\hvelf\state.json`.

## Status

Windows was the reference platform until the author's desktop moved to Linux in
September 2026. The build workflow builds and tests all four targets on every push.
macOS was exercised from source on an Intel Mac on 2 November 2026; the October
changes have not yet run on a Windows desktop.

**macOS**: vaults are discovered from Obsidian's registry, tiles launch
through the `obsidian://` URI, the tray runs, and the summon and quick-launch
hotkeys are registered through Carbon (`hotkey_macos.rs`), with the ISO and
ANSI grave keys both bound. Open state comes from Obsidian's own per-vault
flags, so the board labels it as reported rather than observed. hvelf does not
raise or close another application's window on macOS, and recency falls back
to Obsidian's last-opened stamps.

**Linux**: what works depends on the session, and `hvelf --capabilities`
prints the matrix for the one you are in.

| | X11 with an EWMH window manager | Wayland (GNOME, KDE) | no display |
|---|---|---|---|
| vaults from `obsidian.json` (XDG, `~/.config`, Flatpak, Snap) | yes | yes | yes |
| open through `obsidian://` (`xdg-open`, else `gio open`) | yes | yes | handler permitting |
| open dots | observed from `_NET_CLIENT_LIST` | reported from Obsidian's flags | reported |
| raise the vault's window | `_NET_ACTIVE_WINDOW`, for an open vault at once and for one just opened through the URI as soon as Obsidian shows it (KWin refuses Obsidian's own request) | asked of Obsidian through the URI; the compositor may only flag it | no |
| close from the board | polite `_NET_CLOSE_WINDOW` | unsupported: the cross only removes the tile | no |
| focus recency | active window every 2 s | Obsidian's stamps only | Obsidian's stamps only |
| global hotkey | grabbed by keycode on the root window | bind `hvelf --toggle` as a desktop shortcut | none |

A link to an Obsidian that is already running goes through Obsidian's own
command socket, `$XDG_RUNTIME_DIR/.obsidian-cli.sock` since 1.11.7. An
Obsidian that quits removes that socket, even one a newer Obsidian is
listening on, after which every link is dropped silently. hvelf checks for the
socket before sending a link and tells you to restart Obsidian instead, and
if a link goes out and no window appears within ten seconds it says that too.

hvelf currently uses EWMH for window observation and control, which applies to
X11. It has no Wayland compositor integration for those operations. Global
shortcuts are a separate feature: the desktop portal provides an API on
supported desktops, but this build only detects that API and does not bind
through it. Closing on X11 is a request handled by Obsidian, never a process
kill, and hvelf does not edit vault files.

With Lab-OS's vault map the board shows one tile per vault: a link and the
folder it points at, which Obsidian lists as two vaults, are one tile, the
one Obsidian used last. A vault in a bounded partition (Admin, Career,
Personal) is marked. A vault of the map that holds an `.obsidian` folder but
that Obsidian has never opened is shown dimmed and says how to open it, and
an entry of Obsidian's registry whose folder is gone is marked missing
instead of being handed to Obsidian.

A running board takes commands from a second invocation over D-Bus:
`hvelf --toggle`, `--show`, `--hide`, `--quick-launch`, `--keep`,
`--unhide <name or path>` and `--quit`; `hvelf --hidden` prints the hidden
vaults without one. Bind
`hvelf --toggle` to a key in the desktop's own settings (GNOME: Settings,
Keyboard, View and Customise Shortcuts, Custom Shortcuts; KDE: System
Settings, Shortcuts, Add Command). GNOME binds Alt plus the key above Tab to
switching between an application's windows by default, which is the same
physical key as `alt+grave`: pick another chord, or free that one first. On
X11 hvelf reports such a clash as a limit under the board instead of failing
silently. The xdg-desktop-portal GlobalShortcuts route is detected but not yet
used. Autostart is not installed; add a desktop entry to
`~/.config/autostart` yourself if you want it.

## Builds and releases

See [RELEASING.md](RELEASING.md) for Linux prerequisites, locked builds,
installer artifacts and draft releases. The first tagged release is
[v0.1.0](https://github.com/pyatsysh/hvelf/releases/tag/v0.1.0), a prerelease
with installers for all four targets. Linux has been in daily use on a KDE X11
desktop since October 2026; Wayland window control remains unsupported.

## Licence

MIT
