// hvelf — hotkey-summoned tile board for launching Obsidian vaults.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_global_shortcut::ShortcutState;

// ---------------------------------------------------------------- config

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Config {
    hotkey: String,
    hide_on_blur: bool,
    hide: Vec<String>,
    groups: Vec<Group>,
    deep_links: HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
struct Group {
    name: String,
    vaults: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkey: "ctrl+alt+o".into(),
            hide_on_blur: true,
            hide: Vec::new(),
            groups: Vec::new(),
            deep_links: HashMap::new(),
        }
    }
}

fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var("APPDATA").expect("APPDATA not set")).join("hvelf")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(std::env::var("HOME").expect("HOME not set"))
            .join(".config")
            .join("hvelf")
    }
}

fn load_or_seed_config() -> Config {
    let path = config_dir().join("config.json");
    if let Ok(raw) = fs::read_to_string(&path) {
        match serde_json::from_str(&raw) {
            Ok(cfg) => return cfg,
            Err(e) => eprintln!("hvelf: config.json invalid ({e}); using defaults"),
        }
    } else {
        let cfg = Config::default();
        let _ = fs::create_dir_all(config_dir());
        let _ = fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap());
        return cfg;
    }
    Config::default()
}

// ---------------------------------------------------------------- vaults

#[derive(Serialize, Clone)]
struct VaultTile {
    name: String,
    path: String,
    open: bool,
    group: String,
}

fn obsidian_json_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var("APPDATA").expect("APPDATA not set"))
            .join("obsidian")
            .join("obsidian.json")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(std::env::var("HOME").expect("HOME not set"))
            .join("Library/Application Support/obsidian/obsidian.json")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        PathBuf::from(std::env::var("HOME").expect("HOME not set"))
            .join(".config/obsidian/obsidian.json")
    }
}

#[derive(Deserialize)]
struct ObsidianRegistry {
    #[serde(default)]
    vaults: HashMap<String, ObsidianVault>,
}

#[derive(Deserialize)]
struct ObsidianVault {
    path: String,
}

fn registered_vaults() -> Vec<(String, String)> {
    let raw = match fs::read_to_string(obsidian_json_path()) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let reg: ObsidianRegistry = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<(String, String)> = reg
        .vaults
        .values()
        .map(|v| {
            let name = v
                .path
                .replace('\\', "/")
                .rsplit('/')
                .next()
                .unwrap_or(&v.path)
                .to_string();
            (name, v.path.clone())
        })
        .collect();
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out
}

/// Vault names with a live Obsidian window, parsed from window titles of the
/// form "<note> - <vault> - Obsidian <version>".
#[cfg(windows)]
fn open_vault_names() -> HashSet<String> {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
    };

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let titles = &mut *(lparam as *mut Vec<String>);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len > 0 {
            let mut buf = vec![0u16; (len + 1) as usize];
            let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), len + 1);
            if got > 0 {
                titles.push(String::from_utf16_lossy(&buf[..got as usize]));
            }
        }
        1
    }

    let mut titles: Vec<String> = Vec::new();
    unsafe {
        EnumWindows(Some(cb), &mut titles as *mut Vec<String> as LPARAM);
    }
    titles
        .into_iter()
        .filter_map(|t| {
            let parts: Vec<&str> = t.split(" - ").collect();
            if parts.len() >= 3 && parts.last().map_or(false, |l| l.starts_with("Obsidian")) {
                Some(parts[parts.len() - 2].to_string())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(not(windows))]
fn open_vault_names() -> HashSet<String> {
    HashSet::new()
}

// ---------------------------------------------------------------- commands

#[tauri::command]
fn list_vaults(cfg: State<Config>) -> Vec<VaultTile> {
    let open = open_vault_names();
    let hidden: HashSet<&String> = cfg.hide.iter().collect();
    let mut group_of: HashMap<&String, &String> = HashMap::new();
    for g in &cfg.groups {
        for v in &g.vaults {
            group_of.insert(v, &g.name);
        }
    }
    let mut tiles: Vec<VaultTile> = registered_vaults()
        .into_iter()
        .filter(|(name, _)| !hidden.contains(name))
        .map(|(name, path)| VaultTile {
            open: open.contains(&name),
            group: group_of.get(&name).map(|s| s.to_string()).unwrap_or_default(),
            name,
            path,
        })
        .collect();
    // Stable order: configured groups first (in config order), then the rest.
    let order: HashMap<&String, usize> =
        cfg.groups.iter().enumerate().map(|(i, g)| (&g.name, i)).collect();
    tiles.sort_by_key(|t| {
        let g = order.get(&t.group).copied().unwrap_or(usize::MAX);
        (g, t.name.to_lowercase())
    });
    tiles
}

#[tauri::command]
fn launch(app: AppHandle, cfg: State<Config>, name: String) {
    let mut uri = format!("obsidian://open?vault={}", urlencoding::encode(&name));
    if let Some(file) = cfg.deep_links.get(&name) {
        uri.push_str(&format!("&file={}", urlencoding::encode(file)));
    }
    #[cfg(windows)]
    let res = std::process::Command::new("explorer.exe").arg(&uri).spawn();
    #[cfg(target_os = "macos")]
    let res = std::process::Command::new("open").arg(&uri).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let res = std::process::Command::new("xdg-open").arg(&uri).spawn();
    if let Err(e) = res {
        eprintln!("hvelf: failed to launch {uri}: {e}");
    }
    hide_window(app);
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

fn toggle_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

// ---------------------------------------------------------------- main

fn main() {
    let cfg = load_or_seed_config();
    let hotkey = cfg.hotkey.clone();
    let hide_on_blur = cfg.hide_on_blur;

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            toggle_window(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcuts([hotkey.as_str()])
                .expect("hvelf: invalid hotkey in config.json")
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_window(app);
                    }
                })
                .build(),
        )
        .manage(cfg)
        .on_window_event(move |window, event| {
            if hide_on_blur {
                if let tauri::WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![list_vaults, launch, hide_window, quit])
        .run(tauri::generate_context!())
        .expect("hvelf: failed to start");
}
