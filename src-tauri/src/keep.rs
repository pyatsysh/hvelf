// keep: file a captured selection as a markdown note in a vault's keeps
// folder. Everything here is platform independent and unit tested; the
// capture itself lives in capture.rs on Windows (UI Automation, clipboard)
// and in linux.rs on Linux (the X11 PRIMARY selection).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------- registry

/// Where the vaults live, read from exocortex.toml. On Windows the file is
/// written on the WSL side with WSL paths, so `root` is translated for the
/// host; elsewhere it is used as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Exocortex {
    pub root: PathBuf,
    pub fallback: String,
    /// Vaults the exocortex never takes; the native `keep` refuses them too.
    pub excluded: Vec<String>,
    /// The vaults by canonical id when they come from Lab-OS's vault map;
    /// None for the registry reading, where they are the folders at the root.
    pub mapped: Option<Vec<Target>>,
    /// Why the vault map says nothing may be filed, when it says so.
    pub problem: Option<String>,
}

/// A vault a keep can go to.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    /// What the keep records as `vault:` and its path under the root: a
    /// canonical id such as `Research/AI/ML`, or a folder name at the root.
    pub id: String,
    /// Names a window title may carry for it.
    pub names: Vec<String>,
    /// In a bounded partition: searched only when named, never global.
    pub bounded: bool,
}

#[cfg(windows)]
pub const DEFAULT_ROOT: &str = r"D:\Dropbox\Vaults\Operational\research";
#[cfg(windows)]
pub const DEFAULT_REGISTRY: &str = r"D:\Dropbox\Vaults\Operational\registry\exocortex.toml";
/// Elsewhere the registry is Lab-OS's instance file in the selected release,
/// the one the native `keep` reads, and the root comes only from it: with
/// none named, nothing is filed.
#[cfg(not(windows))]
pub const DEFAULT_ROOT: &str = "";
#[cfg(not(windows))]
pub const DEFAULT_REGISTRY: &str = "/opt/labos/current/kernel/instance/exocortex.toml";
pub const DEFAULT_FALLBACK: &str = "exo";

#[derive(Deserialize)]
struct ExocortexFile {
    #[serde(default)]
    vaults: ExocortexVaults,
}

#[derive(Deserialize, Default)]
struct ExocortexVaults {
    #[serde(default)]
    root: String,
    #[serde(default)]
    fallback: String,
    #[serde(default)]
    excluded: Vec<String>,
}

/// Parse exocortex.toml. Missing keys fall back to the defaults; a file
/// that does not parse at all yields None so the caller can say so.
pub fn parse_exocortex(text: &str) -> Option<Exocortex> {
    let f: ExocortexFile = toml::from_str(text).ok()?;
    let root = if f.vaults.root.is_empty() {
        PathBuf::from(DEFAULT_ROOT)
    } else {
        host_path(&f.vaults.root)
    };
    let fallback = if f.vaults.fallback.is_empty() {
        DEFAULT_FALLBACK.to_string()
    } else {
        f.vaults.fallback
    };
    Some(Exocortex { root, fallback, excluded: f.vaults.excluded, mapped: None, problem: None })
}

pub fn load_exocortex(path: &Path) -> Exocortex {
    match fs::read_to_string(path) {
        Ok(text) => match parse_exocortex(&text) {
            Some(e) => e,
            None => {
                eprintln!("hvelf: {} does not parse; using defaults", path.display());
                Exocortex::default()
            }
        },
        Err(_) => Exocortex::default(),
    }
}

impl Default for Exocortex {
    fn default() -> Self {
        Exocortex {
            root: PathBuf::from(DEFAULT_ROOT),
            fallback: DEFAULT_FALLBACK.to_string(),
            excluded: Vec::new(),
            mapped: None,
            problem: None,
        }
    }
}

impl Exocortex {
    /// Keeps as Lab-OS's vault map lays them out: its root, its fallback and
    /// its vaults by canonical id. The map has already resolved the
    /// registry, so nothing here reads exocortex.toml.
    pub fn from_map(m: &crate::vaultmap::VaultMap) -> Exocortex {
        let targets = m
            .vaults
            .iter()
            .filter(|v| !v.excluded)
            .map(|v| Target { id: v.id.clone(), names: v.title_names(), bounded: v.bounded() })
            .collect();
        let problem = match &m.fallback {
            Some(_) => None,
            None if m.problems.is_empty() => Some("the vault map names no fallback vault".into()),
            None => Some(format!("the vault map names no fallback vault: {}", m.problems.join("; "))),
        };
        Exocortex {
            root: m.root.clone(),
            fallback: m.fallback.clone().unwrap_or_default(),
            excluded: Vec::new(),
            mapped: Some(targets),
            problem,
        }
    }

