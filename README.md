# hvelf

*Old Norse **hvelfa**: to vault, to arch.*

A hotkey-summoned tile board for your [Obsidian](https://obsidian.md) vaults.
Press a key, see your vaults as tiles, hit a number or click. The vault opens,
or its existing window comes to front, and the board gets out of your way.

Obsidian's own vault switcher is modal and slow, and the tray flyout is an
unlabelled list. If you run many vaults, switching should cost one keystroke
and one glance. That is all hvelf does.

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
- **Recency order**: tiles sort most-recently-used first, on Obsidian's own
  timestamps, so `1` is always the vault you were last in.
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

Windows first: window detection, hotkey registration and launching are
Windows-native so far. The rest is portable Rust, and macOS and Linux are on
the roadmap along with autostart and themes.

## Licence

MIT
