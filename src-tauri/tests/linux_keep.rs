//! The keep capture on a real X server, end to end through the built binary.
//! Needs a display and a private session bus:
//!
//!     HVELF_X11_TEST=1 xvfb-run -n 171 dbus-run-session -- cargo test --test linux_keep
//!
//! With HVELF_LABOS_KERNEL naming a Lab-OS kernel tree (bin/keep and
//! runtime/, 0.2.0-rc.7 or later), captures are also filed through that real
//! `keep`, whose private-name guard for synced vaults then decides.
//!
//! The test plays the window manager (it publishes `_NET_ACTIVE_WINDOW`) and
//! the application in front, whose PRIMARY selection hvelf reads; it serves
//! that selection itself, INCR included for a large one. `hvelf --keep` stands
//! in for the chord, so no key is injected. Fixtures only: no real vault,
//! registry or desktop is read or written, and hvelf runs with the system's
//! PATH alone, so no installed `keep` or `exo` can be reached.
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

/// The tests share one display and one session bus, where only one board may
/// run: they take turns, whatever the test harness's thread count.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

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
    /// More environment for hvelf, and so for the keep command it runs.
    env: Vec<(String, String)>,
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
/// real `exo` is never run. hvelf writes these keeps itself (an empty
/// keepCommand), so the real `keep` is never run either.
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
        "exocortex": toml.display().to_string(), "vaultMap": map, "keepCommand": [], "hideOnBlur": false, "hide": hide,
    });
    std::fs::write(cfg.join("hvelf/config.json"), conf.to_string()).unwrap();
    let log = home.join("hvelf.log");
    Fixture { home, cfg, vaults, log, env: Vec::new() }
}

/// The binary under test: this build's, or HVELF_BIN, so the same checks can
/// run against a packaged one.
fn hvelf_bin() -> PathBuf {
    std::env::var("HVELF_BIN").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_hvelf")))
}

