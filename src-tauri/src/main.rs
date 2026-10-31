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
#[cfg(target_os = "macos")]
mod hotkey_macos;
mod keep;
#[cfg(target_os = "linux")]
mod linux;
mod vaultmap;

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
    /// Hotkey that files the text selected in the foreground app into a
    /// vault's keeps folder. Empty string disables it.
    keep: String,
    /// exocortex.toml, which names the vault root and the fallback vault.
    exocortex: String,
    /// The command that prints Lab-OS's vault map (labos-vault-map/v1). With
    /// one, the board and the keep chord take their vaults from it; empty,
    /// the board shows Obsidian's registry alone and keeps go by `exocortex`.
    vault_map: Vec<String>,
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
            hotkey: default_hotkey().into(),
            quick_launch: String::new(),
            keep: "alt+k".into(),
            exocortex: keep::DEFAULT_REGISTRY.into(),
            vault_map: default_vault_map(),
            hide_on_blur: true,
            hide: Vec::new(),
            groups: Vec::new(),
            deep_links: HashMap::new(),
        }
    }
}

/// `alt+grave` everywhere but Linux, where GNOME already binds Alt and the
/// key above Tab to switching an application's windows; Ctrl+Alt with that
/// key is bound by nothing there. `alt+grave` stays a choice in the config.
fn default_hotkey() -> &'static str {
    if cfg!(target_os = "linux") {
        "ctrl+alt+grave"
    } else {
        "alt+grave"
    }
}

/// Lab-OS's exo prints the vault map on Linux; elsewhere there is none.
fn default_vault_map() -> Vec<String> {
    if cfg!(target_os = "linux") {
        ["exo", "vaults", "--json"].map(String::from).to_vec()
    } else {
        Vec::new()
    }
}

/// Does a config entry name this vault? Entries are vault names, as they
/// always have been; a full path also matches, which is the only way to name
/// one of two vaults that share a name. A path matches once links resolve,
/// so hiding a vault also hides a link to it that Obsidian lists apart.
fn entry_matches(entry: &str, v: &Vault) -> bool {
    entry == v.name
        || norm_path(entry) == norm_path(&v.path)
        || (Path::new(entry).is_absolute() && same_dir(Path::new(entry), Path::new(&v.path)))
}

/// Two paths name the same folder once links resolve. A path that does not
/// resolve matches nothing here.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
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

/// The config as it stands, without seeding a file: for probes that must not
/// write.
fn read_config() -> Config {
    fs::read_to_string(config_file())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
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
    /// `observed` or `reported`; see open_confidence.
    open_state: &'static str,
    /// Why the cross cannot close this vault's window here; empty when it can.
    close_reason: String,
    group: String,
    /// The vault's id in Lab-OS's vault map (`Research/AI/AL`), empty for a
    /// vault the map does not hold.
    canonical: String,
    /// In a bounded partition (Admin, Career, Personal): searched only when
    /// named. The tile says so.
    bounded: bool,
    /// Obsidian still lists the vault, but its folder is not on this machine.
    missing: bool,
    /// False for a vault of the map that Obsidian has never opened: the tile
    /// shows it, and says how to open it, but cannot open it itself.
    registered: bool,
}

/// Ids of tiles for vaults of the map that Obsidian does not know: this
/// prefix, then the canonical id.
const MAP_TILE: &str = "map:";

// ---------------------------------------------------------------- vault map

/// Lab-OS's vault map as last read, and when. The board takes the copy in
/// hand and has an old one reread in the background; the keep chord, off
/// the main thread, rereads an old one before it files anything.
#[derive(Clone, Default)]
struct MapCache(Arc<Mutex<MapSlot>>);

#[derive(Default)]
struct MapSlot {
    read: Option<(std::time::Instant, Result<vaultmap::VaultMap, String>)>,
    reading: bool,
}

const MAP_FRESH: std::time::Duration = std::time::Duration::from_secs(30);
const MAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Read the map now, or None when the config names no map command.
fn read_map(cfg: &Config) -> Option<Result<vaultmap::VaultMap, String>> {
    (!cfg.vault_map.is_empty()).then(|| vaultmap::load(&cfg.vault_map, MAP_TIMEOUT))
}

impl MapCache {
    /// The map, read again when the copy is old. Waits on the command.
    fn fresh(&self, cfg: &Config) -> Option<Result<vaultmap::VaultMap, String>> {
        if cfg.vault_map.is_empty() {
            return None;
        }
        if let Some((at, r)) = &self.0.lock().unwrap().read {
            if at.elapsed() < MAP_FRESH {
                return Some(r.clone());
            }
        }
        let r = read_map(cfg)?;
        self.0.lock().unwrap().read = Some((std::time::Instant::now(), r.clone()));
        Some(r)
    }

