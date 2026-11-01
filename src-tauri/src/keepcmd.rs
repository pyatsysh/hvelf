// keepcmd: file a capture through Lab-OS's own `keep` command instead of
// writing the note here, so whatever `keep` checks before it writes applies
// to hvelf's captures too: the private-name guard for vaults in a synced
// folder pair (FA-013), excluded vaults, names no vault answers to. The body
// goes to `keep --stdin`; `keep` decides the note's id, path and format.
// Platform independent; the tests run stand-in commands and need Unix.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What `keep` did with a capture.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Filed: the vault `keep` reports, and the note's id and path when its
    /// receipt names them.
    Kept { vault: String, id: Option<String>, path: Option<PathBuf> },
    /// `keep` refused, with its reason. For private names the reason gives
    /// counts and line numbers, never the names.
    Refused(String),
    /// `keep` could not be run, did not answer in time, or failed without
    /// saying why in its own words.
    Failed(String),
}

/// What `keep` records as `source_note`: where the selection came from, and
/// whether the person chose the vault or parked the capture without one.
pub fn source_note(app: &str, title: &str, url: Option<&str>, parked: bool) -> String {
    let mut s = format!("hvelf capture from {}", if app.is_empty() { "an unknown program" } else { app });
    if parked {
        s.push_str(", no vault chosen");
    }
    let title = title.trim();
    if !title.is_empty() {
        s.push_str(": ");
        s.push_str(title);
    }
    if let Some(u) = url.map(str::trim).filter(|u| !u.is_empty()) {
        s.push_str(" <");
        s.push_str(u);
        s.push('>');
    }
    s
}

/// The command line: the configured command, then the capture's flags.
pub fn argv(cmd: &[String], vault: &str, note: &str) -> Vec<String> {
    let mut v = cmd.to_vec();
    v.extend(["--stdin", "--vault", vault, "--source-note", note].map(String::from));
    v
}

/// The vault, the note's id and its path from `keep`'s receipt, which reads
///
///     kept text → <vault> (<how>): <n> words, <id>
///       <path>
///       "<the first line of the passage>"
///
/// The quoted line is never returned: the text itself goes to no log.
pub fn parse_receipt(out: &str) -> (Option<String>, Option<String>, Option<PathBuf>) {
    let mut lines = out.lines();
    let (mut vault, mut id) = (None, None);
    if let Some(first) = lines.next() {
        if let Some((_, rest)) = first.split_once("\u{2192} ") {
            vault = rest.split_once(" (").map(|(v, _)| v.trim().to_string()).filter(|v| !v.is_empty());
        }
        id = first.rsplit_once(", ").map(|(_, i)| i.trim().to_string()).filter(|i| !i.is_empty() && !i.contains(' '));
    }
    let path = lines.next().map(str::trim).filter(|p| p.starts_with('/')).map(PathBuf::from);
    (vault, id, path)
}

/// Hand `body` to `keep` on stdin and wait for its answer, within `timeout`:
/// it is another program, and a capture must not hang on it.
pub fn file(cmd: &[String], vault: &str, note: &str, body: &str, timeout: Duration) -> Outcome {
    let full = argv(cmd, vault, note);
    let Some((prog, args)) = full.split_first().filter(|_| !cmd.is_empty()) else {
        return Outcome::Failed("no keep command is configured (keepCommand is empty)".into());
    };
    let mut child = match Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(format!("cannot run {prog}: {e}")),
    };
    // Written on a thread and closed when done, so a large passage cannot
    // fill the pipe while `keep` has not started reading.
    let mut stdin = child.stdin.take().unwrap();
    let bytes = body.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
    });
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut out).take(64 * 1024).read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut err).take(16 * 1024).read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                return Outcome::Failed(format!("{prog} did not answer within {} s", timeout.as_secs()));
            }
            Err(e) => return Outcome::Failed(format!("cannot wait for {prog}: {e}")),
        }
    };
    let _ = writer.join();
    let text = reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    if status.success() {
        let (v, id, path) = parse_receipt(&text);
        return Outcome::Kept { vault: v.unwrap_or_else(|| vault.to_string()), id, path };
    }
    // `keep` refuses in its own words, "keep: <why>", and exits 1; anything
    // else (a crash, a missing interpreter) is a failure, shown as its last line.
    let lines: Vec<&str> = err.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if let Some(why) = lines.iter().rev().find_map(|l| l.strip_prefix("keep: ")) {
        return Outcome::Refused(why.to_string());
    }
    match lines.last() {
        Some(last) => Outcome::Failed(format!("{prog} exited with {status}: {last}")),
        None => Outcome::Failed(format!("{prog} exited with {status}")),
    }
}