    /// The vaults a keep may go to, by id.
    pub fn targets(&self) -> Vec<Target> {
        match &self.mapped {
            Some(t) => t.clone(),
            None => self
                .vaults()
                .into_iter()
                .map(|n| Target { id: n.clone(), names: vec![n], bounded: false })
                .collect(),
        }
    }

    /// Why nothing can be filed here, if so. Asked before a capture is
    /// taken, so a broken registry costs the user nothing: the selection is
    /// still where it was.
    pub fn unusable(&self) -> Option<String> {
        if let Some(p) = &self.problem {
            return Some(p.clone());
        }
        if self.root.as_os_str().is_empty() {
            return Some("no vault root is named (no exocortex.toml, or one without [vaults] root)".into());
        }
        if !self.root.is_dir() {
            return Some(format!("the vault root {} does not exist", self.root.display()));
        }
        None
    }

    /// The vaults that take keeps: the folders under the root, less the
    /// excluded ones.
    pub fn vaults(&self) -> Vec<String> {
        list_vaults(&self.root)
            .into_iter()
            .filter(|v| !self.excluded.contains(v))
            .collect()
    }
}

/// The registry's root as a path on this host: translated from WSL on
/// Windows, as written everywhere else.
fn host_path(p: &str) -> PathBuf {
    if cfg!(windows) {
        wsl_to_native(p)
    } else {
        PathBuf::from(p)
    }
}

/// Turn a WSL mount path (/mnt/d/x/y) into the Windows path it names
/// (D:\x\y). Anything else is returned as given.
pub fn wsl_to_native(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("/mnt/") {
        let mut it = rest.splitn(2, '/');
        let drive = it.next().unwrap_or("");
        let tail = it.next().unwrap_or("");
        if drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic() {
            let mut s = String::new();
            s.push(drive.chars().next().unwrap().to_ascii_uppercase());
            s.push(':');
            s.push('\\');
            s.push_str(&tail.replace('/', "\\"));
            return PathBuf::from(s);
        }
    }
    PathBuf::from(p)
}

/// The vaults are the subfolders of the root, dotfiles excluded. A link to a
/// folder counts as one: native Lab-OS keeps vault aliases at the root as
/// symbolic links.
pub fn list_vaults(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = match fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort_by_key(|s| s.to_lowercase());
    v
}

/// Pick the vault whose name appears in the window title as a whole word
/// (a short name like "ML" must not fire on "Html"); the longest match
/// wins so "cfd_meshFit" beats "cFD". Case-insensitive.
#[cfg(test)]
pub fn preselect(title: &str, vaults: &[String]) -> Option<String> {
    let targets: Vec<Target> = vaults
        .iter()
        .map(|v| Target { id: v.clone(), names: vec![v.clone()], bounded: false })
        .collect();
    preselect_target(title, &targets)
}

/// The same over vaults with several names: the vault whose name or alias
/// is the longest whole word in the title, by its id.
pub fn preselect_target(title: &str, targets: &[Target]) -> Option<String> {
    let t = title.to_lowercase();
    targets
        .iter()
        .flat_map(|g| g.names.iter().map(move |n| (n, &g.id)))
        .filter(|(n, _)| !n.is_empty() && contains_word(&t, &n.to_lowercase()))
        .max_by_key(|(n, _)| n.len())
        .map(|(_, id)| id.clone())
}

fn contains_word(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + needle.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric()) && !after.is_some_and(|c| c.is_alphanumeric())
    })
}

// ---------------------------------------------------------- rendering

/// Stamp in the id: YYYYMMDDTHHMMSS local time.
pub fn new_id(stamp: &str, rand: u32) -> String {
    format!("{stamp}-{rand:08x}")
}

/// A JSON string literal, which is also a valid YAML scalar.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub struct Meta {
    pub id: String,
    /// RFC 3339 with the local offset, e.g. 2026-08-22T01:30:45+01:00.
    pub ts: String,
    pub vault: String,
    pub app: String,
    pub title: String,
    pub url: Option<String>,
}