    /// The copy in hand, for the board: it waits only while no copy has
    /// ever been read, and an old copy is reread in the background.
    fn cached(&self, cfg: &Config) -> Option<vaultmap::VaultMap> {
        if cfg.vault_map.is_empty() {
            return None;
        }
        let held = self.0.lock().unwrap().read.as_ref().map(|(at, r)| (at.elapsed() >= MAP_FRESH, r.clone()));
        match held {
            None => self.fresh(cfg).and_then(|r| r.ok()),
            Some((old, r)) => {
                if old {
                    self.refresh_in_background(cfg);
                }
                r.ok()
            }
        }
    }

    fn refresh_in_background(&self, cfg: &Config) {
        {
            let mut slot = self.0.lock().unwrap();
            if slot.reading {
                return;
            }
            slot.reading = true;
        }
        let (me, cfg) = (self.clone(), cfg.clone());
        std::thread::spawn(move || {
            let r = read_map(&cfg);
            let mut slot = me.0.lock().unwrap();
            if let Some(r) = r {
                slot.read = Some((std::time::Instant::now(), r));
            }
            slot.reading = false;
        });
    }
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
fn list_vaults(cfg: State<SharedConfig>, hist: State<FocusHistory>, maps: State<MapCache>) -> Result<Vec<VaultTile>, Failure> {
    let cfg = cfg.get();
    let map = maps.cached(&cfg);
    Ok(tiles(&cfg, &hist, load_registry()?, map.as_ref()))
}

fn tiles(cfg: &Config, hist: &FocusHistory, all: Vec<Vault>, map: Option<&vaultmap::VaultMap>) -> Vec<VaultTile> {
    // Open state is read against the whole registry, since a hidden vault
    // still has a window and its title is indistinguishable from its
    // namesake's; labels are only about what the board shows.
    let open = open_ids(&all);
    let open_state = open_confidence();
    let close_reason = close_unavailable();
    let recency = |v: &Vault| v.ts.max(focused_ts(hist, &all, v));
    // With a vault map, Obsidian's entries that are one vault (a link and
    // the folder it points at) make one tile: the one Obsidian used last.
    let locate = map.map(|m| m.locator());
    let canon: Vec<Option<&vaultmap::MapVault>> = all
        .iter()
        .map(|v| locate.as_ref().and_then(|l| l.at(Path::new(&v.path))))
        .collect();
    let mut known: HashSet<&str> = HashSet::new();
    let mut shown: Vec<(Vault, Option<&vaultmap::MapVault>, bool)> = Vec::new();
    for (i, v) in all.iter().enumerate() {
        let Some(mv) = canon[i] else {
            if !cfg.hidden(v) {
                shown.push((v.clone(), None, true));
            }
            continue;
        };
        if !known.insert(mv.id.as_str()) {
            continue;
        }
        let best = all
            .iter()
            .zip(&canon)
            .filter(|(o, c)| c.map(|c| c.id == mv.id).unwrap_or(false) && !cfg.hidden(o))
            .map(|(o, _)| o)
            .max_by_key(|o| recency(o));
        if let Some(best) = best {
            shown.push((best.clone(), Some(mv), true));
        }
    }
    // The map's Obsidian vaults that Obsidian has not opened yet: shown, so
    // the board covers the vault root, but they cannot be opened from here.
    if let Some(m) = map {
        for mv in &m.vaults {
            if known.contains(mv.id.as_str()) || mv.excluded || !mv.path.join(".obsidian").is_dir() {
                continue;
            }
            let v = Vault {
                id: format!("{MAP_TILE}{}", mv.id),
                name: mv.name().to_string(),
                path: mv.path.display().to_string(),
                ts: 0,
                open: false,
            };
            if !cfg.hidden(&v) && !cfg.hide.iter().any(|e| e == &mv.id) {
                shown.push((v, Some(mv), false));
            }
        }
    }
    let refs: Vec<&Vault> = shown.iter().map(|(v, _, _)| v).collect();
    // Rank by whichever is newer: Obsidian's last-opened stamp or hvelf's
    // own last-focused record. Groups keep config order; recency within.
    let mut ranked: Vec<(usize, u64, VaultTile)> = shown
        .iter()
        .zip(qualifiers(&refs))
        .map(|((v, mv, registered), qualifier)| {
            let (pos, group) = cfg.group_of(v);
            let eff = if *registered { recency(v) } else { 0 };
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
                    canonical: mv.map(|m| m.id.clone()).unwrap_or_default(),
                    bounded: mv.map(|m| m.bounded()).unwrap_or(false),
                    missing: *registered && !Path::new(&v.path).is_dir(),
                    registered: *registered,
                },
            )
        })
        .collect();
    // Within a group, what can be opened comes first, so `1` and Enter never
    // land on a vault that is not there: then vaults Obsidian has not opened
    // yet, then registry entries whose folder is gone.
    let rank = |t: &VaultTile| if t.missing { 2 } else if !t.registered { 1 } else { 0 };
    ranked.sort_by_key(|(pos, eff, t)| (*pos, rank(t), std::cmp::Reverse(*eff)));
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
/// namesake keeps its tile. Obsidian's registry is left alone, and Hidden
/// vaults in the tray menu brings the tile back.
#[tauri::command]
fn hide_vault(app: AppHandle, cfg: State<SharedConfig>, maps: State<MapCache>, id: String) -> Result<(), String> {
    // A map tile is a folder Obsidian does not know; any other, its vault.
    let path = match id.strip_prefix(MAP_TILE) {
        Some(cid) => {
            let m = maps.cached(&cfg.get()).ok_or("no vault map")?;
            m.vaults.iter().find(|v| v.id == cid).ok_or("no such vault")?.path.display().to_string()
        }
        None => registered_vaults().into_iter().find(|v| v.id == id).ok_or("no such vault")?.path,
    };
    let v = Vault { id, name: basename(&path), path, ts: 0, open: false };
    {
        let mut cfg = cfg.0.lock().unwrap();
        if cfg.hidden(&v) {
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
    let name_of = |e: &str| name_of(e, all);
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

/// What a `hide` entry calls its vault: the name of the registry vault it
/// names, else the last part of the entry itself.
fn name_of(entry: &str, all: &[Vault]) -> String {
    all.iter()
        .find(|v| entry_matches(entry, v))
        .map_or_else(|| basename(entry), |v| v.name.clone())
}

/// The `hide` entries `hvelf --unhide <arg>` puts back. A path matches its
/// own entry; a bare name matches every entry whose vault has that name,
/// as Obsidian matches names, ignoring case. The tray takes back exactly
/// the one entry it lists instead.
fn unhide_matches(arg: &str, hide: &[String], all: &[Vault]) -> Vec<String> {
    let is_path = arg.contains('/') || (cfg!(windows) && arg.contains('\\'));
    let mut seen = HashSet::new();
    hide.iter()
        .filter(|e| seen.insert(e.as_str()))
        .filter(|e| if is_path { norm_path(e) == norm_path(arg) } else { same_name(&name_of(e, all), arg) })
        .cloned()
        .collect()
}

fn do_launch(app: &AppHandle, cfg: &Config, id: &str) -> Result<(), Failure> {
    if let Some(cid) = id.strip_prefix(MAP_TILE) {
        return Err(Failure::new(
            "missing-input",
            format!("{cid} is not a vault Obsidian knows yet: open its folder once with Obsidian's \"Open folder as vault\", and this tile opens it from then on"),
            "open-vault",
        ));
    }
    let all = registered_vaults();
    let v = match all.iter().find(|v| v.id == id) {
        Some(v) => v,
        None => return Err(Failure::new("missing-input", "that vault is no longer in Obsidian's registry", "open-vault")),
    };
    if !Path::new(&v.path).is_dir() {
        return Err(Failure::new(
            "missing-input",
            format!("{} is not on this machine; Obsidian's registry still lists the vault there", v.path),
            "open-vault",
        ));
    }
    hide_window(app.clone());
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
        .filter(|v| !cfg.hidden(v) && Path::new(&v.path).is_dir())
        .max_by_key(|v| v.ts.max(focused_ts(hist, &all, v)))
        .map(|v| v.id.clone())
}

#[tauri::command]
fn launch(app: AppHandle, cfg: State<SharedConfig>, id: String) -> Result<(), Failure> {
    let r = do_launch(&app, &cfg.get(), &id);
    // The board may have gone already; bring it back to say why.
    if r.is_err() {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
    r
}

/// A launch that nobody invoked from the board (a chord, a command line):
/// its failure is shown on the board rather than lost on stderr.
fn launch_or_tell(app: &AppHandle, cfg: &Config, id: &str) {
    if let Err(f) = do_launch(app, cfg, id) {
        eprintln!("hvelf: {}: {}", f.code, f.message);
        let _ = app.emit("hvelf-notice", f);
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

#[tauri::command]
#[allow(unused_variables)]
fn capabilities(runtime: State<RuntimeCaps>, cfg: State<SharedConfig>, maps: State<MapCache>) -> Vec<Capability> {
    #[allow(unused_mut)]
    let mut out: Vec<Capability> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let cfg = cfg.get();
        let map = maps.fresh(&cfg);
        out.extend(linux::capabilities());
        out.push(map_capability(&cfg, &map));
        out.push(keep_capability(&cfg, map));
    }
    out.extend(runtime.0.lock().unwrap().iter().cloned());
    out
}

// ------------------------------------------------------------------ keep
//
// The keep chord captures whatever text is selected in the foreground app
// and files it verbatim as a note under <vault>/keeps/. The board then
// opens in keep mode to ask which vault: Enter or the chord again takes
// the preselected one, a tile key picks another, Esc parks the note in the
// fallback vault's unrouted folder. A capture is never dropped. On Linux the
// selection is X11's PRIMARY, read in linux.rs; on Windows UI Automation or
// the clipboard, in capture.rs.

/// A capture waiting on the board for a vault.
struct Pending {
    body: String,
    id: String,
    ts: String,
    month: String,
    app: String,
    title: String,
    url: Option<String>,
    /// Preselected vault, by id: from the window title, else the last one
    /// used, else the fallback.
    vault: String,
    /// Vault tiles in board order, preselected first.
    vaults: Vec<keep::Target>,
    exo: keep::Exocortex,
    /// Window to give the focus back to once the note is filed.
    source: isize,
}

#[derive(Clone, Default)]
struct KeepState(Arc<Mutex<Option<Pending>>>);

/// One capture at a time: the chord and `hvelf --keep` run on different
/// threads, and a second capture must see the first one's pending note.
static KEEP_TURN: Mutex<()> = Mutex::new(());

/// What a capture found, before the board asks where it goes.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
struct Captured {
    body: String,
    /// The program it came from, by executable name.
    app: String,
    title: String,
    url: Option<String>,
    /// Window to give the focus back to once the note is filed.
    source: isize,
}

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
    /// The lit vault, by id, as a tile.
    vault: KeepTile,
    vaults: Vec<KeepTile>,
    app: String,
}

/// A vault in keep mode: its id, which is what the board files by, and the
/// name and parent folders it shows.
#[derive(Serialize, Clone)]
struct KeepTile {
    id: String,
    name: String,
    qualifier: String,
    bounded: bool,
}

fn keep_tile(t: &keep::Target) -> KeepTile {
    let (qualifier, name) = t.id.rsplit_once('/').unwrap_or(("", &t.id));
    KeepTile { id: t.id.clone(), name: name.to_string(), qualifier: qualifier.to_string(), bounded: t.bounded }
}

#[tauri::command]
fn keep_pending(keep: State<KeepState>) -> Option<KeepInfo> {
    keep.0.lock().unwrap().as_ref().map(|p| KeepInfo {
        preview: keep::preview(&p.body, 60),
        chars: p.body.chars().count(),
        vault: p
            .vaults
            .iter()
            .find(|t| t.id == p.vault)
            .map(keep_tile)
            .unwrap_or_else(|| keep_tile(&keep::Target { id: p.vault.clone(), names: vec![], bounded: false })),
        vaults: p.vaults.iter().map(keep_tile).collect(),
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
    let into = chosen.as_deref().unwrap_or(&p.exo.fallback);
    if let Err(e) = keep::write_in_vault(&p.exo.root, into, &path, &bytes) {
        eprintln!("hvelf: keep write failed at {}: {e}", path.display());
        // Never drop a capture: the unrouted folder is the last resort, and
        // if even that fails the toast says so and the text is in stderr.
        if chosen.is_some() && keep::write_in_vault(&p.exo.root, &p.exo.fallback, &unrouted, &bytes).is_ok() {
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
/// board's own recency order, then those the board does not know. A vault of
/// the map is ranked by the Obsidian vault in the same folder; at a legacy
/// root, by the Obsidian vault of the same name.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn order_for_board(
    targets: &[keep::Target],
    pre: &str,
    exo: &keep::Exocortex,
    all: &[Vault],
    cfg: &Config,
    hist: &FocusHistory,
) -> Vec<keep::Target> {
    let mapped = exo.mapped.is_some();
    let recency = |t: &keep::Target| -> u64 {
        all.iter()
            .filter(|v| !cfg.hidden(v))
            .filter(|v| if mapped { same_dir(Path::new(&v.path), &exo.root.join(&t.id)) } else { v.name == t.id })
            .map(|v| v.ts.max(focused_ts(hist, all, v)))
            .max()
            .unwrap_or(0)
    };
    let mut ranked: Vec<(u64, &keep::Target)> = targets.iter().filter(|t| t.id != pre).map(|t| (recency(t), t)).collect();
    ranked.sort_by_key(|(eff, _)| std::cmp::Reverse(*eff));
    let first = targets
        .iter()
        .find(|t| t.id == pre)
        .cloned()
        .unwrap_or_else(|| keep::Target { id: pre.to_string(), names: vec![], bounded: false });
    std::iter::once(first).chain(ranked.into_iter().map(|(_, t)| t.clone())).collect()
}

#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
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
    let _turn = KEEP_TURN.lock().unwrap_or_else(|e| e.into_inner());
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
    let exo = keep_destination(app, cfg);
    if let Some(why) = keep_blocker(&exo) {
        toast(app, &format!("keep: {why}; {}", keep_source(cfg)));
        return;
    }
    let c = Captured { body, app: src.exe.clone(), title: src.title.clone(), url, source: src.hwnd };
    queue_keep(app, cfg, hist, keep, exo, now, c);
}

/// The chord on Linux, run off the main thread: reading the selection is a
/// round trip through the program holding it, which may be the board
/// itself. The registry is checked first, so a broken one takes nothing:
/// reading PRIMARY leaves the selection where it was anyway.
#[cfg(target_os = "linux")]
fn start_keep(app: &AppHandle, cfg: &Config, hist: &FocusHistory, keep: &KeepState) {
    let _turn = KEEP_TURN.lock().unwrap_or_else(|e| e.into_inner());
    // Chord again while a capture waits on the board: file it where the
    // banner says.
    if keep.0.lock().unwrap().is_some() {
        file_pending(app, keep, Route::Preselected, true);
        return;
    }
    let exo = keep_destination(app, cfg);
    if let Some(why) = keep_blocker(&exo) {
        toast(app, &format!("keep: {why}; {}", keep_source(cfg)));
        return;
    }
    let now = chrono::Local::now();
    match linux::grab_selection(std::process::id(), std::time::Duration::from_millis(1500)) {
        Ok(linux::Grab::Own) => hide_window(app.clone()),
        Ok(linux::Grab::Text(g)) => {
            let c = Captured { body: g.text, app: g.app, title: g.title, url: None, source: g.window as isize };
            queue_keep(app, cfg, hist, keep, exo, now, c);
        }
        Err(why) => toast(app, &format!("keep: {why}")),
    }
}

/// Where keeps go: Lab-OS's vault map when the config names one, else the
/// registry file. A map that cannot be read leaves nothing to file into:
/// without it there is no guessing at a root.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn destination(cfg: &Config, map: Option<Result<vaultmap::VaultMap, String>>) -> keep::Exocortex {
    match map {
        None => keep::load_exocortex(Path::new(&cfg.exocortex)),
        Some(Ok(m)) => keep::Exocortex::from_map(&m),
        Some(Err(e)) => keep::Exocortex { problem: Some(format!("no vault map ({e})")), ..keep::Exocortex::default() },
    }
}

#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn keep_destination(app: &AppHandle, cfg: &Config) -> keep::Exocortex {
    destination(cfg, app.state::<MapCache>().fresh(cfg))
}

/// Where the vaults came from, for a message that has to say.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn keep_source(cfg: &Config) -> String {
    if cfg.vault_map.is_empty() {
        format!("registry {}", cfg.exocortex)
    } else {
        format!("vault map from `{}`", cfg.vault_map.join(" "))
    }
}

/// Why a capture cannot be filed: no usable root, or no fallback vault to
/// park it in, which is what keeps a capture from ever being dropped.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn keep_blocker(exo: &keep::Exocortex) -> Option<String> {
    exo.unusable().or_else(|| {
        (!exo.root.join(&exo.fallback).is_dir())
            .then(|| format!("the fallback vault {} is not under {}", exo.fallback, exo.root.display()))
    })
}

/// Put a capture on the board in keep mode, with the vault it will go to
/// lit: the one named in the window title, else the last one kept into,
/// else the fallback.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn queue_keep(
    app: &AppHandle,
    cfg: &Config,
    hist: &FocusHistory,
    keep: &KeepState,
    exo: keep::Exocortex,
    now: chrono::DateTime<chrono::Local>,
    c: Captured,
) {
    let targets = exo.targets();
    if targets.is_empty() {
        eprintln!("hvelf: no vaults under {}", exo.root.display());
    }
    // The last vault kept into, as it was recorded: an id, or an older name
    // that one of the vaults still answers to.
    let last = load_persisted().last_keep_vault;
    let last = targets
        .iter()
        .find(|t| t.id == last)
        .or_else(|| targets.iter().find(|t| t.names.contains(&last)))
        .map(|t| t.id.clone());
    let pre = keep::preselect_target(&c.title, &targets)
        .or(last)
        .unwrap_or_else(|| exo.fallback.clone());
    let ordered = order_for_board(&targets, &pre, &exo, &registered_vaults(), cfg, hist);
    // Names and a count only: the text itself never goes to a log.
    eprintln!(
        "hvelf: keep: {} chars from {} are waiting on the board; {pre} is lit; vaults {}",
        c.body.chars().count(),
        c.app,
        ordered.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ")
    );
    let pending = Pending {
        body: c.body,
        id: keep::new_id(&now.format("%Y%m%dT%H%M%S").to_string(), rand32()),
        ts: now.format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        month: now.format("%Y-%m").to_string(),
        app: c.app,
        title: c.title,
        url: c.url,
        vault: pre,
        vaults: ordered,
        exo,
        source: c.source,
    };
    *keep.0.lock().unwrap() = Some(pending);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.emit("keep", ());
    }
}

/// `hvelf --keep`: the chord's work, for a desktop shortcut where no chord
/// can be grabbed. Off the calling thread, since it waits on other programs.
fn keep_from_command(app: &AppHandle) {
    #[cfg(any(windows, target_os = "linux"))]
    {
        let app = app.clone();
        std::thread::spawn(move || {
            #[cfg(windows)]
            capture::init_com();
            let cfg = app.state::<SharedConfig>().get();
            let hist = app.state::<FocusHistory>().inner().clone();
            let keep = app.state::<KeepState>().inner().clone();
            start_keep(&app, &cfg, &hist, &keep);
        });
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    toast(app, "keep: capture is not available on this platform");
}

/// Whether the vaults come from Lab-OS's vault map, for
/// `hvelf --capabilities` and the limits under the board.
#[cfg(target_os = "linux")]
fn map_capability(cfg: &Config, map: &Option<Result<vaultmap::VaultMap, String>>) -> Capability {
    let source = cfg.vault_map.join(" ");
    match map {
        None => linux::cap(
            "vault-map",
            "disabled",
            "no vaultMap command in config.json: the board shows Obsidian's registry alone and keeps go by exocortex",
            "config.json",
        ),
        Some(Err(e)) => linux::cap("vault-map", "unavailable", e.clone(), source),
        Some(Ok(m)) => linux::cap(
            "vault-map",
            "available",
            format!(
                "{} vaults under {}, fallback {}{}",
                m.vaults.len(),
                m.root.display(),
                m.fallback.as_deref().unwrap_or("none"),
                if m.problems.is_empty() { String::new() } else { format!("; problems: {}", m.problems.join("; ")) }
            ),
            source,
        ),
    }
}

/// Whether the keep chord can capture in this session, for
/// `hvelf --capabilities` and the limits under the board.
#[cfg(target_os = "linux")]
fn keep_capability(cfg: &Config, map: Option<Result<vaultmap::VaultMap, String>>) -> Capability {
    let source = keep_source(cfg);
    match linux::session() {
        linux::Session::Wayland => linux::cap(
            "keep-capture",
            "unsupported",
            "capture reads X11's PRIMARY selection; a Wayland desktop gives no application another one's selection",
            source,
        ),
        linux::Session::Headless => linux::cap("keep-capture", "unavailable", "no graphical session", source),
        linux::Session::X11 => match keep_blocker(&destination(cfg, map)) {
            Some(why) => linux::cap("keep-capture", "unavailable", why, source),
            None => linux::cap(
                "keep-capture",
                "available",
                "the text highlighted in the window in front (X11 PRIMARY), filed under <root>/<vault>/keeps/; browser addresses are not captured on Linux",
                source,
            ),
        },
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
fn spawn_hotkeys(app: AppHandle, cfg: SharedConfig, hist: FocusHistory) {
    std::thread::spawn(move || unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetMessageW, SetTimer, MSG, WM_HOTKEY, WM_TIMER,
        };

        // This thread does the UI Automation calls, so it owns a COM
        // apartment; the message loop below is its pump.
        capture::init_com();
        let keep = app.state::<KeepState>().inner().clone();

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
                        if let Some(name) = most_recent_vault(&cfg, &hist) {
                            launch_or_tell(&app, &cfg, &name);
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
        for (slot, spec) in [(1u8, boot.hotkey.clone()), (2u8, boot.quick_launch.clone()), (3u8, boot.keep.clone())] {
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
        let keep = app.state::<KeepState>().inner().clone();
        while let Some(i) = g.next_press(&specs) {
            match chords[i].0 {
                1 => {
                    let app2 = app.clone();
                    let _ = app.run_on_main_thread(move || toggle_window(&app2));
                }
                // On this thread: the capture waits on the program holding
                // the selection, and the main thread may be that program.
                3 => start_keep(&app, &cfg.get(), &hist, &keep),
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

// ------------------------------------------------------------ command line
//
// A second `hvelf` hands its arguments to the running one (the single-
// instance plugin) and exits, so these double as the command a desktop
// shortcut binds where no global chord can be grabbed. No argument keeps
// the old meaning: toggle the board.

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Toggle,
    Show,
    Hide,
    QuickLaunch,
    Keep,
    /// Put back the hidden vaults a name or path matches: the way back on a
    /// desktop that shows no tray.
    Unhide(String),
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run(Option<Action>),
    Capabilities,
    ListVaults,
    Hidden,
    Help,
}

const USAGE: &str = "usage: hvelf [--toggle | --show | --hide | --quick-launch | --keep | --quit | --capabilities | --list-vaults | --hidden | --unhide <name or path> | --help]";

fn parse_cli(args: &[String]) -> Result<Cli, String> {
    let mut out = Cli::Run(None);
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let next = match a.as_str() {
            "--toggle" => Cli::Run(Some(Action::Toggle)),
            "--show" => Cli::Run(Some(Action::Show)),
            "--hide" => Cli::Run(Some(Action::Hide)),
            "--quick-launch" => Cli::Run(Some(Action::QuickLaunch)),
            "--keep" => Cli::Run(Some(Action::Keep)),
            "--quit" => Cli::Run(Some(Action::Quit)),
            "--capabilities" => Cli::Capabilities,
            "--list-vaults" => Cli::ListVaults,
            "--hidden" => Cli::Hidden,
            "--unhide" => match it.next() {
                Some(v) if !v.is_empty() && !v.starts_with("--") => Cli::Run(Some(Action::Unhide(v.clone()))),
                _ => return Err("--unhide needs the name or path of a hidden vault".into()),
            },
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
        Action::Keep => keep_from_command(app),
        Action::Unhide(arg) => {
            let cfg = app.state::<SharedConfig>().get();
            let entries = unhide_matches(&arg, &cfg.hide, &registered_vaults());
            if entries.is_empty() {
                eprintln!("hvelf: nothing hidden matches {arg:?}");
            }
            for e in entries {
                unhide(app, &e);
            }
        }
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
        Ok(Cli::Run(a)) => {
            // Checked here, before the running board is reached, so a name
            // that matches nothing fails where it was typed.
            if let Some(Action::Unhide(arg)) = &a {
                let cfg = read_config();
                if unhide_matches(arg, &cfg.hide, &registered_vaults()).is_empty() {
                    eprintln!("hvelf: nothing hidden matches {arg:?}; `hvelf --hidden` lists what is");
                    std::process::exit(1);
                }
            }
            a
        }
        Ok(Cli::Hidden) => {
            // The Hidden vaults menu as data: each `hide` entry and its label.
            let cfg = read_config();
            let items: Vec<serde_json::Value> = hidden_menu(&cfg.hide, &registered_vaults())
                .into_iter()
                .map(|(entry, label)| serde_json::json!({"entry": entry, "label": label}))
                .collect();
            println!("{}", serde_json::to_string_pretty(&items).unwrap());
            return;
        }
        Ok(Cli::Help) => {
            println!("{USAGE}");
            return;
        }
        Ok(Cli::Capabilities) => {
            #[allow(unused_mut)]
            let mut caps: Vec<Capability> = Vec::new();
            #[cfg(target_os = "linux")]
            {
                let cfg = read_config();
                let map = read_map(&cfg);
                caps.extend(linux::capabilities());
                caps.push(map_capability(&cfg, &map));
                caps.push(keep_capability(&cfg, map));
            }
            println!("{}", serde_json::to_string_pretty(&caps).unwrap());
            return;
        }
        Ok(Cli::ListVaults) => {
            // Names, paths, ids and open state only; never vault contents.
            match load_registry() {
                Ok(all) => {
                    let cfg = load_or_seed_config();
                    let map = match read_map(&cfg) {
                        Some(Ok(m)) => Some(m),
                        Some(Err(e)) => {
                            eprintln!("hvelf: no vault map: {e}");
                            None
                        }
                        None => None,
                    };
                    let t = tiles(&cfg, &FocusHistory::default(), all, map.as_ref());
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
        .manage(KeepState::default())
        .manage(MapCache::default())
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
            if let Err(e) = build_toast(app) {
                eprintln!("hvelf: no toast window ({e}); keep messages go to stderr only");
            }
            let cfg = app.state::<SharedConfig>().inner().clone();
            let hist = app.state::<FocusHistory>().inner().clone();
            app.state::<MapCache>().refresh_in_background(&cfg.get());
            spawn_hotkeys(app.handle().clone(), cfg, hist);
            if let Some(a) = initial {
                perform(app.handle(), a);
            }
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
            capabilities,
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

    fn args(a: &[&str]) -> Vec<String> {
        std::iter::once("hvelf").chain(a.iter().copied()).map(String::from).collect()
    }

    #[test]
    fn command_line_actions() {
        assert_eq!(parse_cli(&args(&[])), Ok(Cli::Run(None)));
        assert_eq!(parse_cli(&args(&["--toggle"])), Ok(Cli::Run(Some(Action::Toggle))));
        assert_eq!(parse_cli(&args(&["--quick-launch"])), Ok(Cli::Run(Some(Action::QuickLaunch))));
        assert_eq!(parse_cli(&args(&["--keep"])), Ok(Cli::Run(Some(Action::Keep))));
        assert_eq!(parse_cli(&args(&["--hidden"])), Ok(Cli::Hidden));
        assert_eq!(parse_cli(&args(&["--unhide", "AL"])), Ok(Cli::Run(Some(Action::Unhide("AL".into())))));
        assert_eq!(parse_cli(&args(&["--unhide", "/v/a b"])), Ok(Cli::Run(Some(Action::Unhide("/v/a b".into())))));
        assert!(parse_cli(&args(&["--unhide"])).is_err());
        assert!(parse_cli(&args(&["--unhide", "--quit"])).is_err());
        assert!(parse_cli(&args(&["--unhide", ""])).is_err());
        assert!(parse_cli(&args(&["--unhide", "AL", "--toggle"])).is_err());
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
        let t = tiles(&cfg, &FocusHistory::default(), all, None);
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
        edit_hide(&path, |h| h.push("/home/p q/archive/notes".into())).unwrap();
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

    #[cfg(unix)]
    #[test]
    fn the_vault_map_makes_one_tile_per_vault_and_marks_the_bounded() {
        let root = std::env::temp_dir().join(format!("hvelf-tiles-map-{} é", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["Research/AI/AL/.obsidian", "Admin/.obsidian", "Lab-OS/.obsidian", "Learning"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::os::unix::fs::symlink(root.join("Research/AI/AL"), root.join("AL")).unwrap();
        let r = root.display().to_string();
        let map = vaultmap::parse(
            &serde_json::json!({
                "schema": vaultmap::SCHEMA, "root": r, "fallback": "Lab-OS",
                "vaults": [
                    {"id": "Admin", "path": format!("{r}/Admin"), "partition": "Admin", "bounded": true},
                    {"id": "Lab-OS", "path": format!("{r}/Lab-OS"), "partition": "global"},
                    {"id": "Learning", "path": format!("{r}/Learning"), "partition": "global"},
                    {"id": "Research/AI/AL", "path": format!("{r}/Research/AI/AL"), "partition": "global", "aliases": ["AL"]},
                ]
            })
            .to_string(),
        )
        .unwrap();
        let at = |id: &str, path: String, ts: u64| Vault { id: id.into(), name: basename(&path), path, ts, open: false };
        let all = vec![
            at("aaaa", format!("{r}/AL"), 5),
            at("bbbb", format!("{r}/Research/AI/AL"), 9),
            at("cccc", format!("{r}/Admin"), 1),
            at("dddd", "/gone/melt".into(), 3),
        ];
        let t = tiles(&Config::default(), &FocusHistory::default(), all.clone(), Some(&map));
        let by = |id: &str| t.iter().find(|x| x.id == id);
        assert!(by("aaaa").is_none(), "a link is the vault it points at");
        let al = by("bbbb").unwrap();
        assert_eq!((al.canonical.as_str(), al.registered, al.missing, al.qualifier.as_str()), ("Research/AI/AL", true, false, ""));
        assert!(by("cccc").unwrap().bounded);
        assert!(!al.bounded);
        let gone = by("dddd").unwrap();
        assert!(gone.missing && gone.canonical.is_empty());
        let lab = by("map:Lab-OS").unwrap();
        assert!(!lab.registered && !lab.missing);
        assert!(by("map:Learning").is_none(), "a vault of the map without .obsidian is no Obsidian vault");
        assert_eq!(t.len(), 4);
        // What opens comes first, whatever the stamps say: the gone melt was
        // used after Admin, yet it is last.
        let order: Vec<&str> = t.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(order, ["bbbb", "cccc", "map:Lab-OS", "dddd"]);

        // Hiding the folder hides the link Obsidian lists apart, and a hidden
        // vault does not come back as one Obsidian does not know.
        let mut cfg = Config::default();
        cfg.hide = vec![format!("{r}/Research/AI/AL")];
        let t = tiles(&cfg, &FocusHistory::default(), all.clone(), Some(&map));
        assert!(t.iter().all(|x| x.canonical != "Research/AI/AL"), "{:?}", t.iter().map(|x| &x.id).collect::<Vec<_>>());
        cfg.hide = vec!["Lab-OS".into()];
        let t = tiles(&cfg, &FocusHistory::default(), all, Some(&map));
        assert!(t.iter().all(|x| x.id != "map:Lab-OS"));

        // Keep mode ranks the map's vaults by the Obsidian vault in the same
        // folder: AL was used last, so it follows the lit fallback.
        let exo = keep::Exocortex::from_map(&map);
        let targets = exo.targets();
        let order = order_for_board(&targets, "Lab-OS", &exo, &[at("bbbb", format!("{r}/Research/AI/AL"), 9)], &Config::default(), &FocusHistory::default());
        let ids: Vec<&str> = order.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["Lab-OS", "Research/AI/AL", "Admin", "Learning"]);
        assert_eq!(keep_tile(&order[1]).qualifier, "Research/AI");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unhide_matches_a_path_exactly_and_a_name_everywhere() {
        let all = vec![
            vault("1", "/v/research/notes", false),
            vault("2", "/v/archive/notes", false),
            vault("3", "/v/research/admin", false),
        ];
        let hide: Vec<String> = ["/v/research/notes", "/v/archive/notes", "/gone/Index", "admin", "/v/research/notes"]
            .map(String::from)
            .into();
        assert_eq!(unhide_matches("notes", &hide, &all), ["/v/research/notes", "/v/archive/notes"]);
        assert_eq!(unhide_matches("/v/archive/notes/", &hide, &all), ["/v/archive/notes"]);
        assert_eq!(unhide_matches("Index", &hide, &all), ["/gone/Index"], "an entry Obsidian no longer knows");
        assert_eq!(unhide_matches("ADMIN", &hide, &all), ["admin"], "names match as Obsidian matches them");
        assert!(unhide_matches("nothing", &hide, &all).is_empty());
        assert!(unhide_matches("/v/research", &hide, &all).is_empty(), "a path matches only its own entry");
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