fn command(f: &Fixture) -> Command {
    let mut c = Command::new(hvelf_bin());
    c.env_clear();
    for k in ["DISPLAY", "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS", "LD_LIBRARY_PATH"] {
        if let Ok(v) = std::env::var(k) {
            c.env(k, v);
        }
    }
    // The system's own directories only: an installed Lab-OS puts `keep` and
    // `exo` in ~/.local/bin and /usr/local/bin, and no test may reach them.
    c.env("PATH", "/usr/bin:/bin");
    for (k, v) in &f.env {
        c.env(k, v);
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
    let _turn = serial();
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

// ------------------------------------------- through Lab-OS's own `keep`

/// A Lab-OS kernel tree to run the real `keep` from: HVELF_LABOS_KERNEL,
/// holding bin/keep and runtime/synced.py (Lab-OS 0.2.0-rc.7 and later). Unset,
/// the test is skipped; set to anything else, it fails.
fn labos_kernel() -> Option<PathBuf> {
    let k = PathBuf::from(std::env::var("HVELF_LABOS_KERNEL").ok()?);
    assert!(
        k.join("bin/keep").is_file() && k.join("runtime/synced.py").is_file(),
        "HVELF_LABOS_KERNEL={} holds no bin/keep with runtime/synced.py",
        k.display()
    );
    Some(k)
}

/// A vault root like the native one, Lab-OS the fallback, Research/AI/AL with
/// its link AL, and a bounded Admin; `keep`'s own instance table for it; a
/// workspace catalogue with one private entry, whose id and name are private
/// territory; the folders in `synced` as the synced folder pairs. The board
/// reads the kernel's own vault map of that table, and files through the
/// kernel's `keep`. HOME is the fixture's, so no real Nextcloud pairs count.
fn keep_fixture(tag: &str, kernel: &Path, synced: &[&str]) -> Fixture {
    let home = root(tag);
    let cfg = home.join("cfg é");
    let vaults = home.join("Vaults root");
    for d in ["Lab-OS", "Research/AI/AL", "Admin"] {
        std::fs::create_dir_all(vaults.join(d)).unwrap();
    }
    std::os::unix::fs::symlink(vaults.join("Research/AI/AL"), vaults.join("AL")).unwrap();
    let toml = home.join("exocortex.toml");
    std::fs::write(
        &toml,
        format!("[vaults]\nroot = {:?}\nbounded = [\"Admin\"]\nfallback = \"Lab-OS\"\nexcluded = []\n", vaults.display().to_string()),
    )
    .unwrap();
    let catalogue = home.join("workspaces.json");
    let entry = serde_json::json!({"id": "zetamatter", "name": "Zeta Matter", "lifecycle": "active", "access_scope": "admin", "category": "admin"});
    std::fs::write(&catalogue, serde_json::json!({"schema": "labos-workspace-catalogue/v1", "entries": [entry]}).to_string()).unwrap();
    let map = home.join("map.py");
    std::fs::write(
        &map,
        "import importlib.util, json, sys, tomllib\n\
         spec = importlib.util.spec_from_file_location('labos_vaults', sys.argv[1])\n\
         v = importlib.util.module_from_spec(spec)\n\
         spec.loader.exec_module(v)\n\
         with open(sys.argv[2], 'rb') as fh:\n    print(json.dumps(v.from_config(tomllib.load(fh)).describe()))\n",
    )
    .unwrap();
    let s = |p: &Path| p.display().to_string();
    std::fs::create_dir_all(cfg.join("hvelf")).unwrap();
    let conf = serde_json::json!({
        "hotkey": "", "quickLaunch": "", "keep": "", "exocortex": s(&toml),
        "vaultMap": ["/usr/bin/python3", s(&map), s(&kernel.join("runtime/vaults.py")), s(&toml)],
        "keepCommand": ["/usr/bin/python3", s(&kernel.join("bin/keep"))],
        "hideOnBlur": false, "hide": [],
    });
    std::fs::write(cfg.join("hvelf/config.json"), conf.to_string()).unwrap();
    let pairs: Vec<String> = synced.iter().map(|d| s(&vaults.join(d)).trim_end_matches('/').to_string()).collect();
    let env = vec![
        ("EXO_CONFIG".to_string(), s(&toml)),
        ("LABOS_CATALOGUE".to_string(), s(&catalogue)),
        ("LABOS_SYNC_PAIRS".to_string(), serde_json::to_string(&pairs).unwrap()),
    ];
    let log = home.join("hvelf.log");
    Fixture { home, cfg, vaults, log, env }
}

/// A note `keep` wrote for a capture: its own format and id, the capture's
/// vault and passage verbatim, and where it came from in source_note.
fn check_kept(path: &Path, vault: &str, title: &str, text: &str) {
    let note = std::fs::read_to_string(path).unwrap();
    let (meta, body) = split(&note);
    let get = |k: &str| meta.iter().find(|(m, _)| m == k).map(|(_, v)| v.clone());
    let keys: Vec<&str> = meta.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["id", "ts", "source", "vault", "app", "source_note", "sha256"], "keep's own format");
    let id = get("id").unwrap();
    assert_eq!(path.file_name().unwrap().to_string_lossy(), format!("{id}.md"));
    assert_eq!(get("source").as_deref(), Some("selection"));
    assert_eq!(get("vault").as_deref(), Some(vault));
    assert_eq!(get("app").as_deref(), Some("terminal"), "keep's own value for text it is handed");
    let note_value: String = serde_json::from_str(&get("source_note").unwrap()).unwrap();
    assert_eq!(note_value, format!("hvelf capture from {}: {title}", this_exe()));
    let want = if text.ends_with('\n') { text.to_string() } else { format!("{text}\n") };
    assert_eq!(body, want, "body verbatim");
    let sha: String = Sha256::digest(want.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(get("sha256"), Some(sha));
}

/// Each board starts its log afresh: keep a scenario's beside the fixture.
fn save_log(f: &Fixture, scenario: &str) {
    let _ = std::fs::copy(&f.log, f.home.join(format!("hvelf {scenario}.log")));
}

/// Everything under the vault root that is not a folder: notes, and any
/// half-written `.part`.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => out.extend(files_in(&p)),
                Ok(t) if t.is_symlink() => {}
                _ => out.push(p),
            }
        }
    }
    out.sort();
    out
}

