//! Black-box checks of the built binary on Linux, under a relocated HOME and
//! XDG tree whose paths hold spaces, quotes and Unicode. Fixtures only: no
//! real vault, registry or desktop session is read.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root(tag: &str) -> PathBuf {
    let base = std::env::var("HVELF_FIXTURE_ROOT").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    let d = base.join(format!("hvelf {tag} ü \"q\" {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fake_bin(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// hvelf with a clean environment: only what the test names.
fn hvelf(args: &[&str], env: &[(&str, &Path)], extra: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hvelf"));
    c.env_clear().args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    for (k, v) in extra {
        c.env(k, v);
    }
    // Test binaries link WebKitGTK from the build root's sysroot.
    if let Ok(l) = std::env::var("LD_LIBRARY_PATH") {
        c.env("LD_LIBRARY_PATH", l);
    }
    c.output().unwrap()
}

const REGISTRY: &str = r#"{"vaults":{
  "a1b2c3d4e5f60718":{"path":"__HOME__/Lab/Vaults/Research/notes","ts":1790000000000,"open":true},
  "0918273645abcdef":{"path":"__HOME__/archive/notes","ts":1780000000000,"open":true},
  "ffffeeee00001111":{"path":"__HOME__/Vaults/Ünïcode \"quoted\" <b>vault<\\b>","ts":1770000000000}
}}"#;

fn write_registry(at: &Path, home: &Path) {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    let body = REGISTRY.replace("__HOME__", &home.display().to_string().replace('"', "\\\""));
    std::fs::write(at, body).unwrap();
}

fn tiles(out: &Output) -> Vec<serde_json::Value> {
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn xdg_registry_duplicate_names_stale_flags_and_config_across_restart() {
    let home = root("xdg");
    let cfg = home.join("cfg dir é");
    let bin = home.join("bin");
    fake_bin(&bin, "xdg-mime", "exit 0");
    write_registry(&cfg.join("obsidian/obsidian.json"), &home);
    // A decoy at the standard path proves the XDG registry is the one read.
    std::fs::create_dir_all(home.join(".config/obsidian")).unwrap();
    std::fs::write(home.join(".config/obsidian/obsidian.json"), r#"{"vaults":{}}"#).unwrap();

    let env = [("HOME", home.as_path()), ("XDG_CONFIG_HOME", cfg.as_path()), ("PATH", bin.as_path())];
    let t = tiles(&hvelf(&["--list-vaults"], &env, &[]));
    assert_eq!(t.len(), 3);
    let notes: Vec<&serde_json::Value> = t.iter().filter(|x| x["name"] == "notes").collect();
    assert_eq!(notes.len(), 2, "duplicate basenames stay two tiles");
    assert_ne!(notes[0]["id"], notes[1]["id"]);
    assert_ne!(notes[0]["qualifier"], notes[1]["qualifier"]);
    let odd = t.iter().find(|x| x["id"] == "ffffeeee00001111").unwrap();
    assert_eq!(odd["name"], "Ünïcode \"quoted\" <b>vault<\\b>");
    // Headless: no window can be observed, so Obsidian's flags are shown as
    // reported (possibly stale) and closing is disabled with a reason.
    for n in &notes {
        assert_eq!(n["open"], true);
        assert_eq!(n["open_state"], "reported");
        assert!(!n["close_reason"].as_str().unwrap().is_empty());
    }
    // Config seeded under XDG_CONFIG_HOME, not the standard ~/.config.
    let conf = cfg.join("hvelf/config.json");
    assert!(conf.is_file());
    assert!(!home.join(".config/hvelf").exists());

    // Restart: a path in `hide` removes exactly one of the namesakes, and
    // keys this build does not know survive.
    let mut doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&conf).unwrap()).unwrap();
    doc["hide"] = serde_json::json!([format!("{}/archive/notes", home.display())]);
    doc["futureKey"] = serde_json::json!({"kept": true});
    std::fs::write(&conf, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    let t = tiles(&hvelf(&["--list-vaults"], &env, &[]));
    assert_eq!(t.len(), 2);
    assert_eq!(t.iter().filter(|x| x["name"] == "notes").count(), 1);
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&conf).unwrap()).unwrap();
    assert_eq!(doc["futureKey"]["kept"], true);
}

