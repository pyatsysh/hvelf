// vaultmap: the canonical vault map that Lab-OS's exo derives and prints
// with `exo vaults --json` (schema labos-vault-map/v1). One entry per vault,
// keyed by its path under the vault root after links resolve, with the names
// that alias it and the partition it belongs to. hvelf reads the map and
// never derives one of its own, so the board, the keep chord and exo agree
// on what a vault is: a link such as ~/Lab/Vaults/ML is a name for
// Research/AI/ML, not a second vault, and a bounded vault stays bounded.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

pub const SCHEMA: &str = "labos-vault-map/v1";

#[derive(Deserialize)]
struct MapFile {
    schema: String,
    root: String,
    #[serde(default)]
    fallback: Option<String>,
    vaults: Vec<EntryFile>,
    #[serde(default)]
    problems: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct EntryFile {
    id: String,
    path: String,
    #[serde(default)]
    partition: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    bounded: Option<bool>,
    #[serde(default)]
    excluded: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapVault {
    /// The path under the root after links resolve, e.g. `Research/AI/ML`.
    pub id: String,
    pub path: PathBuf,
    /// `global`, or the bounded partition the vault sits in (`Admin`).
    pub partition: String,
    /// Other names that resolve to this vault: links, old flat names,
    /// catalogue ids.
    pub aliases: Vec<String>,
    /// In a bounded partition: searched only when named and never in the
    /// global index. The board marks it.
    pub bounded: bool,
    /// Never takes a keep.
    pub excluded: bool,
}

impl MapVault {
    pub fn bounded(&self) -> bool {
        self.bounded
    }

    /// The last part of the id: what Obsidian calls the vault.
    pub fn name(&self) -> &str {
        self.id.rsplit('/').next().unwrap_or(&self.id)
    }

    /// The names a window title may carry for this vault: its own name and
    /// the plain aliases (a path-like alias is no word in a title).
    pub fn title_names(&self) -> Vec<String> {
        let mut out = vec![self.name().to_string()];
        for a in &self.aliases {
            if !a.contains('/') && !out.contains(a) {
                out.push(a.clone());
            }
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VaultMap {
    pub root: PathBuf,
    /// The fallback vault as a canonical id, when the map names one.
    pub fallback: Option<String>,
    pub vaults: Vec<MapVault>,
    /// What the map itself reports as wrong, e.g. a fallback that does not
    /// resolve.
    pub problems: Vec<String>,
}

/// An id is a relative path of plain components; anything else could write
/// outside the root.
pub fn plain_relative(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('/')
        && !id.contains('\\')
        && id.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

pub fn parse(text: &str) -> Result<VaultMap, String> {
    let f: MapFile = serde_json::from_str(text).map_err(|e| format!("the vault map is not JSON of the expected shape: {e}"))?;
    if f.schema != SCHEMA {
        return Err(format!("the vault map's schema is {:?}, not {SCHEMA}", f.schema));
    }
    let root = PathBuf::from(&f.root);
    if !root.is_absolute() {
        return Err(format!("the vault map's root {:?} is not absolute", f.root));
    }
    let mut vaults: Vec<MapVault> = Vec::new();
    for e in f.vaults {
        if !plain_relative(&e.id) {
            return Err(format!("vault id {:?} is not a plain path under the root", e.id));
        }
        if vaults.iter().any(|v| v.id == e.id) {
            return Err(format!("vault id {:?} appears twice", e.id));
        }
        let path = PathBuf::from(&e.path);
        if !path.is_absolute() {
            return Err(format!("vault {:?} has a relative path {:?}", e.id, e.path));
        }
        let bounded = e.bounded.unwrap_or(!e.partition.is_empty() && e.partition != "global");
        vaults.push(MapVault { id: e.id, path, partition: e.partition, aliases: e.aliases, bounded, excluded: e.excluded });
    }
    let mut problems: Vec<String> = f
        .problems
        .iter()
        .map(|p| p.as_str().map(String::from).unwrap_or_else(|| p.to_string()))
        .collect();
    // Only a fallback that fails its own checks is dropped; the map may
    // report other problems (an alias not yet placed) beside a good one.
    let mut fallback = f.fallback;
    if let Some(fb) = fallback.clone() {
        let bad = match vaults.iter().find(|v| v.id == fb) {
            None => Some(format!("the fallback {fb:?} is not one of the map's vaults")),
            Some(v) if v.bounded || v.excluded => Some(format!("the fallback {fb:?} is bounded or excluded")),
            Some(_) => None,
        };
        if let Some(bad) = bad {
            problems.push(bad);
            fallback = None;
        }
    }
    Ok(VaultMap { root, fallback, vaults, problems })
}

fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl VaultMap {
    /// The map's folders resolved once, for many lookups.
    pub fn locator(&self) -> Locator<'_> {
        Locator { real: self.vaults.iter().map(|v| (real(&v.path), v)).collect() }
    }
}

pub struct Locator<'a> {
    real: Vec<(PathBuf, &'a MapVault)>,
}

impl<'a> Locator<'a> {
    pub fn at(&self, dir: &Path) -> Option<&'a MapVault> {
        let want = real(dir);
        self.real.iter().find(|(p, _)| *p == want).map(|(_, v)| *v)
    }
}

/// Run the map command and read its output, within `timeout`: it is another
/// program, and a board must not hang on it.
pub fn load(argv: &[String], timeout: Duration) -> Result<VaultMap, String> {
    let (prog, args) = argv.split_first().ok_or("no vault map command is configured")?;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {prog}: {e}"))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut err).take(4096).read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} did not answer within {} s", argv.join(" "), timeout.as_secs()));
            }
            Err(e) => return Err(format!("cannot wait for {prog}: {e}")),
        }
    };
    let text = reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("{} exited with {status}: {}", argv.join(" "), err.trim()));
    }
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture root: absolute on every platform, since `parse` refuses a
    /// root that is not, and `/v` is not absolute on Windows.
    const ROOT: &str = if cfg!(windows) { r"C:\v" } else { "/v" };

    pub fn sample(root: &str) -> String {
        serde_json::json!({
            "schema": SCHEMA,
            "root": root,
            "fallback": "Lab-OS",
            "vaults": [
                {"id": "Admin", "path": format!("{root}/Admin"), "partition": "Admin", "bounded": true, "aliases": ["admin"]},
                {"id": "Admin/matter-a", "path": format!("{root}/Admin/matter-a"), "partition": "Admin", "aliases": []},
                {"id": "Lab-OS", "path": format!("{root}/Lab-OS"), "partition": "global", "aliases": ["Research/AI/old-notes", "old-notes", "labos"]},
                {"id": "Research/AI/ML", "path": format!("{root}/Research/AI/ML"), "partition": "global", "aliases": ["ML"]},
                {"id": "Research/CFD/fluids-lab", "path": format!("{root}/Research/CFD/fluids-lab"), "partition": "global", "aliases": ["cFD", "cfd"]},
            ]
        })
        .to_string()
    }

    fn by<'a>(m: &'a VaultMap, name: &str) -> &'a MapVault {
        m.vaults.iter().find(|v| v.id == name || v.aliases.iter().any(|a| a == name)).unwrap()
    }

    #[test]
    fn a_map_parses_with_partitions_and_names() {
        let m = parse(&sample(ROOT)).unwrap();
        assert_eq!(m.vaults.len(), 5);
        assert_eq!(by(&m, "ML").id, "Research/AI/ML");
        assert_eq!(by(&m, "ML").path, PathBuf::from(format!("{ROOT}/Research/AI/ML")));
        assert!(by(&m, "Admin").bounded() && by(&m, "admin").bounded());
        assert!(!by(&m, "ML").bounded());
        assert_eq!(by(&m, "ML").name(), "ML");
        assert_eq!(by(&m, "Lab-OS").title_names(), ["Lab-OS", "old-notes", "labos"]);
    }

    #[test]
    fn the_fallback_must_be_a_general_vault_of_the_map() {
        let m = parse(&sample(ROOT)).unwrap();
        assert_eq!(m.fallback.as_deref(), Some("Lab-OS"));
        assert!(m.problems.is_empty());
        assert!(by(&m, "Admin/matter-a").bounded(), "bounded from its partition alone");
        let m = parse(&sample(ROOT).replace("\"fallback\":\"Lab-OS\"", "\"fallback\":\"Admin\"")).unwrap();
        assert_eq!(m.fallback, None);
        assert!(m.problems[0].contains("bounded"), "{:?}", m.problems);
        let m = parse(&sample(ROOT).replace("\"fallback\":\"Lab-OS\"", "\"fallback\":null,\"problems\":[\"exo does not resolve\"]")).unwrap();
        assert_eq!((m.fallback, m.problems), (None, vec!["exo does not resolve".to_string()]));
        // The live map of 2026-10-04 reports aliases MIGRATION has not placed
        // yet beside a good fallback: the fallback stands.
        let m = parse(&sample(ROOT).replace("\"fallback\":\"Lab-OS\"", "\"fallback\":\"Lab-OS\",\"problems\":[\"alias exo points to Operational/exo, which does not exist\"]")).unwrap();
        assert_eq!(m.fallback.as_deref(), Some("Lab-OS"));
        assert_eq!(m.problems.len(), 1);
    }

    #[test]
    fn malformed_maps_are_refused() {
        assert!(parse("{}").is_err());
        let wrong = sample(ROOT).replace(SCHEMA, "labos-vault-map/v0");
        assert!(parse(&wrong).unwrap_err().contains("schema"));
        assert!(parse(&sample("relative")).unwrap_err().contains("not absolute"));
        for bad in ["../x", "/abs", "a//b", "a/./b", "a\\b", ""] {
            let m = sample(ROOT).replace("\"Admin/matter-a\"", &serde_json::to_string(bad).unwrap());
            assert!(parse(&m).is_err(), "{bad:?}");
        }
        let twice = sample(ROOT).replace("\"Admin/matter-a\"", "\"Admin\"");
        assert!(parse(&twice).unwrap_err().contains("twice"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_the_vault_it_points_at() {
        let root = std::env::temp_dir().join(format!("hvelf-map-{} é", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Research/AI/ML")).unwrap();
        std::os::unix::fs::symlink(root.join("Research/AI/ML"), root.join("ML")).unwrap();
        let m = parse(&sample(&root.display().to_string())).unwrap();
        let at = m.locator();
        assert_eq!(at.at(&root.join("ML")).unwrap().id, "Research/AI/ML");
        assert_eq!(at.at(&root.join("Research/AI/ML/")).unwrap().id, "Research/AI/ML");
        assert!(at.at(&root.join("Research")).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn the_command_is_bounded_and_its_failures_named() {
        let sh = |script: &str| vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
        let m = load(&sh(&format!("printf '%s' '{}'", sample(ROOT))), Duration::from_secs(5)).unwrap();
        assert_eq!(m.vaults.len(), 5);
        let e = load(&sh("echo broken >&2; exit 3"), Duration::from_secs(5)).unwrap_err();
        assert!(e.contains("broken"), "{e}");
        let t = Instant::now();
        let e = load(&sh("sleep 5"), Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("did not answer") && t.elapsed() < Duration::from_secs(3), "{e}");
        assert!(load(&["/no/such/exo".to_string()], Duration::from_secs(1)).is_err());
        assert!(load(&[], Duration::from_secs(1)).is_err());
    }
}
