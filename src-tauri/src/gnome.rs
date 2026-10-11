//! GNOME's key bindings, as far as they concern hvelf's chords.
//!
//! On GNOME a chord can be bound by the desktop itself: a custom shortcut
//! (Settings, Keyboard, Custom Shortcuts) that runs `hvelf --toggle`, or one of
//! GNOME's own bindings. When GNOME and hvelf both grab the same chord on X11,
//! the X server gives each press to whichever grabbed it last, so which of the
//! two answers would change with the order they started in. hvelf therefore
//! leaves to GNOME every chord GNOME already binds, and says so in its
//! capability records.
//!
//! The bindings are read with `gsettings`, which reads the dconf database
//! directly; nothing here writes a setting.

use std::process::Command;
use std::time::Duration;

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";

/// GNOME's own binding schemas whose accelerators can clash with a chord.
const BUILTIN: [&str; 5] = [
    "org.gnome.desktop.wm.keybindings",
    "org.gnome.shell.keybindings",
    "org.gnome.mutter.keybindings",
    "org.gnome.mutter.wayland.keybindings",
    MEDIA_KEYS,
];

/// One custom shortcut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub path: String,
    pub name: String,
    pub command: String,
    pub binding: String,
}

/// Who holds a chord on the desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// A custom shortcut that runs hvelf: GNOME handles the chord for hvelf.
    Hvelf { command: String },
    /// A custom shortcut for another command, or one of GNOME's own bindings.
    Other { what: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bindings {
    pub custom: Vec<Shortcut>,
    /// (schema, key, accelerators) of GNOME's own bindings.
    pub builtin: Vec<(String, String, Vec<String>)>,
}

/// Whether the session is a GNOME one, by `XDG_CURRENT_DESKTOP` (`ubuntu:GNOME`, `GNOME`, `GNOME-Classic:GNOME`).
pub fn is_gnome_from(current_desktop: Option<&str>) -> bool {
    current_desktop.is_some_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("gnome") || p.to_ascii_lowercase().starts_with("gnome-")))
}