#[test]
fn standard_location_is_read_without_xdg() {
    let home = root("std");
    let bin = home.join("bin");
    fake_bin(&bin, "xdg-mime", "exit 0");
    write_registry(&home.join(".config/obsidian/obsidian.json"), &home);
    let t = tiles(&hvelf(&["--list-vaults"], &[("HOME", home.as_path()), ("PATH", bin.as_path())], &[]));
    assert_eq!(t.len(), 3);
    assert!(home.join(".config/hvelf/config.json").is_file());
}

#[test]
fn missing_and_malformed_registries_are_named() {
    let home = root("bad");
    let out = hvelf(&["--list-vaults"], &[("HOME", home.as_path())], &[]);
    assert_eq!(out.status.code(), Some(1));
    let f: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!((f["ok"].clone(), f["code"].clone()), (serde_json::json!(false), serde_json::json!("missing-input")));

    std::fs::create_dir_all(home.join(".config/obsidian")).unwrap();
    std::fs::write(home.join(".config/obsidian/obsidian.json"), "{\"vaults\": [truncated").unwrap();
    let out = hvelf(&["--list-vaults"], &[("HOME", home.as_path())], &[]);
    assert_eq!(out.status.code(), Some(1));
    let f: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(f["code"], "malformed-data");
}

fn caps(out: &Output) -> std::collections::HashMap<String, (String, String)> {
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    v.into_iter()
        .map(|c| {
            (
                c["capability"].as_str().unwrap().to_string(),
                (c["state"].as_str().unwrap().to_string(), c["reason"].as_str().unwrap().to_string()),
            )
        })
        .collect()
}

#[test]
fn capability_matrix_is_honest_headless_and_on_wayland() {
    let home = root("caps");
    let nohandler = home.join("bin-nohandler");
    fake_bin(&nohandler, "xdg-mime", "exit 0");
    fake_bin(&nohandler, "xdg-open", "exit 0");
    write_registry(&home.join(".config/obsidian/obsidian.json"), &home);

    // Headless, no handler registered, no session bus.
    let c = caps(&hvelf(&["--capabilities"], &[("HOME", home.as_path()), ("PATH", nohandler.as_path())], &[]));
    assert_eq!(c["session"].0, "available");
    assert_eq!(c["vault-registry"].0, "available");
    assert_eq!(c["open-vault"].0, "unavailable");
    assert!(c["open-vault"].1.contains("x-scheme-handler/obsidian"));
    assert_eq!(c["open-state"].0, "degraded");
    assert_eq!(c["close-window"].0, "unavailable");
    assert_eq!(c["toggle-command"].0, "unavailable");
    assert_eq!(c["global-hotkey-portal"].0, "unavailable");

    // No opener at all.
    let empty = home.join("bin-empty");
    std::fs::create_dir_all(&empty).unwrap();
    let c = caps(&hvelf(&["--capabilities"], &[("HOME", home.as_path()), ("PATH", empty.as_path())], &[]));
    assert_eq!(c["open-vault"].0, "unavailable");
    assert!(c["open-vault"].1.contains("xdg-utils"));

    // Wayland, a handler, and a named but dead bus: window control is
    // unsupported by design, focus is delegated, the portal is unreachable.
    let ok = home.join("bin-ok");
    fake_bin(&ok, "xdg-mime", "echo obsidian.desktop");
    fake_bin(&ok, "xdg-open", "exit 0");
    let dead = format!("unix:path={}/no-such-bus", home.display());
    let c = caps(&hvelf(
        &["--capabilities"],
        &[("HOME", home.as_path()), ("PATH", ok.as_path())],
        &[("WAYLAND_DISPLAY", "wayland-test"), ("DISPLAY", ":99"), ("DBUS_SESSION_BUS_ADDRESS", &dead)],
    ));
    assert_eq!(c["session"].1, "Wayland session");
    assert_eq!(c["open-vault"].0, "available");
    assert_eq!(c["close-window"].0, "unsupported");
    assert_eq!(c["focus-window"].0, "degraded");
    assert_eq!(c["focus-recency"].0, "unsupported");
    assert_eq!(c["toggle-command"].0, "unavailable");
    assert_eq!(c["global-hotkey-portal"].0, "unavailable");
}