/// The whole file in the native `keep` format: frontmatter, then the body
/// with LF line endings and one trailing newline, the sha256 covering the
/// body exactly as written. exo reads keeps with universal newlines, so a
/// CR kept in the body would read back as an altered keep.
pub fn render(meta: &Meta, body: &str) -> Vec<u8> {
    let mut b = body.replace("\r\n", "\n").replace('\r', "\n");
    if !b.ends_with('\n') {
        b.push('\n');
    }
    let sha = hex(&Sha256::digest(b.as_bytes()));
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", meta.id));
    out.push_str(&format!("ts: {}\n", meta.ts));
    out.push_str("source: selection\n");
    // Empty values are left out, as `keep` leaves them out.
    let fields = [("vault", Some(&meta.vault)), ("app", Some(&meta.app)), ("title", Some(&meta.title)), ("url", meta.url.as_ref())];
    for (k, v) in fields {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            out.push_str(&format!("{k}: {}\n", scalar(v)));
        }
    }
    out.push_str(&format!("sha256: {sha}\n"));
    out.push_str("---\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(b.as_bytes());
    bytes
}

/// A frontmatter value: bare when it is a plain token, as the native `keep`
/// writes it, else a JSON string literal, which YAML reads the same way.
fn scalar(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_./:+-".contains(c)) {
        s.to_string()
    } else {
        json_escape(s)
    }
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// <root>/<vault>/keeps/<YYYY-MM>/<id>.md
pub fn keep_path(root: &Path, vault: &str, month: &str, id: &str) -> PathBuf {
    root.join(vault).join("keeps").join(month).join(format!("{id}.md"))
}

/// <root>/<fallback>/keeps/unrouted/<id>.md
pub fn unrouted_path(root: &Path, fallback: &str, id: &str) -> PathBuf {
    root.join(fallback).join("keeps").join("unrouted").join(format!("{id}.md"))
}

/// Write a keep under `<root>/<vault>/` only when that vault's folder is
/// there. A capture never creates a vault: a vanished or misnamed one fails
/// loudly, as the native `keep` refuses a vault that does not exist. The
/// vault is a plain path under the root (`Research/AI/ML`), and a link that
/// leads out of the root is refused, not followed.
pub fn write_in_vault(root: &Path, vault: &str, path: &Path, bytes: &[u8]) -> io::Result<()> {
    if !crate::vaultmap::plain_relative(vault) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{vault:?} is not a vault under the root")));
    }
    let dir = root.join(vault);
    if !dir.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no vault folder at {}", dir.display())));
    }
    if !fs::canonicalize(&dir)?.starts_with(fs::canonicalize(root)?) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} leads out of the vault root", dir.display())));
    }
    write_atomic(path, bytes)
}

