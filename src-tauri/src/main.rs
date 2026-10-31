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

/// Does a config entry name this vault? Entries are vault names, as they
/// always have been; a full path also matches, which is the only way to name
/// one of two vaults that share a name.
fn entry_matches(entry: &str, v: &Vault) -> bool {
    entry == v.name || norm_path(entry) == norm_path(&v.path)
}

fn norm_path(p: &str) -> String {
    p.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

impl Config {
    fn hidden(&self, v: &Vault) -> bool {
        self.hide.iter().any(|e| entry_matches(e, v))
    }
    /// The vault's group: where it sits on the board, and what the heading
    /// says. Vaults in no group fall into a trailing one.
    fn group_of(&self, v: &Vault) -> (usize, String) {
        self.groups
            .iter()
            .enumerate()
            .find(|(_, g)| g.vaults.iter().any(|e| entry_matches(e, v)))
            .map(|(i, g)| (i, g.name.clone()))
            .unwrap_or((usize::MAX, String::new()))
    }
    fn deep_link(&self, v: &Vault) -> Option<&String> {
        self.deep_links
            .iter()
            .find(|(k, _)| entry_matches(k, v))
            .map(|(_, file)| file)
    }
}

/// The config as the running app holds it: read once at startup, and changed
/// after that only by the board hiding a vault. The hotkey handlers hold the
/// same copy, so a vault taken off the board is out of quick launch too.
#[derive(Clone)]
struct SharedConfig(std::sync::Arc<std::sync::Mutex<Config>>);

impl SharedConfig {
    fn get(&self) -> Config {
        self.0.lock().unwrap().clone()
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

/// Change `hide` in config.json. The file is edited as the JSON it is rather
/// than written back from Config, so the user's key order survives, and so
/// does any key this build does not know.
fn edit_hide(path: &Path, edit: impl FnOnce(&mut Vec<serde_json::Value>)) -> Result<(), String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("cannot read config.json: {e}"))?;
    let mut doc: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("config.json invalid ({e})"))?;
    edit(
        doc.as_object_mut()
            .ok_or("config.json is not an object")?
            .entry("hide")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or("`hide` in config.json is not a list")?,
    );
    // Written beside the file and renamed over it, since a half-written
    // config would cost the user their hotkeys and groups.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&doc).unwrap())
        .and_then(|_| fs::rename(&tmp, path))
        .map_err(|e| format!("cannot write config.json: {e}"))
}

fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

// ---------------------------------------------------------------- vaults

#[derive(Serialize, Clone)]
struct VaultTile {
    /// Obsidian's own id for the vault, and the handle every command takes:
    /// see Vault for why the name will not do.
    id: String,
    /// The vault name, as Obsidian shows it: its folder.
    name: String,
    /// What separates this tile from a namesake, empty when it has none;
    /// see qualifiers.
    qualifier: String,
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
    /// Whether Obsidian has a window open on this vault; see Vault.
    #[serde(default)]
    open: bool,
}

/// One vault as Obsidian registers it.
///
/// `id` is the key in obsidian.json and the only unique handle hvelf has. A
/// vault's name is nothing more than the basename of its folder, so two
/// registered vaults can and do share one, and Obsidian resolves a name in
/// registry order, arbitrarily from here. Everything that acts on a vault
/// therefore travels by id.
#[derive(Clone)]
struct Vault {
    id: String,
    name: String,
    path: String,
    ts: u64,
    /// Obsidian's own record of whether this vault has a window open. It
    /// keeps the flag per vault rather than only for the newest, and clears
    /// stale ones when it starts. On macOS this is where open state comes
    /// from; everywhere it is what tells same-named vaults apart.
    open: bool,
}

