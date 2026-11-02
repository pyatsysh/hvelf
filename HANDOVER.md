# Handover

State of record for the next instance. hvelf has no NEXT-STEPS or STATUS file;
this is it.

## Branches

- `main` is the trunk and carries the site in `docs/`. As of 2 November 2026
  it holds everything: `keep` was merged on 4 October (ce62a15), and the
  `native/*` branches that followed it (the Linux keep chord off the X11
  PRIMARY selection, the Lab-OS vault map as the source of tiles, `--hidden`
  and `--unhide`, captures filed through Lab-OS's `keep`, GNOME chord
  handling, the board taking the keyboard on a summon) were fast-forwarded
  onto it from the Lab-OS completion worktree and published to origin.
- `keep` (ee6ef3d) is folded into main; the local branch stays as history.
- `mac/hvelf-macos` is the macOS port branch, already folded into main.
- Lab-OS carries an anonymised copy of this tree under `components/hvelf`,
  taken by its snapshot tool; its 19 October README edit (the wording of
  `keep`'s refusal) is on main too.

## The Windows build tree

The running Windows binary is **not** built from this checkout. It is built
from a hand copy of branch `keep` at `%USERPROFILE%\projects\hvelf`, whose
`src-tauri/target` holds the warm build. The copy is not a git checkout, so it
drifts silently: after changing anything here, copy the changed files across
and rebuild, or it keeps running the old code. As of 2026-09-28 the copy
matches `keep` exactly, and the running binary was built from it that day.
Main has moved past `keep` since, so refresh the copy from main before the
next Windows build.

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

The way back is **Hidden vaults** in the tray menu (`tray_menu`, `unhide`):
one item per `hide` entry, labelled as its tile was, with the parent folder
where the name is shared and `not in Obsidian` where the registry has dropped
the vault. A click takes the entry out of `hide`, in the file and in the
running config, and rebuilds the menu; `hide_vault` rebuilds it too. A hidden
vault stays off the board even while it is open, which is how an open vault
went missing from the board on 2026-09-27 with nothing to show why. Showing
open vaults whatever `hide` says is the obvious next step, not yet taken.

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

Asked on 2026-09-28: make Hidden vaults available on Windows, macOS and Linux,
commit it, and let the gate publish it.

- **Done.** The tray menu is on main and on `keep`. Main compiled with the
  Windows toolchain, no warnings, 6 of 6 tests. `keep` built as the release
  binary in the Windows tree, no warnings, 15 of 15 tests, and runs from the
  Startup shortcut. The menu itself has not yet been clicked by anyone.
- **Done, 4 October (76aa447):** `hvelf --hidden` prints the hidden entries
  as JSON and `hvelf --unhide <name or path>` puts them back, the way back on
  a Linux desktop that shows no tray; documented in the README's Linux
  paragraph.
- **Linux built here on 2 November 2026** with the host toolchain (cargo
  1.98.1): `cargo test --locked` passed 73 tests, 59 in the crate and 14 in
  the two Linux integration suites, with no warnings. The X11 test against an
  Xvfb server in RELEASING.md was not run. macOS: only CI. `build.yml` builds
  Linux, Windows and both macOS targets on every push to main, so the 2
  November push is the three platform check; look at that Actions run.
- **Not taken.** A hidden vault that is open still gets no tile (see Removing
  a tile). No version tag or draft release has been cut: the gate does not
  push tags, and a release is the owner's call per RELEASING.md.
- The vault that went missing on 2026-09-27 is still in `hide` in the Windows
  config; the tray menu puts it back.
