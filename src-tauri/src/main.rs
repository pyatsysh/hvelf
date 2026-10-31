// hvelf: hotkey-summoned tile board for launching Obsidian vaults.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(windows)]
mod capture;
mod keep;

// ---------------------------------------------------------------- config

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Config {
    hotkey: String,
    /// Optional second hotkey that opens the most recent vault directly,
    /// without showing the board. Empty string disables it.
    quick_launch: String,
    /// Hotkey that files the text selected in the foreground app into a
    /// vault's keeps folder. Empty string disables it.
    keep: String,
    /// exocortex.toml, which names the vault root and the fallback vault.
    exocortex: String,
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
            keep: "alt+k".into(),
            exocortex: keep::DEFAULT_REGISTRY.into(),
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

// ------------------------------------------------------------------ keep
//
// The keep chord captures whatever text is selected in the foreground app
// and files it verbatim as a note under <vault>/keeps/. The board then
// opens in keep mode to ask which vault: Enter or the chord again takes
// the preselected one, a tile key picks another, Esc parks the note in the
// fallback vault's unrouted folder. A capture is never dropped.

/// A capture waiting on the board for a vault.
struct Pending {
    body: String,
    id: String,
    ts: String,
    month: String,
    app: String,
    title: String,
    url: Option<String>,
    /// Preselected vault: from the window title, else the last one used.
    vault: String,
    /// Vault tiles in board order, preselected first.
    vaults: Vec<String>,
    exo: keep::Exocortex,
    /// Window to give the focus back to once the note is filed.
    source: isize,
}

#[derive(Clone, Default)]
struct KeepState(Arc<Mutex<Option<Pending>>>);

enum Route {
    Preselected,
    Vault(String),
    Unrouted,
}

/// Small things remembered across runs, next to config.json.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Persisted {
    last_keep_vault: String,
}