/// GNOME as `gsettings` shows it with the coordinator's custom shortcuts for hvelf and GNOME's own switch-group
/// binding; any other question is an error, so the test sees every call it did not expect.
const FAKE_GSETTINGS: &str = r#"P=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings
case "$1 $2 $3" in
  "get org.gnome.settings-daemon.plugins.media-keys custom-keybindings") echo "['$P/hvelf-toggle/', '$P/hvelf-keep/']" ;;
  *hvelf-toggle/\ command) echo "'hvelf --toggle'" ;;
  *hvelf-toggle/\ binding) echo "'<Control><Alt>grave'" ;;
  *hvelf-toggle/\ name) echo "'hvelf: show or hide the board'" ;;
  *hvelf-keep/\ command) echo "'hvelf --keep'" ;;
  *hvelf-keep/\ binding) echo "'<Alt>k'" ;;
  *hvelf-keep/\ name) echo "'hvelf: keep the highlighted text'" ;;
  "list-recursively org.gnome.desktop.wm.keybindings ") echo "org.gnome.desktop.wm.keybindings switch-group ['<Super>Above_Tab', '<Alt>Above_Tab']" ;;
  list-recursively*) ;;
  *) echo "unexpected gsettings call: $*" >&2; exit 1 ;;
esac"#;

/// The chords' state is reported truthfully (DELIVERY-11): GNOME's custom shortcuts that run hvelf, and the
/// running board's own grab record, or why there is none.
#[test]
fn chords_report_the_desktop_binding_and_the_boards_own_grab_state() {
    let home = root("chords");
    let bin = home.join("bin");
    fake_bin(&bin, "gsettings", FAKE_GSETTINGS);
    fake_bin(&bin, "xdg-mime", "echo obsidian.desktop");
    fake_bin(&bin, "xdg-open", "exit 0");
    let rt = home.join("runtime");
    std::fs::create_dir_all(&rt).unwrap();
    std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = [("HOME", home.as_path()), ("PATH", bin.as_path()), ("XDG_RUNTIME_DIR", rt.as_path())];
    let gnome = [("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")];

    // GNOME's custom shortcuts for hvelf are named; no board has run, so its grab state is not known.
    let c = caps(&hvelf(&["--capabilities"], &env, &gnome));
    assert_eq!(c["global-hotkey-desktop"].0, "available");
    assert!(c["global-hotkey-desktop"].1.contains("Ctrl+Alt+` runs `hvelf --toggle`"), "{}", c["global-hotkey-desktop"].1);
    assert!(c["global-hotkey-desktop"].1.contains("Alt+K runs `hvelf --keep`"), "{}", c["global-hotkey-desktop"].1);
    assert_eq!(c["global-hotkey-x11"].0, "unknown");
    assert!(c["global-hotkey-x11"].1.contains("no board has published"), "{}", c["global-hotkey-x11"].1);

    // Outside GNOME there is no desktop binding to report.
    let c = caps(&hvelf(&["--capabilities"], &env, &[("XDG_CURRENT_DESKTOP", "KDE")]));
    assert!(!c.contains_key("global-hotkey-desktop"));

    // A board that published and has stopped since holds no grab.
    std::fs::create_dir_all(rt.join("hvelf")).unwrap();
    let record = |pid: u32| {
        serde_json::json!({"pid": pid, "capabilities": [{"capability": "global-hotkey-x11", "state": "available",
            "reason": "Alt+K grabbed on the root window by keycode", "source": "XGrabKey", "platform": "linux-x11"}]})
        .to_string()
    };
    std::fs::write(rt.join("hvelf/board.json"), record(u32::MAX - 1)).unwrap();
    let c = caps(&hvelf(&["--capabilities"], &env, &gnome));
    assert_eq!(c["global-hotkey-x11"].0, "unavailable");
    assert!(c["global-hotkey-x11"].1.contains("no board is running"), "{}", c["global-hotkey-x11"].1);

    // A live board: its record is printed as it published it. A copy of `sleep` named hvelf stands in for it.
    let stand_in = home.join("board-bin").join("hvelf");
    std::fs::create_dir_all(stand_in.parent().unwrap()).unwrap();
    std::fs::copy("/bin/sleep", &stand_in).unwrap();
    let mut board = Command::new(&stand_in).arg("30").spawn().unwrap();
    std::fs::write(rt.join("hvelf/board.json"), record(board.id())).unwrap();
    let c = caps(&hvelf(&["--capabilities"], &env, &gnome));
    let _ = board.kill();
    let _ = board.wait();
    assert_eq!(c["global-hotkey-x11"].0, "available");
    assert!(c["global-hotkey-x11"].1.contains("Alt+K grabbed"), "{}", c["global-hotkey-x11"].1);

    // Without XDG_RUNTIME_DIR there is nowhere to look.
    let c = caps(&hvelf(&["--capabilities"], &[("HOME", home.as_path()), ("PATH", bin.as_path())], &gnome));
    assert_eq!(c["global-hotkey-x11"].0, "unknown");
    assert!(c["global-hotkey-x11"].1.contains("XDG_RUNTIME_DIR"), "{}", c["global-hotkey-x11"].1);
}

