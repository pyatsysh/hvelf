# hvelf

<img src="src-tauri/icons/icon-512.png" width="120" align="right" alt="an arcade of vault doorways, the centre one lit" />

*Old Norse **hvelfa**: to vault, to arch.*

A hotkey-summoned tile board for your [Obsidian](https://obsidian.md) vaults.
Press a key, see your vaults as tiles, hit a number or click. The vault opens,
or its existing window comes to front, and the board gets out of your way.

Obsidian's own vault switcher is modal and slow, and the tray flyout is an
unlabelled list. If you run many vaults, switching should cost one keystroke
and one glance. That is all hvelf does.

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

- **Summoned on a hotkey**: `` Alt+` `` by default toggles the board; `Esc` or
  focus loss dismisses it. A tray icon gives a mouse path and a quit; there is
  no taskbar clutter.
- **Quick launch**: an optional second hotkey opens your most recent vault
  directly, skipping the board entirely.
- **Focus or launch**: a tile raises the vault's window if it is already open,
  and opens the vault through Obsidian's `obsidian://` URI if it is not.
- **Live state**: tiles show which vaults are open right now, read from the
  actual windows rather than from session data that may be stale.
- **Close from the board**: hovering an open tile reveals an `×` that closes
  that vault's window. It is an ordinary close, so Obsidian saves state, and
  the renderer gives back the RAM it was holding.
- **Recency order**: tiles sort most-recently-used first, so `1` is always
  the vault you were last in. While hvelf runs it tracks which vault window
  you actually have focused, which beats Obsidian's own timestamps: those
  only update when a vault is opened, not while you work in it.
- **Keyboard first**: `1`–`9` launch tiles directly, typing filters them,
  `Enter` opens the first match, `Ctrl+Q` quits.
- **Customisable tiles**: groups, ordering, hidden vaults and per-vault deep
  links, all in one JSON config. A deep link opens a vault straight onto a
  named note.
- **Light**: one small binary on Tauri 2 and WebView2, with no Electron and no
  background CPU.

Vaults come from Obsidian's own registry (`obsidian.json`), so there is nothing
to set up and the board always matches what Obsidian knows.

## Build

Requires Rust, with the MSVC toolchain on Windows. No Node, no npm.

    cd src-tauri
    cargo build --release

The binary lands in `src-tauri/target/release/hvelf.exe`. Run it once and it
sits silent until the hotkey.

## Configuration

`%APPDATA%\hvelf\config.json`, created with defaults on first run:

```json
{
  "hotkey": "alt+grave",
  "quickLaunch": "alt+1",
  "hideOnBlur": true,
  "hide": ["OldVault"],
  "groups": [
    { "name": "research", "vaults": ["melt", "cDFT", "AISTATS"] },
    { "name": "admin", "vaults": ["taxes"] }
  ],
  "deepLinks": { "melt": "index" }
}
```

- Hotkeys combine `ctrl`, `alt`, `shift`, `win` with a letter, a digit, an
  f-key, or `grave`. The name `grave` binds the physical key under Esc on any
  keyboard layout: hvelf registers keys natively by scancode rather than
  through a US-mapped name table, so it works the same on UK, DE or FR
  layouts. `quickLaunch` may be empty to disable it.
- `groups` order the board. Vaults you do not list fall into a trailing group.
- `hide` removes a vault's tile without touching Obsidian.
- `deepLinks` make a tile open a specific note, via `&file=` in the URI.
- The config is read at startup, so restart hvelf after editing it.

## Status

Windows is the supported platform today.

On macOS the app compiles and the basics work: vaults are discovered from
Obsidian's registry, tiles launch and focus vaults through the `obsidian://`
URI, and the tray menu runs. Three things do not work there yet:

- the global hotkeys (summon and quick launch): a small port, planned first;
- live open state (the green dots and the close button);
- focus-based recency, which falls back to Obsidian's last-opened timestamps.

The last two need macOS's accessibility permission to read window titles, so
they will arrive permission-gated with a graceful fallback. Until then, treat
macOS as launch-only. Linux is untested. Autostart and themes are on the
roadmap for every platform.

## Licence

MIT