fn load_persisted() -> Persisted {
    fs::read_to_string(config_dir().join("state.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_persisted(p: &Persisted) {
    let _ = fs::create_dir_all(config_dir());
    let _ = fs::write(
        config_dir().join("state.json"),
        serde_json::to_string_pretty(p).unwrap_or_default(),
    );
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct KeepInfo {
    preview: String,
    chars: usize,
    vault: String,
    vaults: Vec<String>,
    app: String,
}

#[tauri::command]
fn keep_pending(keep: State<KeepState>) -> Option<KeepInfo> {
    keep.0.lock().unwrap().as_ref().map(|p| KeepInfo {
        preview: keep::preview(&p.body, 60),
        chars: p.body.chars().count(),
        vault: p.vault.clone(),
        vaults: p.vaults.clone(),
        app: p.app.clone(),
    })
}

/// From the board: a named vault, or none for the unrouted folder.
#[tauri::command]
fn keep_file(app: AppHandle, keep: State<KeepState>, vault: Option<String>) {
    let route = match vault {
        Some(v) => Route::Vault(v),
        None => Route::Unrouted,
    };
    file_pending(&app, keep.inner(), route, true);
}

fn file_pending(app: &AppHandle, keep: &KeepState, route: Route, restore_focus: bool) {
    let Some(p) = keep.0.lock().unwrap().take() else {
        return;
    };
    let chosen: Option<String> = match route {
        Route::Preselected => Some(p.vault.clone()),
        Route::Vault(v) => Some(v),
        Route::Unrouted => None,
    };
    let unrouted = keep::unrouted_path(&p.exo.root, &p.exo.fallback, &p.id);
    let path = match &chosen {
        Some(v) => keep::keep_path(&p.exo.root, v, &p.month, &p.id),
        None => unrouted.clone(),
    };
    let meta = keep::Meta {
        id: p.id.clone(),
        ts: p.ts.clone(),
        vault: chosen.clone().unwrap_or_else(|| "unrouted".into()),
        app: p.app.clone(),
        title: p.title.clone(),
        url: p.url.clone(),
    };
    let bytes = keep::render(&meta, &p.body);
    let mut label = chosen.clone().unwrap_or_else(|| "unrouted".into());
    if let Err(e) = keep::write_atomic(&path, &bytes) {
        eprintln!("hvelf: keep write failed at {}: {e}", path.display());
        // Never drop a capture: the unrouted folder is the last resort, and
        // if even that fails the toast says so and the text is in stderr.
        if chosen.is_some() && keep::write_atomic(&unrouted, &bytes).is_ok() {
            label = format!("unrouted, {} refused: {e}", chosen.clone().unwrap());
        } else {
            eprintln!("hvelf: keep lost, body follows\n{}", p.body);
            hide_window(app.clone());
            if restore_focus {
                focus_window(p.source);
            }
            toast(app, &format!("keep FAILED: {e}"));
            return;
        }
    }
    if let Some(v) = &chosen {
        save_persisted(&Persisted {
            last_keep_vault: v.clone(),
        });
    }
    hide_window(app.clone());
    if restore_focus {
        focus_window(p.source);
    }
    toast(app, &format!("kept \u{2192} {label}"));
}

/// Vaults as tiles: the preselected one first, then the rest in the
/// board's own recency order, then any the board does not know about.
#[cfg_attr(not(windows), allow(dead_code))]
fn order_for_board(vaults: &[String], pre: &str, cfg: &Config, hist: &FocusHistory) -> Vec<String> {
    let mut ranked: Vec<(String, u64)> = registered_vaults()
        .into_iter()
        .filter(|(name, _, _)| vaults.contains(name) && !cfg.hide.contains(name))
        .map(|(name, _, ts)| {
            let eff = ts.max(hist.get(&name));
            (name, eff)
        })
        .collect();
    ranked.sort_by_key(|(_, eff)| std::cmp::Reverse(*eff));
    let mut out: Vec<String> = vec![pre.to_string()];
    for (name, _) in ranked {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    for name in vaults {
        if !out.contains(name) {
            out.push(name.clone());
        }
    }
    out
}

#[cfg_attr(not(windows), allow(dead_code))]
fn rand32() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    h.finish() as u32
}

/// The chord. Runs on the hotkey thread, which holds the COM apartment.
#[cfg(windows)]
fn start_keep(app: &AppHandle, cfg: &Config, hist: &FocusHistory, keep: &KeepState) {
    // Chord again while a capture waits on the board: file it where the
    // banner says.
    if keep.0.lock().unwrap().is_some() {
        file_pending(app, keep, Route::Preselected, true);
        return;
    }
    let Some(src) = capture::snapshot() else {
        toast(app, "keep: no foreground window");
        return;
    };
    if src.pid == std::process::id() {
        hide_window(app.clone());
        return;
    }
    let now = chrono::Local::now();
    let text = capture::selection_uia(src.hwnd).or_else(|| {
        if capture::is_terminal(&src.exe) {
            None
        } else {
            capture::selection_clipboard()
        }
    });
    let Some(body) = text else {
        toast(app, "keep: nothing selected");
        return;
    };
    let url = if capture::is_browser(&src.exe) {
        capture::browser_url(src.hwnd)
    } else {
        None
    };
    let exo = keep::load_exocortex(Path::new(&cfg.exocortex));
    let vaults = keep::list_vaults(&exo.root);
    if vaults.is_empty() {
        eprintln!("hvelf: no vaults under {}", exo.root.display());
    }
    let last = load_persisted().last_keep_vault;
    let pre = keep::preselect(&src.title, &vaults)
        .or_else(|| vaults.contains(&last).then(|| last.clone()))
        .unwrap_or_else(|| exo.fallback.clone());
    let ordered = order_for_board(&vaults, &pre, cfg, hist);
    let pending = Pending {
        body,
        id: keep::new_id(&now.format("%Y%m%dT%H%M%S").to_string(), rand32()),
        ts: now.format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        month: now.format("%Y-%m").to_string(),
        app: src.exe.clone(),
        title: src.title.clone(),
        url,
        vault: pre,
        vaults: ordered,
        exo,
        source: src.hwnd,
    };
    *keep.0.lock().unwrap() = Some(pending);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.emit("keep", ());
    }
}

// ----------------------------------------------------------------- toast
//
// A small always-on-top window that cannot take focus (WS_EX_NOACTIVATE
// through tauri's focusable=false), so it can appear over the app the
// user is working in without interrupting it.

static TOAST_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn toast(app: &AppHandle, text: &str) {
    use std::sync::atomic::Ordering;
    eprintln!("hvelf: {text}");
    let Some(w) = app.get_webview_window("toast") else {
        return;
    };
    let _ = w.eval(&format!("show({})", serde_json::to_string(text).unwrap_or_default()));
    let _ = w.show();
    let gen = TOAST_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(2000));
        if TOAST_GEN.load(Ordering::SeqCst) == gen {
            let _ = w.hide();
        }
    });
}