/// Captures go through Lab-OS's own `keep` (rc.7's, with the private-name
/// guard for synced vaults, FA-013). With the whole vault root a synced pair:
/// a plain passage is filed into Lab-OS in keep's format; a passage naming
/// the private entry is refused there and in Research/AI/AL, the refusal
/// keeps it on the board (the chord again is refused again), and nothing at
/// all is written, not even to the unrouted folder; the same passage is filed
/// into the bounded Admin, which never syncs. With no synced pair, it is filed
/// into Lab-OS. hvelf's log never holds the private name.
#[test]
fn capture_through_keep_is_refused_in_a_synced_vault_and_filed_elsewhere() {
    let _turn = serial();
    if !enabled() {
        return;
    }
    let Some(kernel) = labos_kernel() else {
        eprintln!("skipped: set HVELF_LABOS_KERNEL to a Lab-OS kernel tree holding bin/keep and runtime/synced.py");
        return;
    };
    let f = keep_fixture("guard", &kernel, &[""]);
    let v = f.vaults.clone();
    let private = "notes on the Zetamatter hearing\nsecond line, plain";
    let plain = "a plain note é \"q\"\n\tindented";
    let to_lab = "draft - Lab-OS - Obsidian v1.6.7";

    // Filing is through keep, which is there.
    {
        let board = Board::start(&f);
        let caps: Vec<serde_json::Value> = serde_json::from_slice(&run(&f, &["--capabilities"]).stdout).unwrap();
        let get = |name: &str| caps.iter().find(|c| c["capability"] == name).unwrap().clone();
        assert_eq!(get("keep-filing")["state"], "available", "{}", get("keep-filing"));
        assert_eq!(get("vault-map")["state"], "available", "{}", get("vault-map"));
        assert_eq!(get("keep-capture")["state"], "available", "{}", get("keep-capture"));

        // A plain passage into the synced Lab-OS: filed, in keep's format.
        let front = Front::new(to_lab, Some(plain));
        board.keep();
        board.wait_log("chars from");
        assert!(board.log().contains("Lab-OS is lit;"), "{}", board.log());
        board.keep();
        let got = wait_files(&v.join("Lab-OS/keeps"), 1);
        check_kept(&got[0], "Lab-OS", to_lab, plain);
        board.wait_log("kept \u{2192} Lab-OS");
        board.wait_log(&format!("filed by `/usr/bin/python3 {}` into Lab-OS as ", kernel.join("bin/keep").display()));
        drop(front);
    }
    save_log(&f, "1 plain into Lab-OS");
    let filed = files_in(&v);
    assert_eq!(filed.len(), 1, "{filed:?}");

    // The private name into Lab-OS: refused, kept on the board, refused again
    // on the chord, and nothing written anywhere.
    {
        let board = Board::start(&f);
        let front = Front::new(to_lab, Some(private));
        board.keep();
        board.wait_log("chars from");
        board.keep();
        board.wait_log("chars not filed into Lab-OS: refused: this keep would be written into");
        board.keep();
        wait_for("the second refusal", 10, || board.log().matches("chars not filed into Lab-OS").count() >= 2);
        let log = board.log();
        assert!(log.contains("names private territory (text: 1 private identifier and 0 private paths on line 1)"), "{log}");
        assert!(!log.to_lowercase().contains("zetamatter"), "the private name is in hvelf's log:\n{log}");
        assert!(!log.contains("kept \u{2192}"), "{log}");
        drop(front);
    }
    save_log(&f, "2 private into Lab-OS");
    // Into Research/AI/AL, synced too: refused.
    {
        let board = Board::start(&f);
        let front = Front::new("notes - AL - Obsidian v1.6.7", Some(private));
        board.keep();
        board.wait_log("Research/AI/AL is lit;");
        board.keep();
        board.wait_log("chars not filed into Research/AI/AL: refused");
        drop(front);
    }
    save_log(&f, "3 private into Research-AI-AL");
    assert_eq!(files_in(&v), filed, "refusals write nothing at all, no .part and no unrouted note");
    assert!(!v.join("Lab-OS/keeps/unrouted").exists());

    // Into the bounded Admin, inside the pair but never synced: filed.
    {
        let board = Board::start(&f);
        let title = "notes - Admin - Obsidian v1.6.7";
        let front = Front::new(title, Some(private));
        board.keep();
        board.wait_log("Admin is lit;");
        board.keep();
        let got = wait_files(&v.join("Admin/keeps"), 1);
        check_kept(&got[0], "Admin", title, private);
        board.wait_log("kept \u{2192} Admin");
        assert!(!board.log().to_lowercase().contains("zetamatter"), "{}", board.log());
        drop(front);
    }
    save_log(&f, "4 private into Admin");

    // No synced pair at all: the same passage is filed into Lab-OS.
    let g = keep_fixture("unsynced", &kernel, &[]);
    let board = Board::start(&g);
    let front = Front::new(to_lab, Some(private));
    board.keep();
    board.wait_log("chars from");
    board.keep();
    let got = wait_files(&g.vaults.join("Lab-OS/keeps"), 1);
    check_kept(&got[0], "Lab-OS", to_lab, private);
    drop(front);
    drop(board);
    save_log(&g, "5 private into an unsynced Lab-OS");
}

