//! The keep capture on a real X server, end to end through the built binary.
//! Needs a display and a private session bus:
//!
//!     HVELF_X11_TEST=1 xvfb-run -n 171 dbus-run-session -- cargo test --test linux_keep
//!
//! The test plays the window manager (it publishes `_NET_ACTIVE_WINDOW`) and
//! the application in front, whose PRIMARY selection hvelf reads; it serves
//! that selection itself, INCR included for a large one. `hvelf --keep` stands
//! in for the chord, so no key is injected. Fixtures only: no real vault,
//! registry or desktop is read or written.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property,
    SelectionNotifyEvent, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

fn enabled() -> bool {
    if std::env::var("HVELF_X11_TEST").is_err() || std::env::var("DISPLAY").is_err() {
        eprintln!("skipped: run under xvfb-run and dbus-run-session with HVELF_X11_TEST=1");
        return false;
    }
    // This test plays the window manager and owns the selection; on a display
    // where a real one runs it would act on somebody's desktop. Refuse.
    let (conn, screen) = x11rb::connect(None).expect("the test display");
    let root = conn.setup().roots[screen].root;
    let check = conn.intern_atom(false, b"_NET_SUPPORTING_WM_CHECK").unwrap().reply().unwrap().atom;
    let wm = conn.get_property(false, root, check, AtomEnum::WINDOW, 0, 1).unwrap().reply().unwrap();
    assert!(wm.value.is_empty(), "a window manager runs on this display: run this test on a private Xvfb only");
    true
}

