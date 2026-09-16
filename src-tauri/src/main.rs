// hvelf: hotkey-summoned tile board for launching Obsidian vaults.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

#[cfg(target_os = "macos")]
mod hotkey_macos;

// ---------------------------------------------------------------- config

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Config {
    hotkey: String,
    /// Optional second hotkey that opens the most recent vault directly,
    /// without showing the board. Empty string disables it.
    quick_launch: String,
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
            hotkey: "alt+grave".into(),
            quick_launch: String::new(),
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
    /// Last-opened timestamp (ms epoch), maintained by Obsidian itself.
    #[serde(default)]
    ts: u64,
    /// Obsidian's own record of whether this vault has a window open, and it
    /// tracks every open vault rather than only the newest. On macOS this is
    /// where the open state comes from; see open_vault_names.
    #[serde(default)]
    open: bool,
}

fn registered_vaults() -> Vec<(String, String, u64)> {
    let raw = match fs::read_to_string(obsidian_json_path()) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let reg: ObsidianRegistry = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    reg.vaults
        .values()
        .map(|v| {
            let name = v
                .path
                .replace('\\', "/")
                .rsplit('/')
                .next()
                .unwrap_or(&v.path)
                .to_string();
            (name, v.path.clone(), v.ts)
        })
        .collect()
}

// ---------------------------------------------------------- focus history
//
// Obsidian's `ts` stamps a vault when it is OPENED, not while it is used, so
// on its own it misranks a vault you have had open all day but are typing in
// right now. hvelf keeps its own record of which vault window was last in
// the foreground (sampled every 2s by the hotkey thread) and ranks by
// whichever signal is newer.

#[derive(Clone, Default)]
struct FocusHistory(std::sync::Arc<std::sync::Mutex<HashMap<String, u64>>>);

impl FocusHistory {
    fn note(&self, vault: String) {
        self.0.lock().unwrap().insert(vault, now_ms());
    }
    fn get(&self, vault: &str) -> u64 {
        self.0.lock().unwrap().get(vault).copied().unwrap_or(0)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Extract the vault name from an Obsidian window title of the form
/// "<note> - <vault> - Obsidian <version>".
fn vault_from_title(title: &str) -> Option<String> {
    let parts: Vec<&str> = title.split(" - ").collect();
    if parts.len() >= 3 && parts.last().map_or(false, |l| l.starts_with("Obsidian")) {
        Some(parts[parts.len() - 2].to_string())
    } else {
        None
    }
}

#[cfg(windows)]
fn foreground_vault() -> Option<String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
    unsafe {
        let h = GetForegroundWindow();
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 512];
        let got = GetWindowTextW(h, buf.as_mut_ptr(), 512);
        if got <= 0 {
            return None;
        }
        vault_from_title(&String::from_utf16_lossy(&buf[..got as usize]))
    }
}

/// Live Obsidian windows as (hwnd, vault name), parsed from window titles of
/// the form "<note> - <vault> - Obsidian <version>".
#[cfg(windows)]
fn obsidian_windows() -> Vec<(isize, String)> {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
    };

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let wins = &mut *(lparam as *mut Vec<(isize, String)>);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len > 0 {
            let mut buf = vec![0u16; (len + 1) as usize];
            let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), len + 1);
            if got > 0 {
                wins.push((hwnd as isize, String::from_utf16_lossy(&buf[..got as usize])));
            }
        }
        1
    }

    let mut wins: Vec<(isize, String)> = Vec::new();
    unsafe {
        EnumWindows(Some(cb), &mut wins as *mut Vec<(isize, String)> as LPARAM);
    }
    wins.into_iter()
        .filter_map(|(h, t)| vault_from_title(&t).map(|v| (h, v)))
        .collect()
}

#[cfg(not(windows))]
fn obsidian_windows() -> Vec<(isize, String)> {
    Vec::new()
}