/// A keep command that is not there: the chord captures nothing, says why,
/// and nothing is written; the capability says so before any chord.
#[test]
fn a_missing_keep_command_captures_nothing() {
    let _turn = serial();
    if !enabled() {
        return;
    }
    let f = fixture_with("nokeep", None, true);
    let conf_path = f.cfg.join("hvelf/config.json");
    let mut conf: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&conf_path).unwrap()).unwrap();
    conf["keepCommand"] = serde_json::json!(["no-such-keep-command"]);
    std::fs::write(&conf_path, conf.to_string()).unwrap();
    let board = Board::start(&f);
    let caps: Vec<serde_json::Value> = serde_json::from_slice(&run(&f, &["--capabilities"]).stdout).unwrap();
    let kf = caps.iter().find(|c| c["capability"] == "keep-filing").unwrap();
    assert_eq!(kf["state"], "unavailable", "{kf}");
    let front = Front::new("draft - AL - Obsidian v1.6.7", Some("text"));
    board.keep();
    board.wait_log("no-such-keep-command is not on PATH (keepCommand in config.json), so nothing was captured");
    std::thread::sleep(Duration::from_millis(300));
    assert!(keeps_in(&f.vaults).is_empty(), "nothing written");
    drop(front);
}

/// A keysym's keycode in the display's keymap.
fn keycode_for(conn: &impl Connection, keysym: u32) -> u8 {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1).unwrap().reply().unwrap();
    let per = usize::from(map.keysyms_per_keycode).max(1);
    let i = map.keysyms.chunks(per).position(|syms| syms.contains(&keysym)).expect("keysym in the keymap");
    min + i as u8
}

/// A fixture whose board takes the given chords, sees GNOME through a fake `gsettings` printing `script`'s
/// answers, and publishes its records under its own XDG_RUNTIME_DIR.
fn chord_fixture(tag: &str, hotkey: &str, quick_launch: &str, keep: &str, gsettings: Option<&str>) -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let mut f = fixture(tag, None);
    let conf_path = f.cfg.join("hvelf/config.json");
    let mut conf: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&conf_path).unwrap()).unwrap();
    conf["hotkey"] = hotkey.into();
    conf["quickLaunch"] = quick_launch.into();
    conf["keep"] = keep.into();
    std::fs::write(&conf_path, conf.to_string()).unwrap();
    if let Some(script) = gsettings {
        let bin = f.home.join("gnome bin");
        std::fs::create_dir_all(&bin).unwrap();
        // Written by a child process, so no forked test thread holds it open when it runs (ETXTBSY).
        let path = bin.join("gsettings");
        let mut w = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 0755 \"$1\"", "sh"])
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut w.stdin.take().unwrap(), format!("#!/bin/sh\n{script}\n").as_bytes()).unwrap();
        assert!(w.wait().unwrap().success());
        f.env.push(("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())));
        f.env.push(("XDG_CURRENT_DESKTOP".into(), "ubuntu:GNOME".into()));
    }
    let rt = f.home.join("runtime");
    std::fs::create_dir_all(&rt).unwrap();
    std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.env.push(("XDG_RUNTIME_DIR".into(), rt.display().to_string()));
    f
}

