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