fn gsettings(args: &[&str]) -> Result<String, String> {
    let out = Command::new("gsettings").args(args).output().map_err(|e| format!("gsettings could not be run: {e}"))?;
    if !out.status.success() {
        return Err(format!("gsettings {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// GNOME's custom shortcuts and its own bindings, or why they cannot be read. Bounded, so a stuck `gsettings`
/// never holds up the board.
pub fn read() -> Result<Bindings, String> {
    if !is_gnome_from(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref()) {
        return Err("not a GNOME session".into());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_now());
    });
    rx.recv_timeout(Duration::from_secs(4)).unwrap_or_else(|_| Err("gsettings did not answer within 4 s".into()))
}

fn read_now() -> Result<Bindings, String> {
    let list = gsettings(&["get", MEDIA_KEYS, "custom-keybindings"])?;
    let paths = parse_strv(&list).ok_or_else(|| format!("custom-keybindings is not a list of paths: {}", list.trim()))?;
    let mut custom = Vec::new();
    for path in paths {
        let schema = format!("{MEDIA_KEYS}.custom-keybinding:{path}");
        let get = |key: &str| gsettings(&["get", &schema, key]).ok().and_then(|v| parse_str(&v)).unwrap_or_default();
        custom.push(Shortcut { name: get("name"), command: get("command"), binding: get("binding"), path });
    }
    let mut builtin = Vec::new();
    for schema in BUILTIN {
        let Ok(all) = gsettings(&["list-recursively", schema]) else { continue };
        for line in all.lines() {
            let mut parts = line.splitn(3, ' ');
            let (Some(s), Some(key), Some(value)) = (parts.next(), parts.next(), parts.next()) else { continue };
            if s == MEDIA_KEYS && key == "custom-keybindings" {
                continue;
            }
            let accels = parse_strv(value).or_else(|| parse_str(value).map(|v| vec![v])).unwrap_or_default();
            if !accels.is_empty() {
                builtin.push((s.to_string(), key.to_string(), accels));
            }
        }
    }
    Ok(Bindings { custom, builtin })
}

impl Bindings {
    /// Who on the desktop holds `chord` (hvelf's spelling, e.g. `ctrl+alt+grave`), if anyone.
    pub fn owner_of(&self, chord: &str) -> Option<Owner> {
        let want = normalize_chord(chord)?;
        for s in &self.custom {
            if accel_to_chord(&s.binding).as_deref() == Some(want.as_str()) {
                return Some(if hvelf_action(&s.command).is_some() {
                    Owner::Hvelf { command: s.command.trim().to_string() }
                } else {
                    Owner::Other { what: format!("the GNOME custom shortcut '{}'", s.name) }
                });
            }
        }
        for (schema, key, accels) in &self.builtin {
            if accels.iter().any(|a| accel_to_chord(a).as_deref() == Some(want.as_str())) {
                return Some(Owner::Other { what: format!("GNOME's own shortcut {key} ({schema})") });
            }
        }
        None
    }

    /// The bound custom shortcuts that run hvelf, as (the chord as a person writes it, the command).
    pub fn hvelf_shortcuts(&self) -> Vec<(String, String)> {
        self.custom
            .iter()
            .filter(|s| hvelf_action(&s.command).is_some())
            .filter_map(|s| Some((pretty(&accel_to_chord(&s.binding)?), s.command.trim().to_string())))
            .collect()
    }
}

/// The hvelf action a desktop command runs: `Some("--toggle")` for `hvelf --toggle` or `/usr/bin/hvelf --toggle`,
/// `Some("")` for a bare `hvelf`, `None` for any other command.
pub fn hvelf_action(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace();
    let exe = words.next()?;
    if exe.rsplit('/').next() != Some("hvelf") {
        return None;
    }
    match (words.next(), words.next()) {
        (None, _) => Some(""),
        (Some(a), None) if a.starts_with("--") => Some(a),
        _ => None,
    }
}

const MOD_ORDER: [&str; 4] = ["ctrl", "alt", "shift", "super"];

fn key_word(k: &str) -> Option<String> {
    let k = k.trim();
    let lower = k.to_ascii_lowercase();
    match lower.as_str() {
        "grave" | "above_tab" | "quoteleft" | "backquote" | "`" => return Some("grave".into()),
        _ => {}
    }
    let lower = lower.strip_prefix("digit").or_else(|| lower.strip_prefix("key")).filter(|r| !r.is_empty()).unwrap_or(&lower).to_string();
    if lower.chars().count() == 1 && lower.chars().next()?.is_ascii_alphanumeric() {
        return Some(lower);
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then(|| format!("f{n}"));
    }
    (!lower.is_empty()).then_some(lower)
}

fn join(mods: &[&str], key: &str) -> String {
    let mut out: Vec<&str> = MOD_ORDER.iter().copied().filter(|m| mods.contains(m)).collect();
    out.push(key);
    out.join("+")
}

/// hvelf's chord spelling in one canonical order, e.g. `Alt+Ctrl+Grave` and `ctrl+alt+grave` both give
/// `ctrl+alt+grave`. None when it names no key.
pub fn normalize_chord(spec: &str) -> Option<String> {
    let mut mods = Vec::new();
    let mut key = None;
    for tok in spec.split('+') {
        let t = tok.trim().to_ascii_lowercase();
        match t.as_str() {
            "ctrl" | "control" => mods.push("ctrl"),
            "alt" => mods.push("alt"),
            "shift" => mods.push("shift"),
            "win" | "super" | "meta" => mods.push("super"),
            "" => return None,
            k => key = Some(key_word(k)?),
        }
    }
    key.map(|k| join(&mods, &k))
}

/// A GTK accelerator (`<Control><Alt>grave`, `<Primary><Alt>Above_Tab`, `<Alt>k`) in hvelf's canonical chord
/// spelling. None for an empty, disabled or unparsable binding, or one with a modifier hvelf's chords cannot hold.
pub fn accel_to_chord(accel: &str) -> Option<String> {
    let mut rest = accel.trim();
    if rest.is_empty() || rest.eq_ignore_ascii_case("disabled") {
        return None;
    }
    let mut mods = Vec::new();
    while let Some(r) = rest.strip_prefix('<') {
        let end = r.find('>')?;
        match r[..end].to_ascii_lowercase().as_str() {
            "control" | "ctrl" | "ctl" | "primary" => mods.push("ctrl"),
            "alt" | "mod1" => mods.push("alt"),
            "shift" | "shft" => mods.push("shift"),
            "super" | "mod4" | "meta" | "hyper" => mods.push("super"),
            "release" => {}
            _ => return None,
        }
        rest = &r[end + 1..];
    }
    Some(join(&mods, &key_word(rest)?))
}

/// The chord as a person writes it: `Ctrl+Alt+``, `Alt+K`, `Ctrl+Alt+1`, `Super+F11`.
pub fn pretty(canonical: &str) -> String {
    canonical
        .split('+')
        .map(|t| match t {
            "ctrl" => "Ctrl".to_string(),
            "alt" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "super" => "Super".to_string(),
            "grave" => "`".to_string(),
            k => k.to_ascii_uppercase(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// One GVariant text-format string (`'…'` or `"…"`, with GLib's escapes) at the start of `s`, and what follows.
fn take_str(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    let quote = s.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let mut out = String::new();
    let mut it = s.char_indices().skip(1);
    while let Some((i, c)) = it.next() {
        if c == quote {
            return Some((out, &s[i + 1..]));
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        let (_, e) = it.next()?;
        match e {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'u' | 'U' => {
                let n = if e == 'u' { 4 } else { 8 };
                let hex: String = (0..n).map(|_| it.next().map(|(_, h)| h)).collect::<Option<String>>()?;
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            other => out.push(other),
        }
    }
    None
}

/// A GVariant string as `gsettings get` prints it.
pub fn parse_str(s: &str) -> Option<String> {
    let (v, rest) = take_str(s)?;
    rest.trim().is_empty().then_some(v)
}

/// A GVariant string array as `gsettings get` prints it: `['a', 'b']`, `[]` or `@as []`.
pub fn parse_strv(s: &str) -> Option<Vec<String>> {
    let s = s.trim();
    let s = s.strip_prefix("@as").map(str::trim_start).unwrap_or(s);
    let mut s = s.strip_prefix('[')?.trim_start();
    let mut out = Vec::new();
    if let Some(r) = s.strip_prefix(']') {
        return r.trim().is_empty().then_some(out);
    }
    loop {
        let (v, rest) = take_str(s)?;
        out.push(v);
        let rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix(',') {
            s = r.trim_start();
        } else {
            let r = rest.strip_prefix(']')?;
            return r.trim().is_empty().then_some(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gvariant_strings_and_lists_as_gsettings_prints_them() {
        assert_eq!(parse_str("'hvelf --toggle'\n").as_deref(), Some("hvelf --toggle"));
        assert_eq!(parse_str("\"it's mine\"").as_deref(), Some("it's mine"));
        assert_eq!(parse_str(r"'a \'quoted\' word \\ é'").as_deref(), Some("a 'quoted' word \\ é"));
        assert_eq!(parse_str("'unterminated"), None);
        assert_eq!(parse_str("'a' trailing"), None);
        assert_eq!(parse_strv("@as []\n"), Some(vec![]));
        assert_eq!(parse_strv("[]"), Some(vec![]));
        assert_eq!(
            parse_strv("['/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/hvelf-toggle/', '/x/custom0/']"),
            Some(vec!["/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/hvelf-toggle/".to_string(), "/x/custom0/".to_string()])
        );
        assert_eq!(parse_strv("['<Super>Above_Tab', '<Alt>Above_Tab']").map(|v| v.len()), Some(2));
        assert_eq!(parse_strv("['a' 'b']"), None);
        assert_eq!(parse_strv("'not a list'"), None);
    }

    #[test]
    fn accelerators_and_chords_meet_in_one_spelling() {
        assert_eq!(accel_to_chord("<Control><Alt>grave").as_deref(), Some("ctrl+alt+grave"));
        assert_eq!(accel_to_chord("<Primary><Alt>Above_Tab").as_deref(), Some("ctrl+alt+grave"));
        assert_eq!(accel_to_chord("<Alt><Ctrl>1").as_deref(), Some("ctrl+alt+1"));
        assert_eq!(accel_to_chord("<Alt>K").as_deref(), Some("alt+k"));
        assert_eq!(accel_to_chord("<Super>F11").as_deref(), Some("super+f11"));
        assert_eq!(accel_to_chord("<Alt>Above_Tab").as_deref(), Some("alt+grave"));
        assert_eq!(accel_to_chord(""), None);
        assert_eq!(accel_to_chord("disabled"), None);
        assert_eq!(accel_to_chord("<Mod5>grave"), None, "a modifier hvelf cannot hold matches nothing");
        assert_eq!(accel_to_chord("<Control>"), None);
        assert_eq!(normalize_chord("ctrl+alt+grave").as_deref(), Some("ctrl+alt+grave"));
        assert_eq!(normalize_chord("Alt+Ctrl+`").as_deref(), Some("ctrl+alt+grave"));
        assert_eq!(normalize_chord("alt+KeyK").as_deref(), Some("alt+k"));
        assert_eq!(normalize_chord("ctrl+alt+Digit1").as_deref(), Some("ctrl+alt+1"));
        assert_eq!(normalize_chord("win+F11").as_deref(), Some("super+f11"));
        assert_eq!(normalize_chord("ctrl+"), None);
        assert_eq!(pretty("ctrl+alt+grave"), "Ctrl+Alt+`");
        assert_eq!(pretty("alt+k"), "Alt+K");
    }

    #[test]
    fn desktop_commands_that_run_hvelf() {
        assert_eq!(hvelf_action("hvelf --toggle"), Some("--toggle"));
        assert_eq!(hvelf_action("  /usr/bin/hvelf   --keep "), Some("--keep"));
        assert_eq!(hvelf_action("hvelf"), Some(""));
        assert_eq!(hvelf_action("hvelfx --toggle"), None);
        assert_eq!(hvelf_action("sh -c 'hvelf --toggle'"), None);
        assert_eq!(hvelf_action("hvelf --toggle --quit"), None);
        assert_eq!(hvelf_action("hvelf toggle"), None);
        assert_eq!(hvelf_action(""), None);
    }

    #[test]
    fn a_chord_gnome_binds_is_left_to_gnome() {
        let b = Bindings {
            custom: vec![
                Shortcut { path: "/p/hvelf-toggle/".into(), name: "hvelf".into(), command: "hvelf --toggle".into(), binding: "<Control><Alt>grave".into() },
                Shortcut { path: "/p/custom0/".into(), name: "Terminal".into(), command: "gnome-terminal".into(), binding: "<Control><Alt>t".into() },
                Shortcut { path: "/p/custom1/".into(), name: "unbound".into(), command: "hvelf --keep".into(), binding: "".into() },
            ],
            builtin: vec![("org.gnome.desktop.wm.keybindings".into(), "switch-group".into(), vec!["<Super>Above_Tab".into(), "<Alt>Above_Tab".into()])],
        };
        assert_eq!(b.owner_of("ctrl+alt+grave"), Some(Owner::Hvelf { command: "hvelf --toggle".into() }));
        assert_eq!(b.owner_of("ctrl+alt+t"), Some(Owner::Other { what: "the GNOME custom shortcut 'Terminal'".into() }));
        assert_eq!(b.owner_of("alt+grave"), Some(Owner::Other { what: "GNOME's own shortcut switch-group (org.gnome.desktop.wm.keybindings)".into() }));
        assert_eq!(b.owner_of("alt+k"), None, "an unbound custom shortcut holds no chord");
        assert_eq!(b.owner_of("ctrl+alt+1"), None);
        assert_eq!(b.hvelf_shortcuts(), vec![("Ctrl+Alt+`".to_string(), "hvelf --toggle".to_string())], "an unbound one is left out");
    }

    #[test]
    fn gnome_sessions_by_their_desktop_name() {
        assert!(is_gnome_from(Some("ubuntu:GNOME")));
        assert!(is_gnome_from(Some("GNOME")));
        assert!(is_gnome_from(Some("GNOME-Classic:GNOME")));
        assert!(!is_gnome_from(Some("KDE")));
        assert!(!is_gnome_from(Some("XFCE")));
        assert!(!is_gnome_from(None));
    }
}