/// The program a command runs, found as the shell would: a path as given,
/// else the first executable of that name on PATH.
pub fn find_program(prog: &str, path_var: Option<&str>) -> Option<PathBuf> {
    if prog.is_empty() {
        return None;
    }
    if prog.contains('/') {
        let p = PathBuf::from(prog);
        return executable(&p).then_some(p);
    }
    path_var?
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(prog))
        .find(|p| executable(p))
}

fn executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

// ---------------------------------------------------------------- tests

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hvelf-keepcmd-{tag}-{} é", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A stand-in `keep`: a shell script in `dir`.
    fn stand_in(dir: &Path, body: &str) -> Vec<String> {
        let p = dir.join("keep stand-in");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        vec![p.display().to_string()]
    }

    const RECEIPT: &str = "kept text \u{2192} Research/AI/AL (named): 3 words, 20261011T031500-1a2b3c4d\n  /v/Research/AI/AL/keeps/2026-10/20261011T031500-1a2b3c4d.md\n  \u{201c}notes on things\u{201d}\n";

    #[test]
    fn the_command_line_is_the_configured_one_then_the_flags() {
        let cmd = vec!["python3".to_string(), "/k/bin/keep".to_string()];
        assert_eq!(
            argv(&cmd, "Research/AI/AL", "hvelf capture from x"),
            ["python3", "/k/bin/keep", "--stdin", "--vault", "Research/AI/AL", "--source-note", "hvelf capture from x"]
        );
    }

    #[test]
    fn the_source_note_says_where_from_and_whether_a_vault_was_chosen() {
        assert_eq!(source_note("gnome-terminal-server", "draft - AL - Obsidian", None, false),
                   "hvelf capture from gnome-terminal-server: draft - AL - Obsidian");
        assert_eq!(source_note("x", "  ", None, true), "hvelf capture from x, no vault chosen");
        assert_eq!(source_note("", "t", Some(" https://e.org/a "), false), "hvelf capture from an unknown program: t <https://e.org/a>");
    }

    #[test]
    fn the_receipt_gives_vault_id_and_path_never_the_quote() {
        let (v, id, path) = parse_receipt(RECEIPT);
        assert_eq!(v.as_deref(), Some("Research/AI/AL"));
        assert_eq!(id.as_deref(), Some("20261011T031500-1a2b3c4d"));
        assert_eq!(path, Some(PathBuf::from("/v/Research/AI/AL/keeps/2026-10/20261011T031500-1a2b3c4d.md")));
        assert_eq!(parse_receipt(""), (None, None, None));
        assert_eq!(parse_receipt("something else entirely"), (None, None, None));
    }

    #[test]
    fn a_kept_capture_reaches_keep_on_stdin_with_its_flags() {
        let d = scratch("kept");
        let args = d.join("args");
        let got = d.join("stdin");
        let cmd = stand_in(&d, &format!(
            "printf '%s\\n' \"$@\" > '{}'\ncat > '{}'\nprintf '%s' '{}'",
            args.display(), got.display(), RECEIPT.replace('\'', "'\\''")
        ));
        let body = "line one é \"q\"\n\tline two";
        let out = file(&cmd, "Research/AI/AL", "hvelf capture from x: t", body, Duration::from_secs(10));
        assert_eq!(out, Outcome::Kept {
            vault: "Research/AI/AL".into(),
            id: Some("20261011T031500-1a2b3c4d".into()),
            path: Some(PathBuf::from("/v/Research/AI/AL/keeps/2026-10/20261011T031500-1a2b3c4d.md")),
        });
        assert_eq!(std::fs::read_to_string(&got).unwrap(), body, "the passage verbatim on stdin");
        assert_eq!(std::fs::read_to_string(&args).unwrap(), "--stdin\n--vault\nResearch/AI/AL\n--source-note\nhvelf capture from x: t\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_large_passage_does_not_stall_on_the_pipe() {
        let d = scratch("big");
        let got = d.join("stdin");
        let cmd = stand_in(&d, &format!("sleep 0.2\ncat > '{}'\necho 'kept text \u{2192} Lab-OS (named): 1 words, id1'", got.display()));
        let body: String = (0..20_000).map(|i| format!("line {i} ünïcode\n")).collect();
        assert!(body.len() > 300_000);
        match file(&cmd, "Lab-OS", "n", &body, Duration::from_secs(20)) {
            Outcome::Kept { vault, id, .. } => assert_eq!((vault.as_str(), id.as_deref()), ("Lab-OS", Some("id1"))),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&got).unwrap(), body);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn keeps_own_refusal_is_a_refusal_anything_else_a_failure() {
        let d = scratch("refused");
        let refused = stand_in(&d, "cat > /dev/null\necho 'keep: refused: this keep would be written into /v/Lab-OS, inside the synced folder pair /v (text: 1 private identifier and 0 private paths on line 1)' >&2\nexit 1");
        match file(&refused, "Lab-OS", "n", "x", Duration::from_secs(10)) {
            Outcome::Refused(why) => assert!(why.starts_with("refused: this keep would be written into /v/Lab-OS"), "{why}"),
            other => panic!("{other:?}"),
        }
        let crashed = stand_in(&d, "echo 'Traceback (most recent call last):' >&2\necho 'ValueError: boom' >&2\nexit 3");
        match file(&crashed, "Lab-OS", "n", "x", Duration::from_secs(10)) {
            Outcome::Failed(why) => assert!(why.contains("ValueError: boom") && why.contains("exit status: 3"), "{why}"),
            other => panic!("{other:?}"),
        }
        let silent = stand_in(&d, "exit 4");
        match file(&silent, "Lab-OS", "n", "x", Duration::from_secs(10)) {
            Outcome::Failed(why) => assert!(why.ends_with("exit status: 4"), "{why}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_hung_or_unset_keep_fails_and_nothing_waits_forever() {
        let d = scratch("failed");
        let missing = vec![d.join("no such keep").display().to_string()];
        assert!(matches!(file(&missing, "Lab-OS", "n", "x", Duration::from_secs(5)), Outcome::Failed(w) if w.starts_with("cannot run")));
        let hung = stand_in(&d, "sleep 30");
        let t = Instant::now();
        match file(&hung, "Lab-OS", "n", "x", Duration::from_millis(400)) {
            Outcome::Failed(why) => assert!(why.contains("did not answer"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(t.elapsed() < Duration::from_secs(5), "the hung command was killed");
        assert!(matches!(file(&[], "Lab-OS", "n", "x", Duration::from_secs(1)), Outcome::Failed(w) if w.contains("keepCommand is empty")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn programs_are_found_as_the_shell_finds_them() {
        let d = scratch("find");
        let bin = d.join("bin dir");
        std::fs::create_dir_all(&bin).unwrap();
        stand_in(&bin, "exit 0");
        let exe = bin.join("keep stand-in");
        let plain = bin.join("not executable");
        std::fs::write(&plain, "x").unwrap();
        let path = format!("/nonexistent::{}", bin.display());
        assert_eq!(find_program("keep stand-in", Some(&path)), Some(exe.clone()));
        assert_eq!(find_program(&exe.display().to_string(), None), Some(exe.clone()));
        assert_eq!(find_program("not executable", Some(&path)), None);
        assert_eq!(find_program(&plain.display().to_string(), None), None);
        assert_eq!(find_program("keep stand-in", None), None);
        assert_eq!(find_program("", Some(&path)), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
