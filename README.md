# hvelf

*Old Norse **hvelfa**: to vault, to arch.*

A hotkey-summoned tile board for your [Obsidian](https://obsidian.md) vaults.
Press a key, see your vaults as tiles, hit a number or click — the vault opens,
or its existing window comes to front. The board gets out of your way.

Obsidian's own vault switcher is modal and slow, and the tray flyout is an
unlabelled list. If you run many vaults, switching should be one keystroke and
one glance. That is all hvelf does.

## Features

- **Summoned, not resident**: global hotkey (default `Ctrl+Alt+O`) toggles the
  board; `Esc` or focus loss dismisses it. No taskbar clutter, no dock icon.
- **Focus-or-launch**: a tile raises the vault's window if it is open, opens
  the vault if not (via Obsidian's `obsidian://` URI).
- **Live state**: tiles show which vaults are open right now, read from the
  actual windows — not from stale session data.
- **Close from the board**: hovering an open tile reveals an `×` that closes
  that vault's window (a normal close — Obsidian saves state), freeing the
  RAM its renderer held.
- **Recency order**: tiles are sorted most-recently-used first (Obsidian's own
  timestamps), so `1` is always your latest vault.
- **Keyboard-first**: `1`–`9` launch tiles directly, type to filter,
  `Enter` opens the first match, `Ctrl+Q` quits.
- **Customisable tiles**: groups, ordering, hidden vaults and per-vault deep
  links (open a vault straight onto a specific note) in one JSON config.
- **Lightweight**: a single small binary (Tauri 2 / WebView2), no Node, no
  Electron, no background CPU.

Vaults are discovered from Obsidian's own registry (`obsidian.json`) — no
setup needed; the board always matches what Obsidian knows.

## Build

Requires Rust (MSVC toolchain on Windows). No Node/npm.

    cd src-tauri
    cargo build --release

The binary lands in `src-tauri/target/release/hvelf.exe`. Run it once; it sits
silent until the hotkey.

## Configuration

`%APPDATA%\hvelf\config.json` (created with defaults on first run):

```json
{
  "hotkey": "ctrl+alt+o",
  "hideOnBlur": true,
  "hide": ["OldVault"],
  "groups": [
    { "name": "research", "vaults": ["melt", "cDFT", "AISTATS"] },
    { "name": "admin", "vaults": ["taxes"] }
  ],
  "deepLinks": { "melt": "index" }
}
```

- `groups` order the board; vaults not listed fall into a trailing group.
- `hide` removes a vault's tile without touching Obsidian.
- `deepLinks` make a tile open a specific note (`&file=` in the URI).
- Config is read at startup; restart hvelf after editing.

## Status

Windows-first (window detection and launching are Windows-native so far).
The rest is portable Rust; macOS/Linux are on the roadmap, along with a tray
icon, autostart and themes.

Known limitation: hotkey key names are mapped US-layout-style by the
underlying hotkey library, so punctuation keys land on different physical
keys on other layouts (on UK, `Backquote` is the `#~` key, not the key under
Esc). Letters and digits are safe on any layout. Scancode-based registration
(bind the physical key regardless of layout) is on the roadmap.

## Licence

MIT