fn basename(path: &str) -> String {
    path.replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Obsidian matches vault names case-insensitively when it resolves a URI,
/// so two folders differing only in case are one name to it and to hvelf.
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Is another registered vault called the same thing?
fn shares_name(vaults: &[Vault], v: &Vault) -> bool {
    vaults
        .iter()
        .any(|o| o.id != v.id && same_name(&o.name, &v.name))
}

/// What each tile says under its name, in the order given, and nothing at
/// all for a name that is the only one of its kind. Two vaults with the same
/// name make two identical tiles, which is no use to anyone: the parent
/// folder is what separates them on sight, and the whole path when even that
/// repeats.
fn qualifiers(vaults: &[&Vault]) -> Vec<String> {
    let mut out: Vec<String> = vaults
        .iter()
        .map(|v| {
            let alone = vaults
                .iter()
                .filter(|o| same_name(&o.name, &v.name))
                .count()
                < 2;
            match (alone, parent(&v.path)) {
                (true, _) => String::new(),
                (false, Some(p)) => p,
                (false, None) => v.path.clone(),
            }
        })
        .collect();
    let repeated: Vec<bool> = vaults
        .iter()
        .enumerate()
        .map(|(i, v)| {
            !out[i].is_empty()
                && vaults.iter().enumerate().any(|(j, o)| {
                    j != i && out[j] == out[i] && same_name(&o.name, &v.name)
                })
        })
        .collect();
    for (i, r) in repeated.iter().enumerate() {
        if *r {
            out[i] = vaults[i].path.clone();
        }
    }
    out
}

fn parent(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let path = path.trim_end_matches('/');
    let up = &path[..path.rfind('/')?];
    let name = up.rsplit('/').next().unwrap_or(up);
    (!name.is_empty()).then(|| name.to_string())
}

fn registered_vaults() -> Vec<Vault> {
    let raw = match fs::read_to_string(obsidian_json_path()) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let reg: ObsidianRegistry = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    reg.vaults
        .into_iter()
        .map(|(id, v)| Vault {
            id,
            name: basename(&v.path),
            path: v.path,
            ts: v.ts,
            open: v.open,
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

/// Which vaults are open, for the indicator on each tile, by id.
///
/// macOS has no free equivalent of the window list: both routes to another
/// app's window titles are permission-gated, the accessibility API and
/// CGWindowList's window names. Obsidian, though, already records the answer
/// in its own registry and keeps a flag per vault rather than only for the
/// newest, so the indicator costs nothing here: no permission, no prompt, no
/// window enumeration.
///
/// The flag is written when a vault opens or closes, so it can lag a crash
/// that leaves it set. It is the indicator that is briefly wrong, which is
/// the cheapest thing in the app to be wrong.
#[cfg(target_os = "macos")]
fn open_ids(vaults: &[Vault]) -> HashSet<String> {
    vaults
        .iter()
        .filter(|v| v.open)
        .map(|v| v.id.clone())
        .collect()
}

/// Windows reads open state off the live window list, which is exact and
/// also yields the handle needed to focus or close a window. A title carries
/// the vault's name and not its id, though, so when two vaults share a name
/// the list alone cannot say which of them is open: Obsidian's own per-id
/// flag breaks that tie. A window still has to exist either way, which is
/// what keeps a flag left set by a crash from lighting a dot.
#[cfg(not(target_os = "macos"))]
fn open_ids(vaults: &[Vault]) -> HashSet<String> {
    let titled: Vec<String> = obsidian_windows().into_iter().map(|(_, v)| v).collect();
    vaults
        .iter()
        .filter(|v| titled.iter().any(|t| same_name(t, &v.name)))
        .filter(|v| v.open || !shares_name(vaults, v))
        .map(|v| v.id.clone())
        .collect()
}

/// The window belonging to one vault, when it can be told apart. Titles read
/// "<note> - <vault> - Obsidian <version>", so for two same-named vaults
/// only Obsidian's per-id flag says which window is whose; when both are
/// open nothing does, and hvelf would rather leave a window alone than raise
/// or close the wrong one.
fn window_for(vaults: &[Vault], v: &Vault) -> Option<isize> {
    let wins: Vec<isize> = obsidian_windows()
        .into_iter()
        .filter(|(_, t)| same_name(t, &v.name))
        .map(|(h, _)| h)
        .collect();
    if !shares_name(vaults, v) {
        return wins.first().copied();
    }
    let sibling_open = vaults
        .iter()
        .any(|o| o.id != v.id && same_name(&o.name, &v.name) && o.open);
    match wins.as_slice() {
        [only] if v.open && !sibling_open => Some(*only),
        _ => None,
    }
}

// ---------------------------------------------------------------- commands

#[tauri::command]
fn list_vaults(cfg: State<SharedConfig>, hist: State<FocusHistory>) -> Vec<VaultTile> {
    let cfg = cfg.get();
    // Open state is read against the whole registry, since a hidden vault
    // still has a window and its title is indistinguishable from its
    // namesake's; labels are only about what the board shows.
    let all = registered_vaults();
    let open = open_ids(&all);
    let shown: Vec<&Vault> = all.iter().filter(|v| !cfg.hidden(v)).collect();
    // Rank by whichever is newer: Obsidian's last-opened stamp or hvelf's
    // own last-focused record. Groups keep config order; recency within.
    let mut ranked: Vec<(usize, u64, VaultTile)> = shown
        .iter()
        .zip(qualifiers(&shown))
        .map(|(v, qualifier)| {
            let (pos, group) = cfg.group_of(v);
            let eff = v.ts.max(focused_ts(&hist, &all, v));
            (
                pos,
                eff,
                VaultTile {
                    id: v.id.clone(),
                    name: v.name.clone(),
                    qualifier,
                    path: v.path.clone(),
                    open: open.contains(&v.id),
                    group,
                },
            )
        })
        .collect();
    ranked.sort_by_key(|(pos, eff, _)| (*pos, std::cmp::Reverse(*eff)));
    ranked.into_iter().map(|(_, _, t)| t).collect()
}

/// Close a vault's window gracefully (WM_CLOSE: same as clicking its X;
/// Obsidian saves state and releases the vault's renderer memory).
#[tauri::command]
#[allow(unused_variables)]
fn close_vault(id: String) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        let all = registered_vaults();
        if let Some(h) = all
            .iter()
            .find(|v| v.id == id)
            .and_then(|v| window_for(&all, v))
        {
            unsafe {
                PostMessageW(h as _, WM_CLOSE, 0, 0);
            }
        }
    }
}

/// Take a vault off the board for good. Its path goes into `hide`, in the
/// running config and in config.json: the path rather than the name, so a
/// namesake keeps its tile. Obsidian's registry is left alone, and Hidden
/// vaults in the tray menu brings the tile back.
#[tauri::command]
fn hide_vault(app: AppHandle, cfg: State<SharedConfig>, id: String) -> Result<(), String> {
    let all = registered_vaults();
    let v = all.iter().find(|v| v.id == id).ok_or("no such vault")?;
    {
        let mut cfg = cfg.0.lock().unwrap();
        if cfg.hidden(v) {
            return Ok(());
        }
        edit_hide(&config_file(), |h| h.push(v.path.clone().into()))?;
        cfg.hide.push(v.path.clone());
    }
    refresh_tray(&app);
    Ok(())
}

/// Put a `hide` entry's vaults back on the board, from the tray. The board
/// re-reads its tiles when summoned, so nothing else needs telling.
fn unhide(app: &AppHandle, entry: &str) {
    {
        let mut cfg = app.state::<SharedConfig>().inner().0.lock().unwrap();
        match edit_hide(&config_file(), |h| h.retain(|e| e.as_str() != Some(entry))) {
            Ok(()) => cfg.hide.retain(|e| e != entry),
            Err(e) => eprintln!("hvelf: {e}"),
        }
    }
    refresh_tray(app);
}

/// The Hidden vaults menu, as (entry, label) pairs sorted by label: one per
/// `hide` entry, named the way its tile was. The parent folder is added
/// where the name alone would not say which vault, and an entry Obsidian no
/// longer knows says so, since putting it back will show nothing.
fn hidden_menu(hide: &[String], all: &[Vault]) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let entries: Vec<&String> = hide.iter().filter(|e| seen.insert(e.as_str())).collect();
    let name_of = |e: &str| {
        all.iter()
            .find(|v| entry_matches(e, v))
            .map_or_else(|| basename(e), |v| v.name.clone())
    };
    let mut items: Vec<(String, String)> = entries
        .iter()
        .map(|e| {
            let name = name_of(e);
            let known = all.iter().any(|v| entry_matches(e, v));
            let namesake = all.iter().any(|v| same_name(&v.name, &name) && !entry_matches(e, v))
                || entries.iter().any(|o| o != e && same_name(&name_of(o), &name));
            let mut notes = Vec::new();
            if namesake {
                notes.extend(parent(e));
            }
            if !known {
                notes.push("not in Obsidian".to_string());
            }
            let label = if notes.is_empty() { name } else { format!("{name} ({})", notes.join(", ")) };
            (e.to_string(), label)
        })
        .collect();
    items.sort_by_key(|(_, label)| label.to_lowercase());
    items
}

fn do_launch(app: &AppHandle, cfg: &Config, id: &str) {
    hide_window(app.clone());
    let all = registered_vaults();
    let v = match all.iter().find(|v| v.id == id) {
        Some(v) => v,
        None => return,
    };
    // An already-open vault is focused by hvelf itself: as the recipient of
    // the user's hotkey or click, hvelf holds the foreground-change rights
    // that Windows denies to a background Obsidian asked via URI. The URI
    // path serves vaults with no window yet, and deep links which must
    // navigate inside the vault.
    if cfg.deep_link(v).is_none() {
        if let Some(h) = window_for(&all, v) {
            focus_window(h);
            return;
        }
    }
    // By id, not by name: Obsidian's resolver takes either, checks the id
    // first, and would otherwise pick whichever same-named vault its
    // registry happens to list first.
    let mut uri = format!("obsidian://open?vault={}", urlencoding::encode(&v.id));
    if let Some(file) = cfg.deep_link(v) {
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

/// hvelf's own last-focused stamp for a vault. The stamp is filed under the
/// name in the window title, which two vaults can share, so it only counts
/// for one Obsidian records as open: a shut vault cannot inherit the recency
/// of the namesake being worked in.
fn focused_ts(hist: &FocusHistory, vaults: &[Vault], v: &Vault) -> u64 {
    if shares_name(vaults, v) && !v.open {
        return 0;
    }
    hist.get(&v.name)
}

fn most_recent_vault(cfg: &Config, hist: &FocusHistory) -> Option<String> {
    let all = registered_vaults();
    all.iter()
        .filter(|v| !cfg.hidden(v))
        .max_by_key(|v| v.ts.max(focused_ts(hist, &all, v)))
        .map(|v| v.id.clone())
}

#[tauri::command]
fn launch(app: AppHandle, cfg: State<SharedConfig>, id: String) {
    do_launch(&app, &cfg.get(), &id);
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
    let all = registered_vaults();
    let mut ranked: Vec<(String, u64)> = all
        .iter()
        .filter(|v| vaults.contains(&v.name) && !cfg.hidden(v))
        .map(|v| (v.name.clone(), v.ts.max(focused_ts(hist, &all, v))))
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
fn spawn_hotkeys(app: AppHandle, cfg: SharedConfig, hist: FocusHistory, keep: KeepState) {
    std::thread::spawn(move || unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetMessageW, SetTimer, MSG, WM_HOTKEY, WM_TIMER,
        };

        // This thread does the UI Automation calls, so it owns a COM
        // apartment; the message loop below is its pump.
        capture::init_com();

        // The chords are bound once, from the config as it stood at startup.
        let boot = cfg.get();
        match parse_hotkey(&boot.hotkey) {
            Some(h) => {
                if RegisterHotKey(std::ptr::null_mut(), 1, h.mods | MOD_NOREPEAT, h.vk) == 0 {
                    eprintln!("hvelf: hotkey '{}' is taken by another app", boot.hotkey);
                }
            }
            None => eprintln!("hvelf: cannot parse hotkey '{}'", boot.hotkey),
        }
        if !boot.quick_launch.is_empty() {
            match parse_hotkey(&boot.quick_launch) {
                Some(h) => {
                    if RegisterHotKey(std::ptr::null_mut(), 2, h.mods | MOD_NOREPEAT, h.vk) == 0 {
                        eprintln!("hvelf: quickLaunch '{}' is taken by another app", boot.quick_launch);
                    }
                }
                None => eprintln!("hvelf: cannot parse quickLaunch '{}'", boot.quick_launch),
            }
        }
        if !boot.keep.is_empty() {
            match parse_hotkey(&boot.keep) {
                Some(h) => {
                    if RegisterHotKey(std::ptr::null_mut(), 3, h.mods | MOD_NOREPEAT, h.vk) == 0 {
                        eprintln!("hvelf: keep '{}' is taken by another app", boot.keep);
                    }
                }
                None => eprintln!("hvelf: cannot parse keep '{}'", boot.keep),
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
                        let cfg = cfg.get();
                        if let Some(id) = most_recent_vault(&cfg, &hist) {
                            do_launch(&app, &cfg, &id);
                        }
                    }
                    3 => start_keep(&app, &cfg.get(), &hist, &keep),
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
fn spawn_hotkeys(_app: AppHandle, _cfg: SharedConfig, _hist: FocusHistory, _keep: KeepState) {
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

/// Menu ids of the Hidden vaults items: this prefix, then the `hide` entry.
const UNHIDE: &str = "unhide:";

fn tray_menu<M: Manager<tauri::Wry>>(app: &M) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

    let cfg = app.state::<SharedConfig>().get();
    let entries = hidden_menu(&cfg.hide, &registered_vaults());
    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = Vec::new();
    if entries.is_empty() {
        items.push(Box::new(MenuItem::new(app, "Nothing is hidden", false, None::<&str>)?));
    } else {
        items.push(Box::new(MenuItem::new(app, "Click one to put it back", false, None::<&str>)?));
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
        for (entry, label) in entries {
            items.push(Box::new(MenuItem::with_id(app, format!("{UNHIDE}{entry}"), label, true, None::<&str>)?));
        }
    }
    let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = items.iter().map(|i| i.as_ref()).collect();
    let hidden = Submenu::with_items(app, "Hidden vaults", true, &refs)?;
    let show = MenuItem::with_id(app, "show", "Show board", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit hvelf", true, None::<&str>)?;
    Menu::with_items(app, &[&show, &hidden, &quit])
}

/// Rebuild the tray menu after `hide` changes. No tray, nothing to do.
fn refresh_tray(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id("hvelf") {
        match tray_menu(app) {
            Ok(menu) => {
                let _ = tray.set_menu(Some(menu));
            }
            Err(e) => eprintln!("hvelf: tray menu: {e}"),
        }
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    TrayIconBuilder::with_id("hvelf")
        .icon(app.default_window_icon().expect("no window icon").clone())
        .tooltip("hvelf")
        .menu(&tray_menu(app)?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "show" => toggle_window(app),
            "quit" => app.exit(0),
            id => {
                if let Some(entry) = id.strip_prefix(UNHIDE) {
                    unhide(app, entry);
                }
            }
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
        .manage(SharedConfig(std::sync::Arc::new(std::sync::Mutex::new(cfg))))
        .manage(FocusHistory::default())
        .manage(KeepState::default())
        .setup(|app| {
            build_tray(app)?;
            build_toast(app)?;
            let cfg = app.state::<SharedConfig>().inner().clone();
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
            hide_vault,
            hide_window,
            quit,
            keep_pending,
            keep_file
        ])
        .run(tauri::generate_context!())
        .expect("hvelf: failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(id: &str, path: &str, open: bool) -> Vault {
        Vault { id: id.into(), name: basename(path), path: path.into(), ts: 0, open }
    }

    #[test]
    fn putting_back_removes_only_that_entry_and_keeps_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("hvelf-unhide-{} é 'q'", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"zeta":1,"hide":["/v/a","/v/b","/v/a"],"hotkey":"alt+grave"}"#).unwrap();
        edit_hide(&path, |h| h.retain(|e| e.as_str() != Some("/v/a"))).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["hide"], serde_json::json!(["/v/b"]));
        let keys: Vec<String> = doc.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["zeta", "hide", "hotkey"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_menu_names_entries_the_way_their_tiles_were() {
        let all = vec![
            vault("1", "/v/research/admin", true),
            vault("2", "/v/research/notes", false),
            vault("3", "/v/archive/notes", false),
        ];
        let hide: Vec<String> = ["/v/research/admin", "/v/archive/notes", "/gone/Index", "/v/research/admin"]
            .map(String::from)
            .into();
        let m = hidden_menu(&hide, &all);
        let labels: Vec<&str> = m.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels, ["admin", "Index (not in Obsidian)", "notes (archive)"]);
        assert_eq!(m[2].0, "/v/archive/notes");
        // A bare name hides every vault of that name, and is listed as one.
        let m = hidden_menu(&["notes".to_string()], &all);
        assert_eq!(m, [("notes".to_string(), "notes".to_string())]);
    }
}