#[test]
fn arguments_are_data_never_commands() {
    let home = root("args");
    let out = hvelf(&["obsidian://open?vault=x; touch pwned"], &[("HOME", home.as_path())], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
    assert!(!home.join("pwned").exists());
    let out = hvelf(&["--toggle", "--quit"], &[("HOME", home.as_path())], &[]);
    assert_eq!(out.status.code(), Some(2));
}

/// With Lab-OS's vault map, the board lists one tile per vault: a link and
/// the folder it points at, both in Obsidian's registry, are one vault; a
/// bounded vault says so; a vault of the map that Obsidian has never opened
/// is listed as such; an entry whose folder is gone is marked missing.
#[test]
fn the_vault_map_dedups_links_and_marks_bounded_unknown_and_missing() {
    let home = root("map");
    let cfg = home.join("cfg");
    let bin = home.join("bin");
    fake_bin(&bin, "xdg-mime", "exit 0");
    let v = home.join("Vaults");
    for d in ["Research/AI/AL/.obsidian", "Admin/.obsidian", "Lab-OS/.obsidian"] {
        std::fs::create_dir_all(v.join(d)).unwrap();
    }
    std::os::unix::fs::symlink(v.join("Research/AI/AL"), v.join("AL")).unwrap();
    let r = v.display().to_string();
    let map = serde_json::json!({
        "schema": "labos-vault-map/v1", "root": r, "fallback": "Lab-OS",
        "vaults": [
            {"id": "Admin", "path": format!("{r}/Admin"), "partition": "Admin", "bounded": true},
            {"id": "Lab-OS", "path": format!("{r}/Lab-OS"), "partition": "global"},
            {"id": "Research/AI/AL", "path": format!("{r}/Research/AI/AL"), "partition": "global", "aliases": ["AL"]},
        ]
    });
    std::fs::write(home.join("map.json"), map.to_string()).unwrap();
    fake_bin(&bin, "exo", &format!("[ \"$1 $2\" = \"vaults --json\" ] && /bin/cat '{}'", home.join("map.json").display()));
    let registry = serde_json::json!({"vaults": {
        "1111111111111111": {"path": format!("{r}/AL"), "ts": 5},
        "2222222222222222": {"path": format!("{r}/Research/AI/AL"), "ts": 9},
        "3333333333333333": {"path": format!("{r}/Admin"), "ts": 1},
        "4444444444444444": {"path": "/gone/Dropbox/melt", "ts": 3},
    }});
    std::fs::create_dir_all(cfg.join("obsidian")).unwrap();
    std::fs::write(cfg.join("obsidian/obsidian.json"), registry.to_string()).unwrap();

    let env = [("HOME", home.as_path()), ("XDG_CONFIG_HOME", cfg.as_path()), ("PATH", bin.as_path())];
    let t = tiles(&hvelf(&["--list-vaults"], &env, &[]));
    let ids: Vec<&str> = t.iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(t.len(), 4, "{ids:?}");
    let by = |id: &str| t.iter().find(|x| x["id"] == id).unwrap_or_else(|| panic!("no tile {id} in {ids:?}"));
    assert_eq!(by("2222222222222222")["canonical"], "Research/AI/AL", "the one Obsidian used last");
    assert!(!ids.contains(&"1111111111111111"), "the link is the same vault");
    assert_eq!(by("3333333333333333")["bounded"], true);
    assert_eq!(by("4444444444444444")["missing"], true);
    assert_eq!(by("map:Lab-OS")["registered"], false);

    // Without the map command the registry is shown as it is, and says so.
    let plain = home.join("bin-plain");
    fake_bin(&plain, "xdg-mime", "exit 0");
    let env = [("HOME", home.as_path()), ("XDG_CONFIG_HOME", cfg.as_path()), ("PATH", plain.as_path())];
    let out = hvelf(&["--list-vaults"], &env, &[]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("no vault map"));
    assert_eq!(tiles(&out).len(), 4, "all four registry entries, the missing one marked");
}

/// The way back without a tray: `--hidden` lists the hidden vaults as the
/// tray menu labels them, and `--unhide` refuses, where it was typed, a name
/// that matches nothing, before any board is reached.
#[test]
fn hidden_lists_the_way_back_and_unhide_refuses_what_matches_nothing() {
    let home = root("hidden");
    let cfg = home.join("cfg");
    let bin = home.join("bin");
    fake_bin(&bin, "xdg-mime", "exit 0");
    write_registry(&cfg.join("obsidian/obsidian.json"), &home);
    std::fs::create_dir_all(cfg.join("hvelf")).unwrap();
    let archive = format!("{}/archive/notes", home.display());
    let conf = serde_json::json!({"hide": [archive, "/gone/Index"], "vaultMap": []});
    std::fs::write(cfg.join("hvelf/config.json"), conf.to_string()).unwrap();
    let env = [("HOME", home.as_path()), ("XDG_CONFIG_HOME", cfg.as_path()), ("PATH", bin.as_path())];

    let out = hvelf(&["--hidden"], &env, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let items: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        items,
        [
            serde_json::json!({"entry": "/gone/Index", "label": "Index (not in Obsidian)"}),
            serde_json::json!({"entry": archive, "label": "notes (archive)"}),
        ]
    );

    let out = hvelf(&["--unhide", "nothing-hidden"], &env, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing hidden matches"));
    let out = hvelf(&["--unhide"], &env, &[]);
    assert_eq!(out.status.code(), Some(2));
    // Nothing was touched.
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(cfg.join("hvelf/config.json")).unwrap()).unwrap();
    assert_eq!(after["hide"].as_array().unwrap().len(), 2);
}

/// How keeps are filed: by `keep` when it is on PATH, so its checks apply;
/// refused rather than written around it when it is not; and by hvelf itself,
/// without those checks, when keepCommand is empty, which the record says.
#[test]
fn keep_filing_follows_the_keep_command() {
    let home = root("filing");
    write_registry(&home.join(".config/obsidian/obsidian.json"), &home);
    let with = home.join("bin with keep");
    fake_bin(&with, "keep", "exit 0");
    let without = home.join("bin-without");
    std::fs::create_dir_all(&without).unwrap();

    let c = caps(&hvelf(&["--capabilities"], &[("HOME", home.as_path()), ("PATH", with.as_path())], &[]));
    assert_eq!(c["keep-filing"].0, "available", "{:?}", c["keep-filing"]);
    assert!(c["keep-filing"].1.contains(&format!("{}/keep", with.display())), "{:?}", c["keep-filing"]);

    let c = caps(&hvelf(&["--capabilities"], &[("HOME", home.as_path()), ("PATH", without.as_path())], &[]));
    assert_eq!(c["keep-filing"].0, "unavailable", "{:?}", c["keep-filing"]);
    assert!(c["keep-filing"].1.contains("captures nothing"), "{:?}", c["keep-filing"]);

    let cfg = home.join("cfg");
    std::fs::create_dir_all(cfg.join("hvelf")).unwrap();
    std::fs::write(cfg.join("hvelf/config.json"), r#"{"keepCommand":[]}"#).unwrap();
    let env = [("HOME", home.as_path()), ("XDG_CONFIG_HOME", cfg.as_path()), ("PATH", with.as_path())];
    let c = caps(&hvelf(&["--capabilities"], &env, &[]));
    assert_eq!(c["keep-filing"].0, "degraded", "{:?}", c["keep-filing"]);
    assert!(c["keep-filing"].1.contains("private-name guard"), "{:?}", c["keep-filing"]);
}
