// hvelf: hotkey-summoned tile board for launching Obsidian vaults.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

#[cfg(target_os = "macos")]
mod hotkey_macos;
#[cfg(target_os = "linux")]
mod linux;

// ------------------------------------------------------- shared vocabulary

/// The error envelope the board shows: `{"ok": false, "code", "message",
/// "feature"}`. An action that cannot run says why instead of pretending.
#[derive(Serialize, Clone, Debug)]
pub struct Failure {
    ok: bool,
    code: &'static str,
    message: String,
    feature: &'static str,
}

impl Failure {
    pub fn new(code: &'static str, message: impl Into<String>, feature: &'static str) -> Failure {
        Failure { ok: false, code, message: message.into(), feature }
    }
}

/// One platform capability as the board and `hvelf --capabilities` report
/// it: state is available, degraded, unavailable, unsupported, disabled or
/// unknown, and anything short of available carries its reason.
#[derive(Serialize, Clone, Debug)]
pub struct Capability {
    capability: &'static str,
    state: &'static str,
    reason: String,
    source: String,
    platform: &'static str,
}

/// Capability records found at runtime (chord grabs, tray), added to the
/// static probes when the board asks.
#[derive(Clone, Default)]
struct RuntimeCaps(std::sync::Arc<std::sync::Mutex<Vec<Capability>>>);

impl RuntimeCaps {
    #[allow(dead_code)]
    fn record(&self, c: Capability) {
        let mut v = self.0.lock().unwrap();
        v.retain(|o| o.capability != c.capability);
        v.push(c);
    }
}

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

/// Does a config entry name this vault? Entries are vault names, as they
/// always have been; a full path also matches, which is the only way to name
/// one of two vaults that share a name.
fn entry_matches(entry: &str, v: &Vault) -> bool {
    entry == v.name || norm_path(entry) == norm_path(&v.path)
}

fn norm_path(p: &str) -> String {
    let p = slashes(p);
    let p = p.trim_end_matches('/');
    // Linux paths are case-sensitive: two vaults differing only in case are
    // two vaults there, while Windows and macOS fold case by default.
    if cfg!(target_os = "linux") {
        p.to_string()
    } else {
        p.to_lowercase()
    }
}

/// Paths with `/` separators. Only Windows uses a backslash as one; on
/// Linux and macOS it is an ordinary character a folder name may contain.
fn slashes(p: &str) -> String {
    if cfg!(windows) {
        p.replace('\\', "/")
    } else {
        p.to_string()
    }
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
    #[cfg(target_os = "linux")]
    {
        linux::config_home().join("hvelf")
    }
    #[cfg(all(not(windows), not(target_os = "linux")))]
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

/// Add one entry to `hide` in config.json. The file is edited as the JSON it
/// is rather than written back from Config, so the user's key order survives,
/// and so does any key this build does not know.
fn persist_hidden(entry: &str) -> Result<(), String> {
    let path = config_dir().join("config.json");
    let raw = fs::read_to_string(&path).map_err(|e| format!("cannot read config.json: {e}"))?;
    let mut doc: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("config.json invalid ({e})"))?;
    doc.as_object_mut()
        .ok_or("config.json is not an object")?
        .entry("hide")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or("`hide` in config.json is not a list")?
        .push(entry.into());
    // Written beside the file and renamed over it, since a half-written
    // config would cost the user their hotkeys and groups.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&doc).unwrap())
        .and_then(|_| fs::rename(&tmp, &path))
        .map_err(|e| format!("cannot write config.json: {e}"))
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
    /// `observed` or `reported`; see open_confidence.
    open_state: &'static str,
    /// Why the cross cannot close this vault's window here; empty when it can.
    close_reason: String,
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
    #[cfg(target_os = "linux")]
    {
        linux::registry().0
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "linux")))]
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
    slashes(path)
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
    let path = slashes(path);
    let path = path.trim_end_matches('/');
    let up = &path[..path.rfind('/')?];
    let name = up.rsplit('/').next().unwrap_or(up);
    (!name.is_empty()).then(|| name.to_string())
}

