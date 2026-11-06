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
(`hotkey_macos.rs` taking the shared config) was built and used on a Mac on
2 November 2026; see Open.

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
  Xvfb server in RELEASING.md was not run. `build.yml` builds
  Linux, Windows and both macOS targets on every push to main. The run on
  the 2 November push (41480559678): Linux and both macOS builds passed;
  Windows failed three `vaultmap` fixtures that used the Unix root `/v`,
  which is not absolute there, so the draft release was skipped. With the
  fixtures on a Windows root, run 41541229968 on a366040 passed all four.
- **macOS built and used on 2 November 2026** on an Intel MacBook Pro,
  macOS 26.6.2, Homebrew rustc 1.98.0: `cargo build --release --locked` with
  no warnings, and `cargo test --release --locked` passed 38 tests in the
  crate, before and after the Windows root fix. The release binary runs from
  the `app.hvelf` launch agent; both hotkeys bind and the log reports the keep
  chord unbound, as designed. The owner clicked through it: the `×` on a shut
  tile asks, then removes it, and Hidden vaults in the tray menu puts it back.
  `--list-vaults` and `--hidden` answer from the command line.
- **Not taken.** A hidden vault that is open still gets no tile (see Removing
  a tile). v0.1.0 was tagged on a366040 and published as a
  prerelease on 2 November 2026 on the owner's word, with the four installers,
  SHA256SUMS and receipts; the tag went up with the gate override, since the
  gate does not push tags.
- The vault that went missing on 2026-09-27 is still in `hide` in the Windows
  config; the tray menu puts it back.
- **4 November 2026, KDE under X11:** a vault picked on the board stayed
  behind the window the owner was in whenever hvelf went through
  `obsidian://`, which it does for a vault with no window and did for one
  whose window was titled with the vault alone, before a note is open (a
  two-part title was not read). KWin at its default focus-stealing level
  refuses Obsidian's own request, timed from Obsidian's last keystroke, and
  leaves the window where it is; the pager-style `_NET_ACTIVE_WINDOW` hvelf
  sends for a window it already knows is honoured, minimised ones included.
  Both were checked on the desktop with a ClientMessage from a script and
  `xdg-open` of the link, reading `_NET_ACTIVE_WINDOW` back. hvelf now reads
  the two-part title and, after the link, polls the window list for up to
  ten seconds and raises the window Obsidian made, or the only one of that
  name. The hub was still running the 12 October 0.1.0 package; the fix needs
  a rebuilt one.
- **5 November 2026, the links themselves.** With that build installed, a
  vault with no window still did not open: driving the board by keyboard for
  Lab-OS produced no window at all, and `xdg-open` of a deep link into the
  open AL vault changed nothing either. `obsidian.log` shows why. Obsidian
  takes each link in a second process that hands it to the running one, and
  since the 1.15.0 update was downloaded on 2 November at 16:06, behind an
  Obsidian started at 13:06 that day on 1.14.5, every link launch logs
  `Loaded updated app obsidian-1.15.0.asar` and nothing follows: the 4
  November test that was read as KWin refusing the focus was one of these.
  Links come back when Obsidian is restarted on the new build. The board now
  says so when its ten-second wait ends with no window of that name.
- **5 November 2026, later: the socket, not the build.** After the restart
  the first link went through and the next ones did not. Obsidian's launcher
  (read from obsidian-1.15.0.asar) sends a second process's command line,
  links included, to `$XDG_RUNTIME_DIR/.obsidian-cli.sock` and exits with
  "Unable to connect to main process" when it cannot connect, with no other
  route; the running Obsidian removes that path with `unlinkSync` on
  `will-quit`, unconditionally. An Obsidian that quit at 10:23:21, five
  seconds after the new one had started listening, took the new one's socket
  path with it: `ss` still showed the listener, `ls` showed no file, and the
  earlier failures since 2 November had the same shape. Remedy: quit
  Obsidian, wait until no obsidian process is left, then start it. The board
  now checks for the socket before sending a link and says this at once.
- **5 November 2026, the raise itself.** With links flowing again, an open
  vault picked on the board still stayed behind: the board logged "raising
  its window" and the focus went straight back to the vault the owner came
  from. The board's hide (GTK's connection) and the raise (hvelf's own) reach
  KWin in no fixed order; when the raise lands first, KWin sees its active
  window unmap and hands the focus to the previously active window, after
  the vault's. A pager-style request from a script, with no board to hide,
  raised the same window every time. The raise now waits until the keyboard
  window manager no longer lists the board and the active window is not
  hvelf's (the keyboard focus alone leaves earlier, when the X server reverts
  it on the unmap, and a raise sent then was still undone), plus 60 ms, before
  it is sent, on a thread of its own: `launch` runs on the main thread, the
  one that processes the hide, and a first attempt that waited there held the
  board up for the half second and then lost the same race the other way.
  This was in the 12 October build too.
- **5 November 2026, the request that never arrived.** Even sent after
  the hide, the board's raise did nothing, while the same request from a
  script landed at any delay, and the board's landed every time under
  strace. The difference was the connection: `focus_window` opened one,
  wrote and flushed the request and dropped it the next instant, and that
  request never reached KWin. Holding the connection open to read
  `_NET_ACTIVE_WINDOW` back makes it land on the first request, three of
  three; `raise_until_in_front` does that for every raise, with a repeat
  as a backstop. The keep flow's focus return went through the same
  function and will have been lost the same way.
  Checked with the packaged binary: an open vault behind another came up
  three times of three, a vault with no window opened and came up within a
  second, each on the first request. Seen once in a full `cargo test` run
  and not since: `missing_handler_and_opener_are_reported_not_ignored`
  failed, then passed alone and in every later run; worth a look if it
  recurs.