/// Which vaults are open, for the indicator on each tile.
///
/// Windows reads this off the live window list, which is exact and also
/// yields the handle needed to focus one. macOS has no equivalent that is
/// free: both routes to another app's window titles are permission-gated,
/// the accessibility API and CGWindowList's window names. Obsidian, though,
/// already records the answer in its own registry and keeps a flag per vault
/// rather than only for the newest, so the indicator costs nothing here: no
/// permission, no prompt, no window enumeration.
///
/// The flag is written when a vault opens or closes, so it can lag a crash
/// that leaves it set. It is the indicator that is briefly wrong, which is
/// the cheapest thing in the app to be wrong.
#[cfg(target_os = "macos")]
fn open_vault_names() -> HashSet<String> {
    let raw = match fs::read_to_string(obsidian_json_path()) {
        Ok(r) => r,
        Err(_) => return HashSet::new(),
    };
    let reg: ObsidianRegistry = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(_) => return HashSet::new(),
    };
    reg.vaults
        .values()
        .filter(|v| v.open)
        .map(|v| {
            v.path
                .replace('\\', "/")
                .rsplit('/')
                .next()
                .unwrap_or(&v.path)
                .to_string()
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn open_vault_names() -> HashSet<String> {
    obsidian_windows().into_iter().map(|(_, v)| v).collect()
}

// ---------------------------------------------------------------- commands

#[tauri::command]
fn list_vaults(cfg: State<Config>, hist: State<FocusHistory>) -> Vec<VaultTile> {
    let open = open_vault_names();
    let hidden: HashSet<&String> = cfg.hide.iter().collect();
    let mut group_of: HashMap<&String, &String> = HashMap::new();
    for g in &cfg.groups {
        for v in &g.vaults {
            group_of.insert(v, &g.name);
        }
    }
    // Rank by whichever is newer: Obsidian's last-opened stamp or hvelf's
    // own last-focused record. Groups keep config order; recency within.
    let mut ranked: Vec<(VaultTile, u64)> = registered_vaults()
        .into_iter()
        .filter(|(name, _, _)| !hidden.contains(name))
        .map(|(name, path, ts)| {
            let eff = ts.max(hist.get(&name));
            (
                VaultTile {
                    open: open.contains(&name),
                    group: group_of.get(&name).map(|s| s.to_string()).unwrap_or_default(),
                    name,
                    path,
                },
                eff,
            )
        })
        .collect();
    let order: HashMap<&String, usize> =
        cfg.groups.iter().enumerate().map(|(i, g)| (&g.name, i)).collect();
    ranked.sort_by_key(|(t, eff)| {
        (
            order.get(&t.group).copied().unwrap_or(usize::MAX),
            std::cmp::Reverse(*eff),
        )
    });
    ranked.into_iter().map(|(t, _)| t).collect()
}

/// Close a vault's window gracefully (WM_CLOSE: same as clicking its X;
/// Obsidian saves state and releases the vault's renderer memory).
#[tauri::command]
#[allow(unused_variables)]
fn close_vault(name: String) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        for (h, v) in obsidian_windows() {
            if v == name {
                unsafe {
                    PostMessageW(h as _, WM_CLOSE, 0, 0);
                }
            }
        }
    }
}

fn do_launch(app: &AppHandle, cfg: &Config, name: &str) {
    hide_window(app.clone());
    // An already-open vault is focused by hvelf itself: as the recipient of
    // the user's hotkey or click, hvelf holds the foreground-change rights
    // that Windows denies to a background Obsidian asked via URI. The URI
    // path serves vaults with no window yet, and deep links which must
    // navigate inside the vault.
    if !cfg.deep_links.contains_key(name) {
        if let Some((h, _)) = obsidian_windows().into_iter().find(|(_, v)| v == name) {
            focus_window(h);
            return;
        }
    }
    let mut uri = format!("obsidian://open?vault={}", urlencoding::encode(name));
    if let Some(file) = cfg.deep_links.get(name) {
        uri.push_str(&format!("&file={}", urlencoding::encode(file)));
    }
    open_uri(&uri);
}

#[cfg(windows)]
fn focus_window(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };
    unsafe {
        if IsIconic(hwnd as _) != 0 {
            ShowWindow(hwnd as _, SW_RESTORE);
        }
        SetForegroundWindow(hwnd as _);
    }
}

#[cfg(not(windows))]
fn focus_window(_hwnd: isize) {}

/// Dispatch a URI through the OS. On Windows this must be ShellExecuteW:
/// `explorer.exe <uri>` looks like it should work but on current Windows 11
/// builds it opens a Documents folder instead of the protocol handler.
#[cfg(windows)]
fn open_uri(uri: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let op: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = uri.encode_utf16().chain(std::iter::once(0)).collect();
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    if r as isize <= 32 {
        eprintln!("hvelf: ShellExecuteW failed ({}) for {uri}", r as isize);
    }
}