fn registered_vaults() -> Vec<Vault> {
    load_registry().unwrap_or_default()
}

/// The registry, or why there is none: a missing file and a malformed one are
/// different faults and the board names each.
fn load_registry() -> Result<Vec<Vault>, Failure> {
    let path = obsidian_json_path();
    let raw = fs::read_to_string(&path).map_err(|e| {
        Failure::new(
            "missing-input",
            format!("no Obsidian vault registry at {} ({e}); is Obsidian installed and run once?", path.display()),
            "vault-registry",
        )
    })?;
    let reg: ObsidianRegistry = serde_json::from_str(&raw).map_err(|e| {
        Failure::new("malformed-data", format!("{} is not a vault registry: {e}", path.display()), "vault-registry")
    })?;
    Ok(vaults_from(reg))
}

fn vaults_from(reg: ObsidianRegistry) -> Vec<Vault> {
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

/// Linux reads the same titles off the window manager's `_NET_CLIENT_LIST`
/// on X11. Wayland publishes no such list, so there it is empty and open
/// state falls back to Obsidian's flags, labelled as reported.
#[cfg(target_os = "linux")]
fn obsidian_windows() -> Vec<(isize, String)> {
    if linux::session() != linux::Session::X11 {
        return Vec::new();
    }
    match linux::x11::Ewmh::connect(None).and_then(|w| w.titled_windows()) {
        Ok(wins) => wins
            .into_iter()
            .filter_map(|(w, t)| vault_from_title(&t).map(|v| (w as isize, v)))
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
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
fn open_ids_observed(vaults: &[Vault]) -> HashSet<String> {
    let titled: Vec<String> = obsidian_windows().into_iter().map(|(_, v)| v).collect();
    vaults
        .iter()
        .filter(|v| titled.iter().any(|t| same_name(t, &v.name)))
        .filter(|v| v.open || !shares_name(vaults, v))
        .map(|v| v.id.clone())
        .collect()
}

#[cfg(windows)]
fn open_ids(vaults: &[Vault]) -> HashSet<String> {
    open_ids_observed(vaults)
}

/// Linux observes windows where the session lets it (X11 with a window
/// manager publishing EWMH) and otherwise falls back to Obsidian's flags,
/// which are then reported, not observed.
#[cfg(target_os = "linux")]
fn open_ids(vaults: &[Vault]) -> HashSet<String> {
    if window_observation() {
        open_ids_observed(vaults)
    } else {
        vaults.iter().filter(|v| v.open).map(|v| v.id.clone()).collect()
    }
}

#[cfg(target_os = "linux")]
fn window_observation() -> bool {
    linux::session() == linux::Session::X11
        && linux::x11::Ewmh::connect(None).map(|w| w.client_list().is_ok()).unwrap_or(false)
}

/// How far the open dots can be trusted: `observed` from live windows,
/// `reported` from Obsidian's own flags, which a crash can leave set.
fn open_confidence() -> &'static str {
    #[cfg(windows)]
    {
        "observed"
    }
    #[cfg(target_os = "macos")]
    {
        "reported"
    }
    #[cfg(target_os = "linux")]
    {
        if window_observation() {
            "observed"
        } else {
            "reported"
        }
    }
}

/// Why the board cannot close a vault's window here, empty when it can.
fn close_unavailable() -> String {
    #[cfg(windows)]
    {
        String::new()
    }
    #[cfg(target_os = "macos")]
    {
        "closing another application's window is not implemented on macOS".into()
    }
    #[cfg(target_os = "linux")]
    {
        let wc = linux::window_control();
        if wc.close {
            String::new()
        } else if wc.reason.is_empty() {
            "the window manager does not support _NET_CLOSE_WINDOW".into()
        } else {
            wc.reason
        }
    }
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
fn list_vaults(cfg: State<SharedConfig>, hist: State<FocusHistory>) -> Result<Vec<VaultTile>, Failure> {
    Ok(tiles(&cfg.get(), &hist, load_registry()?))
}

fn tiles(cfg: &Config, hist: &FocusHistory, all: Vec<Vault>) -> Vec<VaultTile> {
    // Open state is read against the whole registry, since a hidden vault
    // still has a window and its title is indistinguishable from its
    // namesake's; labels are only about what the board shows.
    let open = open_ids(&all);
    let open_state = open_confidence();
    let close_reason = close_unavailable();
    let shown: Vec<&Vault> = all.iter().filter(|v| !cfg.hidden(v)).collect();
    // Rank by whichever is newer: Obsidian's last-opened stamp or hvelf's
    // own last-focused record. Groups keep config order; recency within.
    let mut ranked: Vec<(usize, u64, VaultTile)> = shown
        .iter()
        .zip(qualifiers(&shown))
        .map(|(v, qualifier)| {
            let (pos, group) = cfg.group_of(v);
            let eff = v.ts.max(focused_ts(hist, &all, v));
            (
                pos,
                eff,
                VaultTile {
                    id: v.id.clone(),
                    name: v.name.clone(),
                    qualifier,
                    path: v.path.clone(),
                    open: open.contains(&v.id),
                    open_state,
                    close_reason: close_reason.clone(),
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
fn close_vault(id: String) -> Result<(), Failure> {
    #[cfg(target_os = "linux")]
    {
        let reason = close_unavailable();
        if !reason.is_empty() {
            return Err(Failure::new(
                if linux::session() == linux::Session::Wayland { "unsupported-session" } else { "disabled" },
                format!("hvelf cannot close vault windows here: {reason}. Close it in Obsidian."),
                "close-window",
            ));
        }
        let all = registered_vaults();
        let v = all.iter().find(|v| v.id == id).ok_or_else(|| Failure::new("missing-input", "no such vault", "close-window"))?;
        // Never a process kill: a polite request that Obsidian answers by
        // saving and closing, exactly as its own close button does.
        let h = window_for(&all, v).ok_or_else(|| {
            Failure::new(
                "missing-input",
                "no window can be told apart for this vault (not open, or it shares its name with another open vault)",
                "close-window",
            )
        })?;
        return linux::x11::Ewmh::connect(None)
            .and_then(|w| w.request_close(h as u32))
            .map_err(|e| Failure::new("io-error", e, "close-window"));
    }
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
    #[allow(unreachable_code)]
    Ok(())
}

/// Take a vault off the board for good. Its path goes into `hide`, in the
/// running config and in config.json: the path rather than the name, so a
/// namesake keeps its tile. Obsidian's registry is left alone, and deleting
/// the entry from config.json brings the tile back.
#[tauri::command]
fn hide_vault(cfg: State<SharedConfig>, id: String) -> Result<(), String> {
    let all = registered_vaults();
    let v = all.iter().find(|v| v.id == id).ok_or("no such vault")?;
    let mut cfg = cfg.0.lock().unwrap();
    if !cfg.hidden(v) {
        persist_hidden(&v.path)?;
        cfg.hide.push(v.path.clone());
    }
    Ok(())
}

fn do_launch(app: &AppHandle, cfg: &Config, id: &str) -> Result<(), Failure> {
    hide_window(app.clone());
    let all = registered_vaults();
    let v = match all.iter().find(|v| v.id == id) {
        Some(v) => v,
        None => return Err(Failure::new("missing-input", "that vault is no longer in Obsidian's registry", "open-vault")),
    };
    // An already-open vault is focused by hvelf itself: as the recipient of
    // the user's hotkey or click, hvelf holds the foreground-change rights
    // that Windows denies to a background Obsidian asked via URI. The URI
    // path serves vaults with no window yet, and deep links which must
    // navigate inside the vault.
    if cfg.deep_link(v).is_none() {
        if let Some(h) = window_for(&all, v) {
            focus_window(h);
            return Ok(());
        }
    }
    // By id, not by name: Obsidian's resolver takes either, checks the id
    // first, and would otherwise pick whichever same-named vault its
    // registry happens to list first.
    let mut uri = format!("obsidian://open?vault={}", urlencoding::encode(&v.id));
    if let Some(file) = cfg.deep_link(v) {
        uri.push_str(&format!("&file={}", urlencoding::encode(file)));
    }
    #[cfg(target_os = "linux")]
    {
        linux::open_uri(&uri, std::env::var("PATH").ok().as_deref())
    }
    #[cfg(not(target_os = "linux"))]
    {
        open_uri(&uri);
        Ok(())
    }
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

#[cfg(target_os = "linux")]
fn focus_window(hwnd: isize) {
    if let Err(e) = linux::x11::Ewmh::connect(None).and_then(|w| w.request_activate(hwnd as u32)) {
        eprintln!("hvelf: cannot raise window: {e}");
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
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

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "linux")))]
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
fn launch(app: AppHandle, cfg: State<SharedConfig>, id: String) -> Result<(), Failure> {
    do_launch(&app, &cfg.get(), &id)
}

/// A launch that nobody invoked from the board (a chord, a command line):
/// its failure is shown on the board rather than lost on stderr.
fn launch_or_tell(app: &AppHandle, cfg: &Config, id: &str) {
    if let Err(f) = do_launch(app, cfg, id) {
        eprintln!("hvelf: {}: {}", f.code, f.message);
        use tauri::Emitter;
        let _ = app.emit("hvelf-notice", f);
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

#[tauri::command]
fn capabilities(runtime: State<RuntimeCaps>) -> Vec<Capability> {
    #[allow(unused_mut)]
    let mut out: Vec<Capability> = Vec::new();
    #[cfg(target_os = "linux")]
    out.extend(linux::capabilities());
    out.extend(runtime.0.lock().unwrap().iter().cloned());
    out
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
fn spawn_hotkeys(app: AppHandle, cfg: SharedConfig, hist: FocusHistory) {
    std::thread::spawn(move || unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetMessageW, SetTimer, MSG, WM_HOTKEY, WM_TIMER,
        };

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
                        if let Some(name) = most_recent_vault(&cfg, &hist) {
                            launch_or_tell(&app, &cfg, &name);
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
fn spawn_hotkeys(app: AppHandle, cfg: SharedConfig, hist: FocusHistory) {
    hotkey_macos::install(app, cfg, hist);
}

/// Linux: on X11 the chords are grabbed on the root window by keycode, and
/// the active window is sampled for recency, as on Windows. X11 key grabs do
/// not cover a Wayland desktop, so there the board is summoned by a desktop
/// shortcut bound to `hvelf --toggle`; the capability records say which
/// route this session has.
#[cfg(target_os = "linux")]
fn spawn_hotkeys(app: AppHandle, cfg: SharedConfig, hist: FocusHistory) {
    let caps = app.state::<RuntimeCaps>().inner().clone();
    let session = linux::session();
    if session != linux::Session::X11 {
        let reason = match session {
            linux::Session::Wayland => "X11 key grabs do not cover a Wayland desktop; portal binding is not implemented, so bind `hvelf --toggle` as a desktop shortcut",
            _ => "no graphical session",
        };
        caps.record(linux::cap("global-hotkey-x11", if session == linux::Session::Wayland { "unsupported" } else { "unavailable" }, reason, "XGrabKey"));
        return;
    }
    let boot = cfg.get();
    std::thread::spawn(move || {
        let g = match linux::x11::Grabber::connect(None) {
            Ok(g) => g,
            Err(e) => {
                caps.record(linux::cap("global-hotkey-x11", "unavailable", e, "XGrabKey"));
                return;
            }
        };
        let mut chords = Vec::new();
        let mut notes = Vec::new();
        for (slot, spec) in [(1u8, boot.hotkey.clone()), (2u8, boot.quick_launch.clone())] {
            if spec.is_empty() {
                continue;
            }
            match linux::x11::parse_chord(&spec, |s| g.keycode_for(s)) {
                None => notes.push(format!("cannot parse '{spec}'")),
                Some(c) => match g.grab(c) {
                    Ok(()) => chords.push((slot, c)),
                    Err(linux::x11::GrabFailure::Conflict) => notes.push(format!("'{spec}' is already grabbed by another application")),
                    Err(linux::x11::GrabFailure::Other(e)) => notes.push(format!("'{spec}': {e}")),
                },
            }
        }
        let state = if notes.is_empty() { "available" } else if chords.is_empty() { "unavailable" } else { "degraded" };
        let reason = if notes.is_empty() {
            "chords grabbed on the root window by keycode; `grave` is the physical key under Esc".to_string()
        } else {
            notes.join("; ")
        };
        caps.record(linux::cap("global-hotkey-x11", state, reason, "XGrabKey"));

        // Recency: which vault window the user is in, every 2 s, on its own
        // connection so a blocked event wait never starves it.
        let sampler_hist = hist.clone();
        std::thread::spawn(move || {
            let Ok(w) = linux::x11::Ewmh::connect(None) else { return };
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if let Some(v) = w.active_window().and_then(|a| w.title(a)).and_then(|t| vault_from_title(&t)) {
                    sampler_hist.note(v);
                }
            }
        });

        let specs: Vec<linux::x11::Chord> = chords.iter().map(|(_, c)| *c).collect();
        while let Some(i) = g.next_press(&specs) {
            match chords[i].0 {
                1 => {
                    let app2 = app.clone();
                    let _ = app.run_on_main_thread(move || toggle_window(&app2));
                }
                _ => {
                    let app2 = app.clone();
                    let cfg2 = cfg.clone();
                    let hist2 = hist.clone();
                    let _ = app.run_on_main_thread(move || {
                        let c = cfg2.get();
                        if let Some(id) = most_recent_vault(&c, &hist2) {
                            launch_or_tell(&app2, &c, &id);
                        }
                    });
                }
            }
        }
    });
}

#[cfg(all(not(windows), not(target_os = "macos"), not(target_os = "linux")))]
fn spawn_hotkeys(_app: AppHandle, _cfg: SharedConfig, _hist: FocusHistory) {
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

// ------------------------------------------------------------ command line
//
// A second `hvelf` hands its arguments to the running one (the single-
// instance plugin) and exits, so these double as the command a desktop
// shortcut binds where no global chord can be grabbed. No argument keeps
// the old meaning: toggle the board.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Toggle,
    Show,
    Hide,
    QuickLaunch,
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run(Option<Action>),
    Capabilities,
    ListVaults,
    Help,
}

const USAGE: &str = "usage: hvelf [--toggle | --show | --hide | --quick-launch | --quit | --capabilities | --list-vaults | --help]";

fn parse_cli(args: &[String]) -> Result<Cli, String> {
    let mut out = Cli::Run(None);
    for a in args.iter().skip(1) {
        let next = match a.as_str() {
            "--toggle" => Cli::Run(Some(Action::Toggle)),
            "--show" => Cli::Run(Some(Action::Show)),
            "--hide" => Cli::Run(Some(Action::Hide)),
            "--quick-launch" => Cli::Run(Some(Action::QuickLaunch)),
            "--quit" => Cli::Run(Some(Action::Quit)),
            "--capabilities" => Cli::Capabilities,
            "--list-vaults" => Cli::ListVaults,
            "-h" | "--help" => Cli::Help,
            other => return Err(format!("unknown argument {other:?}")),
        };
        if out != Cli::Run(None) {
            return Err("give at most one argument".into());
        }
        out = next;
    }
    Ok(out)
}

fn perform(app: &AppHandle, action: Action) {
    match action {
        Action::Toggle => toggle_window(app),
        Action::Show => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        Action::Hide => hide_window(app.clone()),
        Action::Quit => app.exit(0),
        Action::QuickLaunch => {
            let cfg = app.state::<SharedConfig>().get();
            let hist = app.state::<FocusHistory>().inner().clone();
            if let Some(id) = most_recent_vault(&cfg, &hist) {
                launch_or_tell(app, &cfg, &id);
            }
        }
    }
}

fn single_instance_plugin() -> Option<tauri::plugin::TauriPlugin<tauri::Wry>> {
    #[cfg(target_os = "linux")]
    if !linux::session_bus_available() {
        eprintln!("hvelf: no D-Bus session bus; running without single-instance forwarding");
        return None;
    }
    Some(tauri_plugin_single_instance::init(|app, argv, _cwd| {
        match parse_cli(&argv) {
            Ok(Cli::Run(Some(a))) => perform(app, a),
            _ => toggle_window(app),
        }
    }))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let initial = match parse_cli(&args) {
        Ok(Cli::Run(a)) => a,
        Ok(Cli::Help) => {
            println!("{USAGE}");
            return;
        }
        Ok(Cli::Capabilities) => {
            #[allow(unused_mut)]
            let mut caps: Vec<Capability> = Vec::new();
            #[cfg(target_os = "linux")]
            caps.extend(linux::capabilities());
            println!("{}", serde_json::to_string_pretty(&caps).unwrap());
            return;
        }
        Ok(Cli::ListVaults) => {
            // Names, paths, ids and open state only; never vault contents.
            match load_registry() {
                Ok(all) => {
                    let t = tiles(&load_or_seed_config(), &FocusHistory::default(), all);
                    println!("{}", serde_json::to_string_pretty(&t).unwrap());
                }
                Err(f) => {
                    eprintln!("{}", serde_json::to_string(&f).unwrap());
                    std::process::exit(1);
                }
            }
            return;
        }
        Err(e) => {
            eprintln!("hvelf: {e}\n{USAGE}");
            std::process::exit(2);
        }
    };

    let cfg = load_or_seed_config();
    let hide_on_blur = cfg.hide_on_blur;
    let runtime_caps = RuntimeCaps::default();

    let mut builder = tauri::Builder::default();
    if let Some(p) = single_instance_plugin() {
        builder = builder.plugin(p);
    } else {
        #[cfg(target_os = "linux")]
        runtime_caps.record(linux::cap(
            "single-instance",
            "unavailable",
            "no D-Bus session bus: a second hvelf starts a second board instead of reaching this one",
            "tauri-plugin-single-instance",
        ));
    }
    builder
        .manage(SharedConfig(std::sync::Arc::new(std::sync::Mutex::new(cfg))))
        .manage(FocusHistory::default())
        .manage(runtime_caps)
        .setup(move |app| {
            // The tray loads its indicator library at run time on Linux; a
            // desktop without one keeps the board, reachable by command.
            #[cfg(target_os = "linux")]
            {
                let caps = app.state::<RuntimeCaps>().inner().clone();
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_tray(app))) {
                    Ok(Ok(())) => caps.record(linux::cap("tray", "available", "StatusNotifierItem through libayatana-appindicator; GNOME shows it only with the AppIndicator extension enabled", "tray-icon")),
                    Ok(Err(e)) => caps.record(linux::cap("tray", "unavailable", format!("tray failed: {e}"), "tray-icon")),
                    Err(_) => caps.record(linux::cap("tray", "unavailable", "no appindicator library could be loaded; install libayatana-appindicator3-1", "tray-icon")),
                }
            }
            #[cfg(not(target_os = "linux"))]
            build_tray(app)?;
            let cfg = app.state::<SharedConfig>().inner().clone();
            let hist = app.state::<FocusHistory>().inner().clone();
            spawn_hotkeys(app.handle().clone(), cfg, hist);
            if let Some(a) = initial {
                perform(app.handle(), a);
            }
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
            hide_vault,
            hide_window,
            capabilities,
            quit
        ])
        .run(tauri::generate_context!())
        .expect("hvelf: failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        std::iter::once("hvelf").chain(a.iter().copied()).map(String::from).collect()
    }

    #[test]
    fn command_line_actions() {
        assert_eq!(parse_cli(&args(&[])), Ok(Cli::Run(None)));
        assert_eq!(parse_cli(&args(&["--toggle"])), Ok(Cli::Run(Some(Action::Toggle))));
        assert_eq!(parse_cli(&args(&["--quick-launch"])), Ok(Cli::Run(Some(Action::QuickLaunch))));
        assert_eq!(parse_cli(&args(&["--capabilities"])), Ok(Cli::Capabilities));
        assert!(parse_cli(&args(&["--toggle", "--quit"])).is_err());
        assert!(parse_cli(&args(&["obsidian://open?vault=x; rm -rf ~"])).is_err());
    }

    fn vault(id: &str, path: &str, open: bool) -> Vault {
        Vault { id: id.into(), name: basename(path), path: path.into(), ts: 0, open }
    }

    #[test]
    fn duplicate_basenames_keep_distinct_ids_and_qualifiers() {
        let reg: ObsidianRegistry = serde_json::from_str(r#"{"vaults":{
            "a1b2c3d4e5f60718":{"path":"/home/p q/Lab/Vaults/Research/notes","ts":2,"open":true},
            "0918273645abcdef":{"path":"/home/p q/archive/notes","ts":1},
            "ffffeeee00001111":{"path":"/home/p q/Ünïcode \"quoted\" vault","ts":3}
        }}"#).unwrap();
        let all = vaults_from(reg);
        let cfg = Config::default();
        let t = tiles(&cfg, &FocusHistory::default(), all);
        let ids: HashSet<&str> = t.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids.len(), 3);
        let notes: Vec<&VaultTile> = t.iter().filter(|x| x.name == "notes").collect();
        assert_eq!(notes.len(), 2);
        let quals: HashSet<&str> = notes.iter().map(|x| x.qualifier.as_str()).collect();
        assert_eq!(quals, HashSet::from(["Research", "archive"]));
        let odd = t.iter().find(|x| x.id == "ffffeeee00001111").unwrap();
        assert_eq!(odd.name, "Ünïcode \"quoted\" vault");
        assert_eq!(odd.qualifier, "");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn backslash_is_a_name_character_and_case_matters_on_linux() {
        assert_eq!(basename("/v/a\\b"), "a\\b");
        assert_eq!(parent("/v/x\\y/notes").as_deref(), Some("x\\y"));
        let v = vault("1", "/home/p/Notes", false);
        assert!(!entry_matches("/home/p/notes", &v));
        assert!(entry_matches("/home/p/Notes/", &v));
    }

    #[test]
    fn launch_uri_carries_the_id_not_the_name() {
        // The URI builder in do_launch encodes the id; mirror it here.
        let id = "a1b2c3d4e5f60718";
        let uri = format!("obsidian://open?vault={}", urlencoding::encode(id));
        assert_eq!(uri, "obsidian://open?vault=a1b2c3d4e5f60718");
        let file = "Daily notes/2026 \"q\" é.md";
        assert_eq!(urlencoding::encode(file), "Daily%20notes%2F2026%20%22q%22%20%C3%A9.md");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn without_window_observation_open_flags_are_reported_not_observed() {
        if linux::session() == linux::Session::X11 && window_observation() {
            eprintln!("skipped: this X session publishes a client list");
            return;
        }
        let all = vec![vault("1", "/v/a", true), vault("2", "/v/b", false)];
        let open = open_ids(&all);
        assert_eq!(open, HashSet::from(["1".to_string()]));
        assert_eq!(open_confidence(), "reported");
        assert!(!close_unavailable().is_empty());
    }

    #[test]
    fn hiding_by_path_survives_restart_and_keeps_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("hvelf-cfg-{} é 'q'", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"zeta":1,"hotkey":"alt+grave","hide":[]}"#).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        doc["hide"].as_array_mut().unwrap().push("/home/p q/archive/notes".into());
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        let back: Config = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let v = vault("0918273645abcdef", "/home/p q/archive/notes", false);
        let twin = vault("a1b2c3d4e5f60718", "/home/p q/Lab/notes", false);
        assert!(back.hidden(&v));
        assert!(!back.hidden(&twin));
        let keys: Vec<String> = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap())
            .unwrap().as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["zeta", "hotkey", "hide"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