fn root(tag: &str) -> PathBuf {
    let base = std::env::var("HVELF_FIXTURE_ROOT").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    let d = base.join(format!("hvelf keep {tag} ü \"q\" {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ------------------------------------------------------- the app in front

/// A top-level window, made the active one, that owns PRIMARY and serves it
/// on a thread until dropped. Over 64 KiB it answers INCR, in chunks.
struct Front {
    stop: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
}

const CHUNK: usize = 64 * 1024;

impl Front {
    fn new(title: &str, text: Option<&str>) -> Front {
        let (conn, screen) = x11rb::connect(None).unwrap();
        let root = conn.setup().roots[screen].root;
        let atom = |n: &[u8]| conn.intern_atom(false, n).unwrap().reply().unwrap().atom;
        let (active, name, utf8, targets, incr) = (
            atom(b"_NET_ACTIVE_WINDOW"),
            atom(b"_NET_WM_NAME"),
            atom(b"UTF8_STRING"),
            atom(b"TARGETS"),
            atom(b"INCR"),
        );
        let win = conn.generate_id().unwrap();
        conn.create_window(0, win, root, 0, 0, 200, 100, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new())
            .unwrap();
        conn.change_property8(PropMode::REPLACE, win, name, utf8, title.as_bytes()).unwrap();
        conn.change_property32(PropMode::REPLACE, root, active, AtomEnum::WINDOW, &[win]).unwrap();
        match text {
            Some(_) => conn.set_selection_owner(win, AtomEnum::PRIMARY.into(), x11rb::CURRENT_TIME).unwrap(),
            None => conn.set_selection_owner(x11rb::NONE, AtomEnum::PRIMARY.into(), x11rb::CURRENT_TIME).unwrap(),
        };
        conn.flush().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let text = text.unwrap_or("").as_bytes().to_vec();
        let serving = std::thread::spawn(move || {
            // An INCR transfer in flight: requestor, property, bytes sent.
            let mut incr_out: Option<(u32, u32, usize)> = None;
            while !flag.load(Ordering::SeqCst) {
                let Some(ev) = conn.poll_for_event().unwrap() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                match ev {
                    Event::SelectionRequest(req) => {
                        let mut property = req.property;
                        if req.target == targets {
                            conn.change_property32(PropMode::REPLACE, req.requestor, req.property, AtomEnum::ATOM, &[targets, utf8])
                                .unwrap();
                        } else if req.target == utf8 && text.len() > CHUNK {
                            conn.change_window_attributes(
                                req.requestor,
                                &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
                            )
                            .unwrap();
                            conn.change_property32(PropMode::REPLACE, req.requestor, req.property, incr, &[text.len() as u32])
                                .unwrap();
                            incr_out = Some((req.requestor, req.property, 0));
                        } else if req.target == utf8 {
                            conn.change_property8(PropMode::REPLACE, req.requestor, req.property, utf8, &text).unwrap();
                        } else {
                            property = x11rb::NONE;
                        }
                        let notify = SelectionNotifyEvent {
                            response_type: SELECTION_NOTIFY_EVENT,
                            sequence: 0,
                            time: req.time,
                            requestor: req.requestor,
                            selection: req.selection,
                            target: req.target,
                            property,
                        };
                        conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify).unwrap();
                        conn.flush().unwrap();
                    }
                    // The requestor took the last chunk: send the next, and an
                    // empty one after the end.
                    Event::PropertyNotify(p) if p.state == Property::DELETE => {
                        if let Some((w, prop, sent)) = incr_out {
                            if p.window == w && p.atom == prop {
                                let end = (sent + CHUNK).min(text.len());
                                conn.change_property8(PropMode::REPLACE, w, prop, utf8, &text[sent..end]).unwrap();
                                conn.flush().unwrap();
                                incr_out = if sent == text.len() { None } else { Some((w, prop, end)) };
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
        Front { stop, serving: Some(serving) }
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.serving.take() {
            let _ = t.join();
        }
    }
}

// --------------------------------------------------------------- hvelf

struct Fixture {
    home: PathBuf,
    cfg: PathBuf,
    vaults: PathBuf,
    log: PathBuf,
}

/// A fake `exo` printing a labos-vault-map/v1 map of `root`: Research/AI/AL
/// with its link AL, the fallback Lab-OS and a bounded Admin.
fn fake_map(home: &Path, root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    for d in ["Research/AI/AL", "Lab-OS", "Admin"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::os::unix::fs::symlink(root.join("Research/AI/AL"), root.join("AL")).unwrap();
    let r = root.display().to_string();
    let map = serde_json::json!({
        "schema": "labos-vault-map/v1", "root": r, "fallback": "Lab-OS",
        "vaults": [
            {"id": "Admin", "path": format!("{r}/Admin"), "partition": "Admin", "bounded": true, "aliases": ["admin"]},
            {"id": "Lab-OS", "path": format!("{r}/Lab-OS"), "partition": "global", "aliases": ["labos"]},
            {"id": "Research/AI/AL", "path": format!("{r}/Research/AI/AL"), "partition": "global", "aliases": ["AL"]},
        ]
    });
    let json = home.join("map.json");
    std::fs::write(&json, map.to_string()).unwrap();
    let exo = home.join("fake exo");
    std::fs::write(&exo, format!("#!/bin/sh\n[ \"$1 $2\" = \"vaults --json\" ] || exit 2\n/bin/cat '{}'\n", json.display())).unwrap();
    std::fs::set_permissions(&exo, std::fs::Permissions::from_mode(0o755)).unwrap();
    exo
}

fn fixture(tag: &str, registry: Option<&str>) -> Fixture {
    fixture_with(tag, registry, false)
}

/// With `mapped`, the vaults come from a fake `exo vaults --json`; without,
/// from the registry file, and no map command is configured at all, so the
/// real `exo` is never run.
fn fixture_with(tag: &str, registry: Option<&str>, mapped: bool) -> Fixture {
    let home = root(tag);
    let cfg = home.join("cfg é");
    let vaults = home.join("Vaults root");
    for v in ["AL", "exo", "archive"] {
        std::fs::create_dir_all(vaults.join(v)).unwrap();
    }
    let toml = home.join("exocortex.toml");
    let reg = registry.map(String::from).unwrap_or_else(|| {
        format!("[vaults]\nroot = {:?}\nfallback = \"exo\"\nbounded = []\nexcluded = [\"archive\"]\n", vaults.display().to_string())
    });
    std::fs::write(&toml, reg).unwrap();
    std::fs::create_dir_all(cfg.join("hvelf")).unwrap();
    let map: Vec<String> = if mapped {
        vec![fake_map(&home, &vaults.join("mapped")).display().to_string(), "vaults".into(), "--json".into()]
    } else {
        Vec::new()
    };
    // The mapped board starts with Lab-OS hidden, for the way back.
    let hide: Vec<String> = if mapped { vec![vaults.join("mapped/Lab-OS").display().to_string()] } else { Vec::new() };
    let conf = serde_json::json!({
        "hotkey": "", "quickLaunch": "", "keep": "",
        "exocortex": toml.display().to_string(), "vaultMap": map, "hideOnBlur": false, "hide": hide,
    });
    std::fs::write(cfg.join("hvelf/config.json"), conf.to_string()).unwrap();
    let log = home.join("hvelf.log");
    Fixture { home, cfg, vaults, log }
}

fn command(f: &Fixture) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hvelf"));
    c.env_clear();
    for k in ["DISPLAY", "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS", "PATH", "LD_LIBRARY_PATH"] {
        if let Ok(v) = std::env::var(k) {
            c.env(k, v);
        }
    }
    c.env("HOME", &f.home)
        .env("XDG_CONFIG_HOME", &f.cfg)
        .env("XDG_SESSION_TYPE", "x11")
        .env("GDK_BACKEND", "x11")
        .env("WEBKIT_DISABLE_DMABUF_RENDERER", "1")
        .env("LIBGL_ALWAYS_SOFTWARE", "1");
    c
}

fn run(f: &Fixture, args: &[&str]) -> Output {
    command(f).args(args).output().unwrap()
}

fn bus_name_owned() -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else { return false };
    let Ok(proxy) = zbus::blocking::fdo::DBusProxy::new(&conn) else { return false };
    let name = zbus::names::BusName::try_from("app.hvelf.SingleInstance").unwrap();
    proxy.name_has_owner(name).unwrap_or(false)
}

fn wait_for(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The running board: started, reachable over the bus, and quit on drop.
struct Board<'a> {
    f: &'a Fixture,
    child: Child,
}

impl<'a> Board<'a> {
    fn start(f: &'a Fixture) -> Board<'a> {
        assert!(!bus_name_owned(), "another hvelf owns the test bus");
        let log = std::fs::File::create(&f.log).unwrap();
        let child = command(f).stdout(Stdio::null()).stderr(log).spawn().unwrap();
        wait_for("the board on the session bus", 30, bus_name_owned);
        Board { f, child }
    }

    fn keep(&self) {
        let out = run(self.f, &["--keep"]);
        assert!(out.status.success(), "--keep: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.f.log).unwrap_or_default()
    }

    fn wait_log(&self, needle: &str) {
        wait_for(&format!("{needle:?} in the board's log"), 10, || self.log().contains(needle));
    }
}

impl Drop for Board<'_> {
    fn drop(&mut self) {
        let _ = run(self.f, &["--quit"]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Keeps under `dir`, links not followed, so a vault reached through a link
/// is counted once.
fn keeps_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                out.extend(keeps_in(&p));
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn split(note: &str) -> (Vec<(String, String)>, String) {
    let rest = note.strip_prefix("---\n").expect("frontmatter opens");
    let end = rest.find("\n---\n").expect("frontmatter closes");
    let meta = rest[..end]
        .lines()
        .map(|l| {
            let (k, v) = l.split_once(": ").expect("key: value");
            (k.to_string(), v.to_string())
        })
        .collect();
    (meta, rest[end + 5..].to_string())
}

fn this_exe() -> String {
    std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned()
}

// ----------------------------------------------------------------- tests

fn wait_files(dir: &Path, n: usize) -> Vec<PathBuf> {
    wait_for(&format!("{n} keep(s) under {}", dir.display()), 10, || keeps_in(dir).len() >= n);
    keeps_in(dir)
}

fn check_note(path: &Path, vault: &str, title: &str, text: &str) {
    let note = std::fs::read_to_string(path).unwrap();
    let (meta, body) = split(&note);
    let get = |k: &str| meta.iter().find(|(m, _)| m == k).map(|(_, v)| v.clone());
    let keys: Vec<&str> = meta.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["id", "ts", "source", "vault", "app", "title", "sha256"], "no url on Linux");
    let id = get("id").unwrap();
    assert_eq!(path.file_name().unwrap().to_string_lossy(), format!("{id}.md"));
    assert!(id.len() == 24 && id.as_bytes()[8] == b'T' && id.as_bytes()[15] == b'-', "{id}");
    assert_eq!(get("source").as_deref(), Some("selection"));
    assert_eq!(get("vault").as_deref(), Some(vault));
    assert_eq!(get("app"), Some(this_exe()), "credited to the program in front");
    assert_eq!(get("title"), Some(serde_json::to_string(title).unwrap()));
    let want = if text.ends_with('\n') { text.to_string() } else { format!("{text}\n") };
    assert_eq!(body, want, "body verbatim");
    let sha: String = Sha256::digest(want.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(get("sha256"), Some(sha));
    let month = &get("ts").unwrap()[..7];
    assert_eq!(path.parent().unwrap().file_name().unwrap().to_string_lossy(), month);
}

/// One board, one session bus: the scenarios run in order. Highlighted text
/// in the window in front goes to the vault its title names, verbatim, and
/// the chord a second time files it; a large selection comes by INCR, whole;
/// text highlighted in another program, or nothing at all, is refused and
/// nothing is written. Then a registry naming no real root refuses before
/// reading anything and creates nothing.
#[test]
fn capture_files_refuses_and_never_invents_a_vault() {
    if !enabled() {
        return;
    }
    let f = fixture("file", None);
    let al = f.vaults.join("AL").join("keeps");
    {
        let board = Board::start(&f);
        let caps = run(&f, &["--capabilities"]);
        let caps: Vec<serde_json::Value> = serde_json::from_slice(&caps.stdout).unwrap();
        let kc = caps.iter().find(|c| c["capability"] == "keep-capture").expect("keep-capture record");
        assert_eq!(kc["state"], "available", "{kc}");

        // The title names AL; archive is excluded and never offered.
        let title = "draft \"q\" - AL - Obsidian v1.6.7";
        let text = "kept on Linux é \"q\" <b>\n  second line\ttab";
        let front = Front::new(title, Some(text));
        board.keep();
        board.wait_log("chars from");
        let log = board.log();
        assert!(log.contains(&format!("{} chars from {}", text.chars().count(), this_exe())), "{log}");
        assert!(log.contains("AL is lit; vaults AL, exo\n"), "{log}");
        board.keep();
        let got = wait_files(&al, 1);
        check_note(&got[0], "AL", title, text);
        board.wait_log("kept \u{2192} AL");
        drop(front);

        // Over 64 KiB: INCR. No vault in the title, so the last one used.
        let big: String = (0..12_000).map(|i| format!("line {i} ünïcode\n")).collect();
        assert!(big.len() > 3 * CHUNK);
        let front = Front::new("a long page", Some(&big));
        board.keep();
        wait_for("the second capture", 10, || board.log().matches("chars from").count() >= 2);
        board.keep();
        let got = wait_files(&al, 2);
        let newest = got.iter().find(|p| std::fs::read_to_string(p).unwrap().contains("a long page")).unwrap();
        check_note(newest, "AL", "a long page", &big);
        drop(front);

        // Highlighted in another program than the one in front: refused.
        let front = Front::new("draft - AL - Obsidian v1.6.7", None);
        match Command::new("xclip").args(["-selection", "primary", "-i"]).stdin(Stdio::piped()).spawn() {
            Ok(mut xclip) => {
                use std::io::Write;
                xclip.stdin.take().unwrap().write_all(b"someone else's text").unwrap();
                std::thread::sleep(Duration::from_millis(300));
                board.keep();
                board.wait_log("the highlighted text is in xclip");
                let _ = xclip.kill();
                let _ = xclip.wait();
            }
            Err(e) => eprintln!("other-owner case skipped: xclip not runnable ({e})"),
        }
        drop(front);

        // Nothing highlighted anywhere: refused.
        let front = Front::new("draft - AL - Obsidian v1.6.7", None);
        board.keep();
        board.wait_log("keep: nothing selected");
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(keeps_in(&f.vaults).len(), 2, "refusals write nothing");
        drop(front);
    }

    // A registry naming a root that is not there: refused before any read,
    // and the root is not created.
    let missing = f.home.join("no such root");
    let g = fixture("broken", Some(&format!("[vaults]\nroot = {:?}\nfallback = \"exo\"\n", missing.display().to_string())));
    let board = Board::start(&g);
    let caps: Vec<serde_json::Value> = serde_json::from_slice(&run(&g, &["--capabilities"]).stdout).unwrap();
    let kc = caps.iter().find(|c| c["capability"] == "keep-capture").unwrap();
    assert_eq!(kc["state"], "unavailable", "{kc}");
    let front = Front::new("draft - AL - Obsidian v1.6.7", Some("text"));
    board.keep();
    board.wait_log("does not exist");
    assert!(!missing.exists(), "a capture never creates a vault root");
    drop(front);
    drop(board);

    // Lab-OS's vault map: the title's AL is an alias of Research/AI/AL, the
    // keep lands there and records the canonical id; the bounded Admin is
    // offered and marked, and exocortex.toml is not read at all.
    let h = fixture_with("mapped", Some("this is not toml ["), true);
    let mapped = h.vaults.join("mapped");
    let board = Board::start(&h);
    let caps: Vec<serde_json::Value> = serde_json::from_slice(&run(&h, &["--capabilities"]).stdout).unwrap();
    let get = |name: &str| caps.iter().find(|c| c["capability"] == name).unwrap().clone();
    assert_eq!(get("vault-map")["state"], "available", "{}", get("vault-map"));
    assert!(get("vault-map")["reason"].as_str().unwrap().starts_with("3 vaults under"));
    assert_eq!(get("keep-capture")["state"], "available", "{}", get("keep-capture"));
    let title = "draft - AL - Obsidian v1.6.7";
    let front = Front::new(title, Some("a mapped keep"));
    board.keep();
    board.wait_log("Research/AI/AL is lit; vaults Research/AI/AL, Admin, Lab-OS\n");
    board.keep();
    let got = wait_files(&mapped.join("Research/AI/AL/keeps"), 1);
    check_note(&got[0], "Research/AI/AL", title, "a mapped keep");
    assert_eq!(keeps_in(&mapped).len(), 1, "one keep, reached through the link only once");
    drop(front);

    // The way back without a tray: --unhide reaches the running board, which
    // takes the entry out of config.json and its own copy.
    let hidden = |f: &Fixture| -> Vec<serde_json::Value> { serde_json::from_slice(&run(f, &["--hidden"]).stdout).unwrap() };
    assert_eq!(hidden(&h).len(), 1);
    assert_eq!(hidden(&h)[0]["label"], "Lab-OS (not in Obsidian)");
    let out = run(&h, &["--unhide", "Lab-OS"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    wait_for("the entry to leave config.json", 10, || hidden(&h).is_empty());
}