fn build_toast(app: &tauri::App) -> tauri::Result<()> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    let (tw, th) = (380.0, 52.0);
    let mut b = WebviewWindowBuilder::new(app, "toast", WebviewUrl::App("toast.html".into()))
        .title("hvelf")
        .inner_size(tw, th)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focusable(false)
        .visible(false);
    // Bottom right of the primary monitor, clear of the taskbar.
    if let Ok(Some(m)) = app.primary_monitor() {
        let s = m.scale_factor();
        let (pos, size) = (m.position(), m.size());
        let x = pos.x as f64 / s + size.width as f64 / s - tw - 24.0;
        let y = pos.y as f64 / s + size.height as f64 / s - th - 72.0;
        b = b.position(x, y);
    }
    b.build()?;
    Ok(())
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
fn spawn_hotkeys(app: AppHandle, cfg: Config, hist: FocusHistory, keep: KeepState) {
    std::thread::spawn(move || unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetMessageW, SetTimer, MSG, WM_HOTKEY, WM_TIMER,
        };

        // This thread does the UI Automation calls, so it owns a COM
        // apartment; the message loop below is its pump.
        capture::init_com();

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
        if !cfg.keep.is_empty() {
            match parse_hotkey(&cfg.keep) {
                Some(h) => {
                    if RegisterHotKey(std::ptr::null_mut(), 3, h.mods | MOD_NOREPEAT, h.vk) == 0 {
                        eprintln!("hvelf: keep '{}' is taken by another app", cfg.keep);
                    }
                }
                None => eprintln!("hvelf: cannot parse keep '{}'", cfg.keep),
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
                    3 => start_keep(&app, &cfg, &hist, &keep),
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

#[cfg(not(windows))]
fn spawn_hotkeys(_app: AppHandle, _cfg: Config, _hist: FocusHistory, _keep: KeepState) {
    eprintln!("hvelf: global hotkeys are Windows-only for now");
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
        .manage(KeepState::default())
        .setup(|app| {
            build_tray(app)?;
            build_toast(app)?;
            let cfg = app.state::<Config>().inner().clone();
            let hist = app.state::<FocusHistory>().inner().clone();
            let keep = app.state::<KeepState>().inner().clone();
            spawn_hotkeys(app.handle().clone(), cfg, hist, keep);
            Ok(())
        })
        .on_window_event(move |window, event| {
            if window.label() != "main" || !hide_on_blur {
                return;
            }
            if let tauri::WindowEvent::Focused(false) = event {
                // A capture waiting on the board is never dropped: losing
                // the board files it unrouted, to be decided later.
                let keep = window.state::<KeepState>().inner().clone();
                if keep.0.lock().unwrap().is_some() {
                    file_pending(window.app_handle(), &keep, Route::Unrouted, false);
                } else {
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_vaults,
            launch,
            close_vault,
            hide_window,
            quit,
            keep_pending,
            keep_file
        ])
        .run(tauri::generate_context!())
        .expect("hvelf: failed to start");
}