/// Write via a sibling `.part` file and rename, as `keep` does, so neither a
/// sync client nor the indexer ever sees a half-written note.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    fs::create_dir_all(dir)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// The first line of the body, shortened for a banner.
pub fn preview(body: &str, max: usize) -> String {
    let first = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut s: String = first.chars().take(max).collect();
    if first.chars().count() > max {
        s.push('\u{2026}');
    }
    s
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn id_shape() {
        let id = new_id("20260822T013045", 0x3f9a1c7e);
        assert_eq!(id, "20260822T013045-3f9a1c7e");
        assert_eq!(new_id("20260822T013045", 7), "20260822T013045-00000007");
    }

    #[test]
    fn wsl_paths_become_drive_paths() {
        assert_eq!(
            wsl_to_native("/mnt/d/Dropbox/Vaults/Operational/research"),
            PathBuf::from(r"D:\Dropbox\Vaults\Operational\research")
        );
        assert_eq!(wsl_to_native("/mnt/c"), PathBuf::from(r"C:\"));
        assert_eq!(wsl_to_native(r"D:\x"), PathBuf::from(r"D:\x"));
        assert_eq!(wsl_to_native("/home/x"), PathBuf::from("/home/x"));
        assert_eq!(wsl_to_native("/mnt/wsl/x"), PathBuf::from("/mnt/wsl/x"));
    }

    #[test]
    fn toml_parses_and_defaults() {
        let text = r#"
[vaults]
root = "/mnt/d/Dropbox/Vaults/Operational/research"
bounded = ["admin", "career-vault"]
fallback = "exo"
excluded = []

[index]
dir = "/home/py/.local/state/exo"
threshold = 0.45
"#;
        let e = parse_exocortex(text).unwrap();
        #[cfg(windows)]
        assert_eq!(e.root, PathBuf::from(r"D:\Dropbox\Vaults\Operational\research"));
        #[cfg(not(windows))]
        assert_eq!(e.root, PathBuf::from("/mnt/d/Dropbox/Vaults/Operational/research"));
        assert_eq!(e.fallback, "exo");
        assert!(e.excluded.is_empty());

        let e = parse_exocortex("[vaults]\nroot = \"/v\"\nexcluded = [\"archive\"]\n").unwrap();
        assert_eq!(e.excluded, ["archive"]);

        let e = parse_exocortex("[index]\ndir = \"x\"\n").unwrap();
        assert_eq!(e, Exocortex::default());
        assert_eq!(parse_exocortex("[vaults\nroot = 3"), None);
    }

    #[test]
    fn preselect_longest_case_insensitive() {
        let vaults = v(&["ML", "cFD", "cfd_meshFit", "exo", "orbit"]);
        assert_eq!(
            preselect("notes.md - cfd_meshfit - Obsidian v1.6", &vaults),
            Some("cfd_meshFit".into())
        );
        assert_eq!(preselect("Results - CFD - Obsidian", &vaults), Some("cFD".into()));
        assert_eq!(preselect("Calculus of variations", &vaults), None);
        assert_eq!(preselect("Html notes - Obsidian v1.6", &vaults), None);
        assert_eq!(preselect("notes - ML - Obsidian v1.6", &vaults), Some("ML".into()));
        assert_eq!(preselect("(ml) machine learning", &vaults), Some("ML".into()));
        assert_eq!(preselect("Untitled - Notepad", &vaults), None);
        assert_eq!(preselect("anything", &[]), None);
    }

    #[test]
    fn json_escaping() {
        assert_eq!(json_escape("plain"), "\"plain\"");
        assert_eq!(json_escape("a \"q\" \\ b"), "\"a \\\"q\\\" \\\\ b\"");
        assert_eq!(json_escape("l1\nl2\t\u{1}"), "\"l1\\nl2\\t\\u0001\"");
        assert_eq!(json_escape("caf\u{e9} \u{2014}"), "\"caf\u{e9} \u{2014}\"");
    }

    #[test]
    fn frontmatter_exact() {
        let meta = Meta {
            id: "20260822T013045-3f9a1c7e".into(),
            ts: "2026-08-22T01:30:45+01:00".into(),
            vault: "ML".into(),
            app: "chrome.exe".into(),
            title: "A \"quoted\" title".into(),
            url: Some("https://example.org/x?y=1".into()),
        };
        let out = render(&meta, "hello world");
        let s = String::from_utf8(out).unwrap();
        let expect = "---\n\
id: 20260822T013045-3f9a1c7e\n\
ts: 2026-08-22T01:30:45+01:00\n\
source: selection\n\
vault: ML\n\
app: chrome.exe\n\
title: \"A \\\"quoted\\\" title\"\n\
url: \"https://example.org/x?y=1\"\n\
sha256: a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447\n\
---\n\
hello world\n";
        assert_eq!(s, expect);
    }

    #[test]
    fn odd_names_are_quoted_plain_ones_bare() {
        let meta = Meta {
            id: "i".into(),
            ts: "t".into(),
            vault: "Lab notes".into(),
            app: "pid 7: x".into(),
            title: "".into(),
            url: None,
        };
        let s = String::from_utf8(render(&meta, "x")).unwrap();
        assert!(s.contains("\nvault: \"Lab notes\"\n"), "{s}");
        assert!(s.contains("\napp: \"pid 7: x\"\n"), "{s}");
        assert_eq!(scalar("gnome-terminal-server"), "gnome-terminal-server");
        assert_eq!(scalar(""), "\"\"");
    }

    #[test]
    fn body_gets_lf_endings_and_one_trailing_newline() {
        let meta = Meta {
            id: "i".into(),
            ts: "t".into(),
            vault: "v".into(),
            app: "a".into(),
            title: "".into(),
            url: None,
        };
        // A body that already ends in a newline is not given another, and
        // CRs become LFs, as `keep` writes them.
        let s = String::from_utf8(render(&meta, "  two\r\nlines \rthree\n")).unwrap();
        assert!(s.ends_with("---\n  two\nlines \nthree\n"), "{s}");
        assert!(!s.contains("url:") && !s.contains("title:"), "empty values are left out");
        let line = s.lines().find(|l| l.starts_with("sha256: ")).unwrap();
        assert_eq!(line, format!("sha256: {}", hex(&Sha256::digest(b"  two\nlines \nthree\n"))));
        // The hash covers the body as written, newline included.
        let s2 = String::from_utf8(render(&meta, "x")).unwrap();
        let line = s2.lines().find(|l| l.starts_with("sha256: ")).unwrap();
        assert_eq!(line, format!("sha256: {}", hex(&Sha256::digest(b"x\n"))));
    }

    #[test]
    fn paths() {
        let root = Path::new(r"D:\root");
        assert_eq!(
            keep_path(root, "ML", "2026-08", "id1"),
            PathBuf::from(r"D:\root").join("ML").join("keeps").join("2026-08").join("id1.md")
        );
        assert_eq!(
            unrouted_path(root, "exo", "id1"),
            PathBuf::from(r"D:\root").join("exo").join("keeps").join("unrouted").join("id1.md")
        );
    }

    #[test]
    fn atomic_write_roundtrip() {
        let dir = std::env::temp_dir().join(format!("hvelf-keep-test-{}", std::process::id()));
        let p = dir.join("a").join("b.md");
        write_atomic(&p, b"body\n").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"body\n");
        assert!(!dir.join("a").join("b.md.part").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hvelf-keep-{tag}-{} é", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    #[test]
    fn linked_vaults_count_and_excluded_ones_do_not() {
        let root = scratch("list");
        let elsewhere = scratch("list-target");
        fs::create_dir_all(root.join("exo")).unwrap();
        fs::create_dir_all(root.join("archive")).unwrap();
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::write(root.join("notes.md"), "x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("ML")).unwrap();
        std::os::unix::fs::symlink(root.join("gone"), root.join("dangling")).unwrap();
        assert_eq!(list_vaults(&root), ["archive", "exo", "ML"]);
        let exo = Exocortex { root: root.clone(), fallback: "exo".into(), excluded: vec!["archive".into()], ..Exocortex::default() };
        assert_eq!(exo.vaults(), ["exo", "ML"]);
        assert_eq!(exo.unusable(), None);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn an_unusable_registry_says_why() {
        let none = Exocortex { root: PathBuf::new(), fallback: "exo".into(), excluded: vec![], ..Exocortex::default() };
        assert!(none.unusable().unwrap().contains("no vault root"));
        let gone = Exocortex { root: scratch("gone").join("not-there"), ..none.clone() };
        assert!(gone.unusable().unwrap().contains("does not exist"));
        #[cfg(not(windows))]
        assert_eq!(Exocortex::default().unusable(), none.unusable());
    }

    #[test]
    fn a_capture_never_creates_a_vault() {
        let root = scratch("write");
        fs::create_dir_all(root.join("ML")).unwrap();
        let into = |v: &str| keep_path(&root, v, "2026-10", "id1");
        write_in_vault(&root, "ML", &into("ML"), b"body\n").unwrap();
        assert_eq!(fs::read(into("ML")).unwrap(), b"body\n");
        let e = write_in_vault(&root, "Nope", &into("Nope"), b"x").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(!root.join("Nope").exists());
        // A canonical id is a path under the root.
        fs::create_dir_all(root.join("Research/AI/ML")).unwrap();
        write_in_vault(&root, "Research/AI/ML", &into("Research/AI/ML"), b"x\n").unwrap();
        assert!(root.join("Research/AI/ML/keeps/2026-10/id1.md").is_file());
        let e = write_in_vault(&root, "Research/AI/XX", &into("Research/AI/XX"), b"x").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        for bad in ["", ".", "..", "../x", "/abs", "a//b", "a/../b", "a\\b"] {
            let e = write_in_vault(&root, bad, &root.join("x.md"), b"x").unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        #[cfg(unix)]
        {
            let outside = scratch("outside");
            std::os::unix::fs::symlink(&outside, root.join("Out")).unwrap();
            let e = write_in_vault(&root, "Out", &into("Out"), b"x").unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
            assert!(!outside.join("keeps").exists());
            let _ = fs::remove_dir_all(&outside);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn titles_pick_vaults_by_any_of_their_names() {
        let t = |id: &str, names: &[&str]| Target { id: id.into(), names: names.iter().map(|s| s.to_string()).collect(), bounded: false };
        let targets = [
            t("Research/AI/ML", &["ML"]),
            t("Lab-OS", &["Lab-OS", "old-notes", "labos"]),
            t("Research", &["Research"]),
        ];
        assert_eq!(preselect_target("notes - ML - Obsidian v1.6.7", &targets).as_deref(), Some("Research/AI/ML"));
        assert_eq!(preselect_target("labos - Visual Studio Code", &targets).as_deref(), Some("Lab-OS"));
        assert_eq!(preselect_target("ML Research plan", &targets).as_deref(), Some("Research"), "longest name wins");
        assert_eq!(preselect_target("Html", &targets), None);
    }

    #[test]
    fn preview_first_nonblank_line() {
        assert_eq!(preview("\n\n  hello there  \nmore", 20), "hello there");
        assert_eq!(preview("abcdefghij", 4), "abcd\u{2026}");
        assert_eq!(preview("   ", 4), "");
    }
}