#[cfg(target_os = "macos")]
fn open_uri(uri: &str) {
    if let Err(e) = std::process::Command::new("open").arg(uri).spawn() {
        eprintln!("hvelf: failed to launch {uri}: {e}");
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_uri(uri: &str) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(uri).spawn() {
        eprintln!("hvelf: failed to launch {uri}: {e}");
    }
}

fn most_recent_vault(cfg: &Config, hist: &FocusHistory) -> Option<String> {
    registered_vaults()
        .into_iter()
        .filter(|(name, _, _)| !cfg.hide.contains(name))
        .max_by_key(|(name, _, ts)| (*ts).max(hist.get(name)))
        .map(|(name, _, _)| name)
}

#[tauri::command]
fn launch(app: AppHandle, cfg: State<Config>, name: String) {
    do_launch(&app, cfg.inner(), &name);
}

// ------------------------------------------------------- global hotkeys
//
// Native RegisterHotKey instead of a hotkey library, for one reason: the
// key named "grave" must be the physical key under Esc on EVERY layout.
// Libraries map key names to virtual keys through a US table, which lands
// punctuation keys on the wrong physical key elsewhere (on UK, Backquote
// becomes the #~ key). Scancode 0x29 translated through the live layout
// gives the right virtual key everywhere.

#[cfg(windows)]
struct HotSpec {
    mods: u32,
    vk: u32,
}

#[cfg(windows)]
fn parse_hotkey(s: &str) -> Option<HotSpec> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MapVirtualKeyW, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
    };
    let mut mods = 0u32;
    let mut vk: Option<u32> = None;
    for tok in s.split('+') {
        let t = tok.trim().to_lowercase();
        match t.as_str() {
            "alt" => mods |= MOD_ALT,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" | "meta" => mods |= MOD_WIN,
            "grave" | "backquote" | "`" => {
                // Physical key under Esc, whatever the layout calls it.
                vk = Some(unsafe { MapVirtualKeyW(0x29, 1) });
            }
            key => {
                let key = key.strip_prefix("digit").unwrap_or(key);
                let key = key.strip_prefix("key").unwrap_or(key);
                if key.len() == 1 {
                    let c = key.chars().next().unwrap().to_ascii_uppercase();
                    if c.is_ascii_alphanumeric() {
                        vk = Some(c as u32);
                    }
                } else if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
                    if (1..=24).contains(&n) {
                        vk = Some(0x6F + n); // VK_F1 is 0x70
                    }
                }
            }
        }
    }
    vk.filter(|&v| v != 0).map(|vk| HotSpec { mods, vk })
}

#[cfg(windows)]
fn spawn_hotkeys(app: AppHandle, cfg: Config, hist: FocusHistory) {
    std::thread::spawn(move || unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetMessageW, SetTimer, MSG, WM_HOTKEY, WM_TIMER,
        };

        match parse_hotkey(&cfg.hotkey) {
            Some(h) => {
                if RegisterHotKey(std::ptr::null_mut(), 1, h.mods | MOD_NOREPEAT, h.vk) == 0 {
                    eprintln!("hvelf: hotkey '{}' is taken by another app", cfg.hotkey);
                }
            }
            None => eprintln!("hvelf: cannot parse hotkey '{}'", cfg.hotkey),
        }
        if !cfg.quick_launch.is_empty() {
            match parse_hotkey(&cfg.quick_launch) {
                Some(h) => {
                    if RegisterHotKey(std::ptr::null_mut(), 2, h.mods | MOD_NOREPEAT, h.vk) == 0 {
                        eprintln!("hvelf: quickLaunch '{}' is taken by another app", cfg.quick_launch);
                    }
                }
                None => eprintln!("hvelf: cannot parse quickLaunch '{}'", cfg.quick_launch),
            }
        }

        // Sample the foreground window every 2s to learn which vault the
        // user is actually in; WM_TIMER arrives on this thread's queue.
        SetTimer(std::ptr::null_mut(), 1, 2000, None);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            match msg.message {
                WM_HOTKEY => match msg.wParam {
                    1 => toggle_window(&app),
                    2 => {
                        if let Some(name) = most_recent_vault(&cfg, &hist) {
                            do_launch(&app, &cfg, &name);
                        }
                    }
                    _ => {}
                },
                WM_TIMER => {
                    if let Some(v) = foreground_vault() {
                        hist.note(v);
                    }
                }
                _ => {}
            }
        }
    });
}

/// macOS registers its chords on the main thread, against the application's
/// own event target, so there is no second thread and no message pump here.
/// `setup` is that thread, which is why this is called from there.
#[cfg(target_os = "macos")]
fn spawn_hotkeys(app: AppHandle, cfg: Config, hist: FocusHistory) {
    hotkey_macos::install(app, cfg, hist);
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn spawn_hotkeys(_app: AppHandle, _cfg: Config, _hist: FocusHistory) {
    eprintln!("hvelf: global hotkeys are not implemented on this platform");
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

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let show = MenuItem::with_id(app, "show", "Show board", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit hvelf", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    TrayIconBuilder::with_id("hvelf")
        .icon(app.default_window_icon().expect("no window icon").clone())
        .tooltip("hvelf")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "show" => toggle_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    let cfg = load_or_seed_config();
    let hide_on_blur = cfg.hide_on_blur;

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            toggle_window(app);
        }))
        .manage(cfg)
        .manage(FocusHistory::default())
        .setup(|app| {
            build_tray(app)?;
            let cfg = app.state::<Config>().inner().clone();
            let hist = app.state::<FocusHistory>().inner().clone();
            spawn_hotkeys(app.handle().clone(), cfg, hist);
            Ok(())
        })
        .on_window_event(move |window, event| {
            if hide_on_blur {
                if let tauri::WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_vaults,
            launch,
            close_vault,
            hide_window,
            quit
        ])
        .run(tauri::generate_context!())
        .expect("hvelf: failed to start");
}