/// `hvelf --capabilities` as a map from capability to (state, reason).
fn capability_map(f: &Fixture) -> std::collections::HashMap<String, (String, String)> {
    let out = run(f, &["--capabilities"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let caps: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    caps.into_iter()
        .map(|c| (c["capability"].as_str().unwrap_or_default().to_string(), (c["state"].as_str().unwrap_or_default().to_string(), c["reason"].as_str().unwrap_or_default().to_string())))
        .collect()
}

/// Whether another client can take `mods`+`key` on the root window now: Ok when free, the X error otherwise.
fn try_grab(conn: &impl Connection, root: u32, mods: x11rb::protocol::xproto::ModMask, key: u8) -> Result<(), String> {
    use x11rb::protocol::xproto::GrabMode;
    match conn.grab_key(true, root, mods, key, GrabMode::ASYNC, GrabMode::ASYNC).unwrap().check() {
        Ok(()) => {
            conn.ungrab_key(key, root, mods).unwrap();
            conn.flush().unwrap();
            Ok(())
        }
        Err(e) => Err(format!("{e:?}")),
    }
}

/// The owner's chords of 11 October (DELIVERY-11): Alt+` shows the board, Ctrl+Alt+1 is quick launch, Alt+K keeps.
/// GNOME runs `hvelf --toggle` on Alt+`, the same command, so hvelf leaves that chord to GNOME. GNOME runs
/// `hvelf --keep` on Ctrl+Alt+1, not quick launch, so hvelf leaves that one too and reports the conflict. Alt+K is
/// GNOME's for nothing, so hvelf grabs it, and a press reaches the board with Num Lock on and off. The board's
/// stderr, the X server and `hvelf --capabilities` agree on all three.
#[test]
fn chords_gnome_runs_for_hvelf_are_left_to_it_and_the_rest_are_grabbed() {
    if !enabled() {
        return;
    }
    let _s = serial();
    let gnome = "P=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings\ncase \"$1 $2 $3\" in\n  \"get org.gnome.settings-daemon.plugins.media-keys custom-keybindings\") echo \"['$P/hvelf-toggle/', '$P/hvelf-quick-launch/']\" ;;\n  *hvelf-toggle/\\ command) echo \"'hvelf --toggle'\" ;;\n  *hvelf-toggle/\\ binding) echo \"'<Alt>grave'\" ;;\n  *hvelf-toggle/\\ name) echo \"'hvelf: show or hide the board'\" ;;\n  *hvelf-quick-launch/\\ command) echo \"'hvelf --keep'\" ;;\n  *hvelf-quick-launch/\\ binding) echo \"'<Control><Alt>1'\" ;;\n  *hvelf-quick-launch/\\ name) echo \"'hvelf: keep, on the wrong chord'\" ;;\n  \"list-recursively org.gnome.desktop.wm.keybindings \") echo \"org.gnome.desktop.wm.keybindings switch-group ['<Super>Above_Tab']\" ;;\n  list-recursively*) ;;\n  *) exit 1 ;;\nesac";
    let f = chord_fixture("chords", "alt+grave", "ctrl+alt+1", "alt+k", Some(gnome));
    let board = Board::start(&f);
    board.wait_log("Alt+` is a GNOME custom shortcut that runs `hvelf --toggle`; GNOME handles it, so hvelf does not grab it");
    board.wait_log("Ctrl+Alt+1 is a GNOME custom shortcut that runs `hvelf --keep`, not `hvelf --quick-launch`; hvelf leaves it to GNOME and grabs nothing for it");
    board.wait_log("Alt+K grabbed on the root window");

    // The X server agrees: another client can take Alt+` and Ctrl+Alt+1, but Alt+K is hvelf's.
    use x11rb::protocol::xproto::ModMask;
    use x11rb::protocol::xtest::ConnectionExt as _;
    let (other, screen) = x11rb::connect(None).unwrap();
    let root = other.setup().roots[screen].root;
    let (grave, one, k) = (keycode_for(&other, 0x60), keycode_for(&other, 0x31), keycode_for(&other, 0x6b));
    assert_eq!(try_grab(&other, root, ModMask::M1, grave), Ok(()), "Alt+` is free for GNOME");
    assert_eq!(try_grab(&other, root, ModMask::CONTROL | ModMask::M1, one), Ok(()), "Ctrl+Alt+1 is free for GNOME");
    let held = try_grab(&other, root, ModMask::M1, k);
    assert!(held.as_ref().is_err_and(|e| e.contains("Access")), "Alt+K is held by hvelf: {held:?}");

    // `hvelf --capabilities` prints the board's own record, refreshed after its grabs, and GNOME's binding.
    let c = capability_map(&f);
    let (state, reason) = &c["global-hotkey-x11"];
    assert_eq!(state, "degraded", "{reason}");
    assert!(reason.contains("Alt+K grabbed on the root window"), "{reason}");
    assert!(reason.contains("'ctrl+alt+1' runs `hvelf --keep` as a GNOME custom shortcut, not `hvelf --quick-launch`, so hvelf leaves it to GNOME"), "{reason}");
    assert!(reason.contains("Alt+` runs `hvelf --toggle` as a GNOME custom shortcut"), "{reason}");
    let (state, reason) = &c["global-hotkey-desktop"];
    assert_eq!(state, "available", "{reason}");
    assert!(reason.contains("Alt+` runs `hvelf --toggle`, Ctrl+Alt+1 runs `hvelf --keep`"), "{reason}");

    // Alt+K reaches the board with Num Lock on, then off. Nothing is selected, so the keeps write nothing.
    let (num, alt) = (keycode_for(&other, 0xff7f), keycode_for(&other, 0xffe9));
    let tap = |key: u8| {
        for kind in [2u8, 3] {
            other.xtest_fake_input(kind, key, 0, root, 0, 0, 0).unwrap();
        }
        other.flush().unwrap();
    };
    let num_lock = || u16::from(other.query_pointer(root).unwrap().reply().unwrap().mask) & (1 << 4) != 0;
    let presses = || board.log().matches("hvelf: Alt+K pressed").count();
    if num_lock() {
        tap(num);
    }
    tap(num);
    wait_for("Num Lock on", 2, num_lock);
    for (kind, key) in [(2u8, alt), (2, k), (3, k), (3, alt)] {
        other.xtest_fake_input(kind, key, 0, root, 0, 0, 0).unwrap();
    }
    other.flush().unwrap();
    wait_for("the press with Num Lock on", 10, || presses() == 1);
    tap(num);
    wait_for("Num Lock off", 2, || !num_lock());
    for (kind, key) in [(2u8, alt), (2, k), (3, k), (3, alt)] {
        other.xtest_fake_input(kind, key, 0, root, 0, 0, 0).unwrap();
    }
    other.flush().unwrap();
    wait_for("the press with Num Lock off", 10, || presses() == 2);
    assert!(keeps_in(&f.vaults).is_empty(), "nothing was written");
}

/// A chord another program already holds when the board starts (the leading cause of acceptance A4's failure of
/// 11 October): the board says so on stderr, in its record, and through `hvelf --capabilities`, rather than
/// printing nothing while the chord does nothing.
#[test]
fn a_chord_another_program_holds_is_reported_where_people_look() {
    if !enabled() {
        return;
    }
    let _s = serial();
    use x11rb::protocol::xproto::ModMask;
    let (other, screen) = x11rb::connect(None).unwrap();
    let root = other.setup().roots[screen].root;
    let grave = keycode_for(&other, 0x60);
    let held = ModMask::CONTROL | ModMask::M1;
    use x11rb::protocol::xproto::GrabMode;
    other.grab_key(true, root, held, grave, GrabMode::ASYNC, GrabMode::ASYNC).unwrap().check().expect("the stand-in program takes Ctrl+Alt+`");

    let f = chord_fixture("held", "ctrl+alt+grave", "", "", None);
    let board = Board::start(&f);
    board.wait_log("hvelf: Ctrl+Alt+` is already grabbed by another program, so hvelf could not take it");
    let c = capability_map(&f);
    let (state, reason) = &c["global-hotkey-x11"];
    assert_eq!(state, "unavailable", "{reason}");
    assert!(reason.contains("'ctrl+alt+grave' is already grabbed by another program"), "{reason}");
    other.ungrab_key(grave, root, held).unwrap();
    other.flush().unwrap();
    drop(board);

    // Once the board has quit, nothing holds a grab for hvelf, and the record says so.
    let c = capability_map(&f);
    let (state, reason) = &c["global-hotkey-x11"];
    assert_eq!(state, "unavailable", "{reason}");
    assert!(reason.contains("no board is running"), "{reason}");
}
