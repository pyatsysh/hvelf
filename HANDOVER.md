# Handover

State of record for the next instance. hvelf has no NEXT-STEPS or STATUS file;
this is it.

## Branches

- `main` is the trunk and carries the site in `docs/`.
- `keep` adds the capture chord (`capture.rs`, `keep.rs`, `toast.html`) and is
  one commit ahead of its fork point, two behind main (the macOS hotkeys and
  the site catch-up). **The merge is still owed.**
- `mac/hvelf-macos` is the macOS port branch, already folded into main.

## The Windows build tree

The running Windows binary is **not** built from this checkout. It is built
from a hand copy of branch `keep` at `%USERPROFILE%\projects\hvelf`, whose
`src-tauri/target` holds the warm build. The copy is not a git checkout, so it
drifts silently: after changing anything here, copy the changed files across
and rebuild, or it keeps running the old code. As of 2026-09-18 the copy
matches `keep` exactly.

The running binary holds `target\release\hvelf.exe` open, and `deps\hvelf.exe`
is a hard link to the same file, so the linker cannot write either. Rename both
aside before `cargo build --release` (`hvelf-prev-running.exe` is the last
binary, kept as the way back), then stop the tray app and start it again from
its Startup shortcut.

## Removing a tile

A tile's `×` closes the window of an open vault and, on a shut one, takes the
tile off the board after a second click. The removal is `hide_vault`: the
vault's path is appended to `hide` in `config.json`, edited as plain JSON so
the user's key order and unknown keys survive, and to the running config,
which the hotkey handlers share, so the vault leaves quick launch at once.
The change is on both `main` and `keep`. The macOS side of it
(`hotkey_macos.rs` taking the shared config) has never been compiled: this box
has no macOS toolchain.

## Vault identity

Obsidian names a vault after its folder, so two registered vaults can share a
name; only the 16-hex key in `obsidian.json` is unique. Tiles therefore travel
by that id, and a clashing name shows its parent folder under it. Obsidian's
URI resolver takes an id or a name, checks the id first, and otherwise matches
basenames case-insensitively in registry order, which is why the name alone
cannot be trusted to open the vault you meant.

Windows tells same-named vaults apart with Obsidian's own per-vault `open`
flag, since a window title carries the name and not the id. When two namesakes
are both open, nothing distinguishes their windows, and hvelf raises and closes
neither rather than the wrong one.

## Open

- Nothing in flight. The board, the hotkeys, the tray and the keep chord all
  work on Windows; macOS lacks window raising and focus-based recency.
