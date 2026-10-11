//! Linux desktop adapters.
//!
//! Everything hvelf asks of the desktop goes through here on Linux: which
//! session it is in, where Obsidian keeps its vault registry, how an
//! `obsidian://` link is handed to the system, and what can be seen of other
//! applications' windows. X11 publishes the window list (EWMH) and lets a
//! client grab a chord on the root window, so there the board works as it
//! does on Windows. hvelf has no Wayland compositor adapter for observing
//! or controlling other applications' windows; it reports those operations
//! as unsupported. GlobalShortcuts portal detection is separate from binding.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Capability, Failure};

// ---------------------------------------------------------------- session

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    X11,
    Wayland,
    Headless,
}

impl Session {
    pub fn platform(self) -> &'static str {
        match self {
            Session::X11 => "linux-x11",
            Session::Wayland => "linux-wayland",
            Session::Headless => "linux-headless",
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// The session as the environment describes it. `XDG_SESSION_TYPE` wins when
/// it names one, since a Wayland session also exports `DISPLAY` for XWayland.
pub fn detect_session_from(get: impl Fn(&str) -> Option<String>) -> Session {
    let get = |k: &str| get(k).filter(|v| !v.is_empty());
    match get("XDG_SESSION_TYPE").as_deref() {
        Some("wayland") => return Session::Wayland,
        Some("x11") => return Session::X11,
        _ => {}
    }
    if get("WAYLAND_DISPLAY").is_some() {
        Session::Wayland
    } else if get("DISPLAY").is_some() {
        Session::X11
    } else {
        Session::Headless
    }
}

pub fn session() -> Session {
    detect_session_from(env)
}

// ------------------------------------------------------------ directories

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME not set"))
}

/// `$XDG_CONFIG_HOME`, or `~/.config` when it is unset or not absolute, as
/// the XDG base directory specification requires.
pub fn config_home_from(home: &Path, xdg_config_home: Option<&str>) -> PathBuf {
    match xdg_config_home.filter(|x| Path::new(x).is_absolute()) {
        Some(x) => PathBuf::from(x),
        None => home.join(".config"),
    }
}

pub fn config_home() -> PathBuf {
    config_home_from(&home(), env("XDG_CONFIG_HOME").as_deref())
}

/// Where Obsidian may keep `obsidian.json`, in the order hvelf trusts them:
/// the XDG config home, the standard `~/.config` (the same place unless
/// `XDG_CONFIG_HOME` moves it), then the Flatpak and Snap sandboxes. Each
/// install keeps its own registry with its own vault ids, and an id is only
/// understood by the Obsidian that issued it, so the registries are never
/// merged: the first one present is the board.
pub fn registry_candidates(home: &Path, xdg_config_home: Option<&str>) -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = Vec::new();
    let xdg = config_home_from(home, xdg_config_home).join("obsidian/obsidian.json");
    let standard = home.join(".config/obsidian/obsidian.json");
    if xdg != standard {
        out.push((xdg, "xdg"));
    }
    out.push((standard, "standard"));
    out.push((
        home.join(".var/app/md.obsidian.Obsidian/config/obsidian/obsidian.json"),
        "flatpak",
    ));
    out.push((home.join("snap/obsidian/current/.config/obsidian/obsidian.json"), "snap"));
    out
}

/// The registry in use and the kind of install it belongs to. When none
/// exists the first candidate is returned, so an error names a real path.
pub fn pick_registry(cands: &[(PathBuf, &'static str)]) -> (PathBuf, &'static str, Vec<PathBuf>) {
    let present: Vec<&(PathBuf, &'static str)> = cands.iter().filter(|(p, _)| p.is_file()).collect();
    match present.split_first() {
        Some((first, rest)) => (
            first.0.clone(),
            first.1,
            rest.iter().map(|(p, _)| p.clone()).collect(),
        ),
        None => (cands[0].0.clone(), "missing", Vec::new()),
    }
}

pub fn registry() -> (PathBuf, &'static str, Vec<PathBuf>) {
    pick_registry(&registry_candidates(&home(), env("XDG_CONFIG_HOME").as_deref()))
}

// ------------------------------------------------------------ URI dispatch

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub fn find_on_path(prog: &str, path_var: Option<&str>) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .map(|d| d.join(prog))
        .find(|p| is_executable(p))
}

#[derive(Debug, PartialEq)]
pub enum Handler {
    Registered(String),
    Missing,
    Unknown(String),
}

/// Which desktop entry handles `obsidian://`, as `xdg-mime` reports it.
pub fn scheme_handler(path_var: Option<&str>) -> Handler {
    let Some(xdg_mime) = find_on_path("xdg-mime", path_var) else {
        return Handler::Unknown("xdg-mime is not on PATH".into());
    };
    let out = Command::new(xdg_mime)
        .args(["query", "default", "x-scheme-handler/obsidian"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                Handler::Missing
            } else {
                Handler::Registered(s)
            }
        }
        Ok(o) => Handler::Unknown(format!("xdg-mime exited with {}", o.status)),
        Err(e) => Handler::Unknown(format!("xdg-mime could not run: {e}")),
    }
}

/// The opener and its argument vector. The URI is one argument, never shell
/// text, whatever the vault is called.
pub fn opener_argv(uri: &str, path_var: Option<&str>) -> Option<(PathBuf, Vec<String>)> {
    if let Some(p) = find_on_path("xdg-open", path_var) {
        return Some((p, vec![uri.to_string()]));
    }
    find_on_path("gio", path_var).map(|p| (p, vec!["open".to_string(), uri.to_string()]))
}

/// xdg-open's documented exit codes, in the shared error vocabulary.
fn exit_code_meaning(code: Option<i32>) -> &'static str {
    match code {
        Some(3) => "not-installed",
        Some(4) => "no-handler",
        _ => "io-error",
    }
}

/// Hand `uri` to the desktop. The opener normally returns at once; if it
/// fails inside a short window the failure is returned, and if it is still
/// running (a handler launched in the foreground) it is reaped on a thread so
/// no zombie outlives the call.
pub fn open_uri(uri: &str, path_var: Option<&str>) -> Result<(), Failure> {
    const FEATURE: &str = "open-vault";
    if let Handler::Missing = scheme_handler(path_var) {
        return Err(Failure::new(
            "no-handler",
            "No application is registered for obsidian:// links (xdg-mime query default x-scheme-handler/obsidian is empty). Install Obsidian, or run it once so it registers its desktop entry.",
            FEATURE,
        ));
    }
    let Some((prog, args)) = opener_argv(uri, path_var) else {
        return Err(Failure::new(
            "not-installed",
            "Neither xdg-open nor gio is on PATH; install xdg-utils.",
            FEATURE,
        ));
    };
    let mut child = Command::new(&prog)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Failure::new("io-error", format!("cannot start {}: {e}", prog.display()), FEATURE))?;
    let deadline = Instant::now() + Duration::from_millis(1000);
    loop {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return Ok(()),
            Ok(Some(st)) => {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::io::Read;
                    let _ = (&mut e).take(2048).read_to_string(&mut err);
                }
                return Err(Failure::new(
                    exit_code_meaning(st.code()),
                    format!("{} exited with {st}: {}", prog.display(), err.trim()),
                    FEATURE,
                ));
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(40)),
            Ok(None) => {
                std::thread::spawn(move || {
                    if let Some(mut e) = child.stderr.take() {
                        let _ = std::io::copy(&mut e, &mut std::io::sink());
                    }
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(e) => return Err(Failure::new("io-error", format!("cannot wait for opener: {e}"), FEATURE)),
        }
    }
}

// -------------------------------------------------------------- D-Bus

/// zbus resolves the session bus from `DBUS_SESSION_BUS_ADDRESS`, else
/// `$XDG_RUNTIME_DIR/bus`. The single-instance plugin unwraps that lookup, so
/// it is only installed when one of them is there.
pub fn session_bus_available() -> bool {
    env("DBUS_SESSION_BUS_ADDRESS").is_some()
        || env("XDG_RUNTIME_DIR")
            .map(|d| Path::new(&d).join("bus").exists())
            .unwrap_or(false)
}

/// Run a D-Bus probe on a thread and give up after `secs`: activation of a
/// missing service, or a dead bus socket, must not hang the board.
fn bounded<T: Send + 'static>(secs: u64, f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs))
        .unwrap_or_else(|_| Err(format!("no answer within {secs} s")))
}

/// Whether the session bus is there and answers, not merely named.
pub fn session_bus_reachable() -> Result<(), String> {
    if !session_bus_available() {
        return Err("no D-Bus session bus (DBUS_SESSION_BUS_ADDRESS unset, no $XDG_RUNTIME_DIR/bus)".into());
    }
    bounded(2, || zbus::blocking::Connection::session().map(|_| ()).map_err(|e| format!("session bus unreachable: {e}")))
}

/// The GlobalShortcuts portal's interface version, if the session has one.
/// Bounded: portal activation can hang on a session without a portal.
pub fn portal_global_shortcuts() -> Result<u32, String> {
    session_bus_reachable()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = (|| -> Result<u32, String> {
            let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
            let proxy = zbus::blocking::Proxy::new(
                &conn,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.GlobalShortcuts",
            )
            .map_err(|e| e.to_string())?;
            proxy.get_property::<u32>("version").map_err(|e| e.to_string())
        })();
        let _ = tx.send(r);
    });
    rx.recv_timeout(Duration::from_secs(3))
        .unwrap_or_else(|_| Err("portal did not answer within 3 s".into()))
}

// ------------------------------------------------------------------ X11

pub mod x11 {
    use std::time::{Duration, Instant};

    use x11rb::connection::Connection;
    use x11rb::errors::ReplyError;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, Keycode, ModMask,
        Property, Window, WindowClass,
    };
    use x11rb::protocol::{ErrorKind, Event};
    use x11rb::rust_connection::RustConnection;

    fn e(x: impl std::fmt::Display) -> String {
        x.to_string()
    }

    pub struct Ewmh {
        pub conn: RustConnection,
        pub root: Window,
        client_list: u32,
        active: u32,
        close: u32,
        supported: u32,
        net_wm_name: u32,
        utf8: u32,
        wm_pid: u32,
    }

    impl Ewmh {
        pub fn connect(display: Option<&str>) -> Result<Ewmh, String> {
            let (conn, screen) = x11rb::connect(display).map_err(|x| format!("cannot connect to the X display: {x}"))?;
            let root = conn.setup().roots[screen].root;
            let atom = |name: &[u8]| -> Result<u32, String> {
                Ok(conn.intern_atom(false, name).map_err(e)?.reply().map_err(e)?.atom)
            };
            let client_list = atom(b"_NET_CLIENT_LIST")?;
            let active = atom(b"_NET_ACTIVE_WINDOW")?;
            let close = atom(b"_NET_CLOSE_WINDOW")?;
            let supported = atom(b"_NET_SUPPORTED")?;
            let net_wm_name = atom(b"_NET_WM_NAME")?;
            let utf8 = atom(b"UTF8_STRING")?;
            let wm_pid = atom(b"_NET_WM_PID")?;
            Ok(Ewmh { conn, root, client_list, active, close, supported, net_wm_name, utf8, wm_pid })
        }

        fn windows_prop(&self, w: Window, prop: u32) -> Result<Option<Vec<u32>>, String> {
            let r = self
                .conn
                .get_property(false, w, prop, AtomEnum::ANY, 0, 16384)
                .map_err(e)?
                .reply()
                .map_err(e)?;
            if r.type_ == x11rb::NONE {
                return Ok(None);
            }
            Ok(r.value32().map(|it| it.collect()))
        }

        /// The atoms the window manager declares in `_NET_SUPPORTED`.
        pub fn supports(&self, what: &str) -> bool {
            let want = match what {
                "client-list" => self.client_list,
                "active" => self.active,
                "close" => self.close,
                _ => return false,
            };
            matches!(self.windows_prop(self.root, self.supported), Ok(Some(v)) if v.contains(&want))
        }

        /// Managed top-level windows, or an error when no window manager
        /// publishes `_NET_CLIENT_LIST` (then nothing can be observed).
        pub fn client_list(&self) -> Result<Vec<Window>, String> {
            self.windows_prop(self.root, self.client_list)?
                .ok_or_else(|| "the window manager does not publish _NET_CLIENT_LIST".to_string())
        }

        pub fn title(&self, w: Window) -> Option<String> {
            let r = self.conn.get_property(false, w, self.net_wm_name, self.utf8, 0, 4096).ok()?.reply().ok()?;
            if r.type_ != x11rb::NONE && !r.value.is_empty() {
                return Some(String::from_utf8_lossy(&r.value).into_owned());
            }
            let r = self
                .conn
                .get_property(false, w, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 4096)
                .ok()?
                .reply()
                .ok()?;
            // WM_NAME in STRING encoding is Latin-1.
            (!r.value.is_empty()).then(|| r.value.iter().map(|&b| b as char).collect())
        }

        pub fn titled_windows(&self) -> Result<Vec<(Window, String)>, String> {
            Ok(self
                .client_list()?
                .into_iter()
                .filter_map(|w| self.title(w).map(|t| (w, t)))
                .collect())
        }

        pub fn active_window(&self) -> Option<Window> {
            match self.windows_prop(self.root, self.active) {
                Ok(Some(v)) => v.first().copied().filter(|&w| w != 0),
                _ => None,
            }
        }

        fn ask_wm(&self, w: Window, kind: u32, data: [u32; 5]) -> Result<(), String> {
            let ev = ClientMessageEvent::new(32, w, kind, data);
            self.conn
                .send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    ev,
                )
                .map_err(e)?;
            self.conn.flush().map_err(e)
        }

        /// Ask the window manager to raise and focus `w` (source 2: a pager
        /// acting for the user, which window managers honour).
        pub fn request_activate(&self, w: Window) -> Result<(), String> {
            self.ask_wm(w, self.active, [2, 0, 0, 0, 0])
        }

        /// Ask the window manager to close `w` politely, as its own close
        /// button does: Obsidian receives WM_DELETE_WINDOW and saves. Nothing
        /// is killed.
        pub fn request_close(&self, w: Window) -> Result<(), String> {
            self.ask_wm(w, self.close, [0, 2, 0, 0, 0])
        }

        /// The process behind `w`: asked of the X server, which knows without
        /// taking the client's word, else the client's own `_NET_WM_PID`.
        /// None for a remote client that says nothing.
        pub fn pid_of(&self, w: Window) -> Option<u32> {
            client_pid(&self.conn, w).or_else(|| {
                let r = self.conn.get_property(false, w, self.wm_pid, AtomEnum::CARDINAL, 0, 1).ok()?.reply().ok()?;
                let pid = r.value32()?.next();
                pid
            })
        }

        /// The window in front, which a capture is credited to and which gets
        /// the focus back once it is filed.
        pub fn front(&self) -> Result<Front, String> {
            let window = self
                .active_window()
                .ok_or_else(|| "no window is in front (the window manager publishes no _NET_ACTIVE_WINDOW)".to_string())?;
            Ok(Front { window, pid: self.pid_of(window), title: self.title(window).unwrap_or_default() })
        }
    }

    // ----------------------------------------------------------- capture

    /// The window in front when the keep chord was pressed.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Front {
        pub window: Window,
        pub pid: Option<u32>,
        pub title: String,
    }

    /// The local process that created `w`, from the X-Resource extension.
    pub fn client_pid(conn: &impl Connection, w: Window) -> Option<u32> {
        use x11rb::protocol::res::{ClientIdMask, ClientIdSpec, ConnectionExt as _};
        let spec = ClientIdSpec { client: w, mask: ClientIdMask::LOCAL_CLIENT_PID };
        let reply = conn.res_query_client_ids(&[spec]).ok()?.reply().ok()?;
        reply
            .ids
            .into_iter()
            .find(|id| id.spec.mask == ClientIdMask::LOCAL_CLIENT_PID)
            .and_then(|id| id.value.first().copied())
    }

    /// The PRIMARY selection: the text last highlighted on this display, and
    /// the process holding it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Selection {
        pub text: String,
        pub owner_pid: Option<u32>,
    }

    /// Read PRIMARY from its owner the way any X client pastes it, so no key
    /// is injected and the clipboard is left alone. `Ok(None)` when nothing
    /// holds a selection. Bounded by `timeout`: the owner is another program,
    /// and it may be busy or gone.
    pub fn read_primary(display: Option<&str>, timeout: Duration) -> Result<Option<Selection>, String> {
        let (conn, screen) = x11rb::connect(display).map_err(|x| format!("cannot connect to the X display: {x}"))?;
        let root = conn.setup().roots[screen].root;
        let owner = conn.get_selection_owner(AtomEnum::PRIMARY.into()).map_err(e)?.reply().map_err(e)?.owner;
        if owner == x11rb::NONE {
            return Ok(None);
        }
        let atom = |name: &[u8]| -> Result<u32, String> {
            Ok(conn.intern_atom(false, name).map_err(e)?.reply().map_err(e)?.atom)
        };
        let (utf8, incr, prop) = (atom(b"UTF8_STRING")?, atom(b"INCR")?, atom(b"HVELF_SELECTION")?);
        let win = conn.generate_id().map_err(e)?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(e)?;
        let deadline = Instant::now() + timeout;
        let text = (|| -> Result<Option<String>, String> {
            for target in [utf8, u32::from(AtomEnum::STRING)] {
                conn.convert_selection(win, AtomEnum::PRIMARY.into(), target, prop, x11rb::CURRENT_TIME)
                    .map_err(e)?;
                conn.flush().map_err(e)?;
                let notify = loop {
                    if let Event::SelectionNotify(n) = next_event(&conn, deadline)? {
                        if n.requestor == win {
                            break n;
                        }
                    }
                };
                if notify.property == x11rb::NONE {
                    continue; // not offered as this target; try the next
                }
                let bytes = take_property(&conn, win, prop, incr, deadline)?;
                let text = if target == utf8 {
                    String::from_utf8_lossy(&bytes).into_owned()
                } else {
                    bytes.iter().map(|&b| b as char).collect() // STRING is Latin-1
                };
                return Ok(Some(text.trim_end_matches('\0').to_string()));
            }
            Ok(None)
        })();
        let _ = conn.destroy_window(win);
        let _ = conn.flush();
        match text? {
            Some(text) => Ok(Some(Selection { text, owner_pid: client_pid(&conn, owner) })),
            None => Err("the selection is not text".into()),
        }
    }

    fn next_event(conn: &RustConnection, deadline: Instant) -> Result<Event, String> {
        loop {
            if let Some(ev) = conn.poll_for_event().map_err(e)? {
                return Ok(ev);
            }
            if Instant::now() >= deadline {
                return Err("the program holding the selection did not hand it over in time".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The converted selection, read off our window and deleted. A large one
    /// comes INCR: a size first, then chunks, each announced by a property
    /// change and taken by deleting it, until an empty chunk ends it.
    fn take_property(conn: &RustConnection, win: Window, prop: u32, incr: u32, deadline: Instant) -> Result<Vec<u8>, String> {
        let get = || -> Result<x11rb::protocol::xproto::GetPropertyReply, String> {
            let r = conn.get_property(true, win, prop, AtomEnum::ANY, 0, u32::MAX / 4).map_err(e)?.reply().map_err(e)?;
            conn.flush().map_err(e)?;
            Ok(r)
        };
        let first = get()?;
        if first.type_ != incr {
            return Ok(first.value);
        }
        let mut out = Vec::new();
        loop {
            if let Event::PropertyNotify(p) = next_event(conn, deadline)? {
                if p.window == win && p.atom == prop && p.state == Property::NEW_VALUE {
                    let chunk = get()?;
                    if chunk.value.is_empty() {
                        return Ok(out);
                    }
                    out.extend_from_slice(&chunk.value);
                }
            }
        }
    }

    // ------------------------------------------------------------ chords

    const LOCK_MASK: u16 = 1 << 1;
    const MOD2: u16 = 1 << 4;
    const NUM_LOCK: u32 = 0xff7f;
    const SCROLL_LOCK: u32 = 0xff14;

    /// The modifier bits a chord ignores, as GNOME's own shortcuts do: Caps Lock's Lock bit, and whichever of
    /// Mod2, Mod3 and Mod5 the keymap gives Num Lock or Scroll Lock (Num Lock is Mod2 almost everywhere). A grab
    /// covers every combination of them, or the chord dies while one of those locks is on. Shift, Control, Mod1
    /// and Mod4 are never ignored: chords are made of them.
    pub fn lock_bits(modmap: &[Vec<Keycode>], keysyms_of: impl Fn(Keycode) -> Vec<u32>) -> u16 {
        let mut bits = LOCK_MASK;
        for i in [4usize, 5, 7] {
            let locks = modmap.get(i).is_some_and(|codes| {
                codes.iter().any(|&k| k != 0 && keysyms_of(k).iter().any(|&s| s == NUM_LOCK || s == SCROLL_LOCK))
            });
            if locks {
                bits |= 1 << i;
            }
        }
        bits
    }

    /// Every combination of the bits in `mask`, 0 first.
    pub fn submasks(mask: u16) -> Vec<u16> {
        let mut out = vec![0];
        let mut s = mask;
        while s != 0 {
            out.push(s);
            s = (s - 1) & mask;
        }
        out.sort_unstable();
        out
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Chord {
        pub mods: u16,
        pub keycode: Keycode,
    }

    /// X keycode of the physical key under Esc: evdev KEY_GRAVE (41) plus
    /// the X offset of 8. Like Windows scancode 0x29 it names the key, not
    /// the character a layout prints on it.
    pub const GRAVE_KEYCODE: Keycode = 49;

    /// Parse `alt+grave`-style chords. Letters, digits and f-keys are looked
    /// up in the live keyboard map through `keysym_to_code`.
    pub fn parse_chord(s: &str, keysym_to_code: impl Fn(u32) -> Option<Keycode>) -> Option<Chord> {
        let mut mods = 0u16;
        let mut key: Option<Keycode> = None;
        for tok in s.split('+') {
            let t = tok.trim().to_lowercase();
            match t.as_str() {
                "alt" => mods |= u16::from(ModMask::M1),
                "ctrl" | "control" => mods |= u16::from(ModMask::CONTROL),
                "shift" => mods |= u16::from(ModMask::SHIFT),
                "win" | "super" | "meta" => mods |= u16::from(ModMask::M4),
                "grave" | "backquote" | "`" => key = Some(GRAVE_KEYCODE),
                k => {
                    let k = k.strip_prefix("digit").unwrap_or(k);
                    let k = k.strip_prefix("key").unwrap_or(k);
                    let sym = if k.len() == 1 && k.chars().next().unwrap().is_ascii_alphanumeric() {
                        Some(k.chars().next().unwrap() as u32)
                    } else {
                        k.strip_prefix('f')
                            .and_then(|n| n.parse::<u32>().ok())
                            .filter(|n| (1..=24).contains(n))
                            .map(|n| 0xffbe + n - 1)
                    };
                    key = sym.and_then(&keysym_to_code);
                    if key.is_none() {
                        return None;
                    }
                }
            }
        }
        key.map(|keycode| Chord { mods, keycode })
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum GrabFailure {
        /// Another client holds the chord.
        Conflict,
        Other(String),
    }

    pub struct Grabber {
        pub conn: RustConnection,
        pub root: Window,
        /// The lock modifiers this server's keymap defines (see `lock_bits`).
        pub ignored: u16,
    }

    impl Grabber {
        pub fn connect(display: Option<&str>) -> Result<Grabber, String> {
            let (conn, screen) = x11rb::connect(display).map_err(|x| format!("cannot connect to the X display: {x}"))?;
            let root = conn.setup().roots[screen].root;
            let mut g = Grabber { conn, root, ignored: LOCK_MASK | MOD2 };
            if let Some(bits) = g.lock_modifiers() {
                g.ignored = bits;
            }
            Ok(g)
        }

        /// `lock_bits` for this server's modifier map and keymap.
        fn lock_modifiers(&self) -> Option<u16> {
            let mm = self.conn.get_modifier_mapping().ok()?.reply().ok()?;
            let per = (mm.keycodes.len() / 8).max(1);
            let modmap: Vec<Vec<Keycode>> = mm.keycodes.chunks(per).map(|c| c.to_vec()).collect();
            let setup = self.conn.setup();
            let (min, max) = (setup.min_keycode, setup.max_keycode);
            let km = self.conn.get_keyboard_mapping(min, max - min + 1).ok()?.reply().ok()?;
            let per_key = (km.keysyms_per_keycode as usize).max(1);
            Some(lock_bits(&modmap, |k| {
                let i = usize::from(k.saturating_sub(min));
                km.keysyms.get(i * per_key..(i + 1) * per_key).map(<[u32]>::to_vec).unwrap_or_default()
            }))
        }

        /// The ignored lock modifiers, by name, for a capability reason.
        pub fn ignored_names(&self) -> String {
            let names = [(1u16 << 1, "Lock"), (1 << 4, "Mod2"), (1 << 5, "Mod3"), (1 << 7, "Mod5")];
            names.iter().filter(|(b, _)| self.ignored & b != 0).map(|(_, n)| *n).collect::<Vec<_>>().join(", ")
        }

        /// Keycode for a keysym in the current keyboard map.
        pub fn keycode_for(&self, keysym: u32) -> Option<Keycode> {
            let setup = self.conn.setup();
            let (min, max) = (setup.min_keycode, setup.max_keycode);
            let map = self.conn.get_keyboard_mapping(min, max - min + 1).ok()?.reply().ok()?;
            let per = map.keysyms_per_keycode as usize;
            map.keysyms
                .chunks(per.max(1))
                .position(|syms| syms.contains(&keysym))
                .map(|i| min + i as u8)
        }

        pub fn grab(&self, c: Chord) -> Result<(), GrabFailure> {
            for extra in submasks(self.ignored) {
                let r = self
                    .conn
                    .grab_key(true, self.root, ModMask::from(c.mods | extra), c.keycode, GrabMode::ASYNC, GrabMode::ASYNC)
                    .map_err(|x| GrabFailure::Other(x.to_string()))?
                    .check();
                match r {
                    Ok(()) => {}
                    Err(ReplyError::X11Error(x)) if x.error_kind == ErrorKind::Access => {
                        self.ungrab(c);
                        return Err(GrabFailure::Conflict);
                    }
                    Err(x) => return Err(GrabFailure::Other(x.to_string())),
                }
            }
            Ok(())
        }

        pub fn ungrab(&self, c: Chord) {
            for extra in submasks(self.ignored) {
                let _ = self.conn.ungrab_key(c.keycode, self.root, ModMask::from(c.mods | extra));
            }
            let _ = self.conn.flush();
        }

        /// Like `next_press`, but gives up at `deadline`: for tests, which must fail rather than hang.
        #[cfg(test)]
        pub fn next_press_until(&self, chords: &[Chord], deadline: Instant) -> Option<usize> {
            while Instant::now() < deadline {
                match self.conn.poll_for_event() {
                    Ok(Some(Event::KeyPress(ev))) => {
                        let state = u16::from(ev.state) & !self.ignored & 0xff;
                        if let Some(i) = chords.iter().position(|c| c.keycode == ev.detail && c.mods == state) {
                            return Some(i);
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                    Err(_) => return None,
                }
            }
            None
        }

        /// Block until one of `chords` is pressed and return its index.
        /// `None` when the connection to the display is lost.
        pub fn next_press(&self, chords: &[Chord]) -> Option<usize> {
            loop {
                match self.conn.wait_for_event() {
                    Ok(Event::KeyPress(ev)) => {
                        let state = u16::from(ev.state) & !self.ignored & 0xff;
                        if let Some(i) = chords.iter().position(|c| c.keycode == ev.detail && c.mods == state) {
                            return Some(i);
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return None,
                }
            }
        }
    }
}

// ------------------------------------------------------------------ keep

/// What the keep chord found on X11.
pub struct Grabbed {
    pub text: String,
    /// The program in front, by executable name, e.g. `firefox`.
    pub app: String,
    pub title: String,
    pub window: u32,
}

pub enum Grab {
    Text(Grabbed),
    /// The board itself is in front: the chord just puts it away.
    Own,
}

/// The highlighted text for a keep, credited to the window in front. X11's
/// PRIMARY holds the last text highlighted anywhere on the display, which
/// need not be in the window in front; then nothing is taken, since a keep
/// records which program it came from and that would be another one.
pub fn grab_selection(own_pid: u32, timeout: Duration) -> Result<Grab, String> {
    match session() {
        Session::X11 => {}
        Session::Wayland => {
            return Err("capture needs an X11 session; Wayland gives no application another one's selection".into())
        }
        Session::Headless => return Err("no graphical session".into()),
    }
    let front = x11::Ewmh::connect(None)?.front()?;
    if front.pid == Some(own_pid) {
        return Ok(Grab::Own);
    }
    let sel = x11::read_primary(None, timeout)?.ok_or("nothing selected")?;
    let app = front.pid.map(exe_name).unwrap_or_else(|| "unknown".into());
    if let (Some(a), Some(b)) = (front.pid, sel.owner_pid) {
        if a != b {
            return Err(format!("the highlighted text is in {}, not in {app}; select it again", exe_name(b)));
        }
    }
    if sel.text.is_empty() {
        return Err("nothing selected".into());
    }
    Ok(Grab::Text(Grabbed { text: sel.text, app, title: front.title, window: front.window }))
}

/// A process's executable name from /proc, or its command name when the
/// executable cannot be read.
pub fn exe_name(pid: u32) -> String {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().trim_end_matches(" (deleted)").to_string()))
        .or_else(|| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("pid {pid}"))
}

// ---------------------------------------------------------- capabilities

pub fn cap(capability: &'static str, state: &'static str, reason: impl Into<String>, source: impl Into<String>) -> Capability {
    Capability {
        capability,
        state,
        reason: reason.into(),
        source: source.into(),
        platform: session().platform(),
    }
}

/// What X11 window control this session offers, probed once.
pub struct WindowControl {
    pub observe: bool,
    pub activate: bool,
    pub close: bool,
    pub reason: String,
}

pub fn window_control() -> WindowControl {
    let none = |reason: &str| WindowControl { observe: false, activate: false, close: false, reason: reason.into() };
    match session() {
        Session::Wayland => none("hvelf has no Wayland adapter for observing or controlling other applications' windows"),
        Session::Headless => none("no graphical session (neither WAYLAND_DISPLAY nor DISPLAY is set)"),
        Session::X11 => match x11::Ewmh::connect(None) {
            Err(e) => none(&e),
            Ok(w) => match w.client_list() {
                Err(e) => none(&e),
                Ok(_) => WindowControl {
                    observe: true,
                    activate: w.supports("active"),
                    close: w.supports("close"),
                    reason: String::new(),
                },
            },
        },
    }
}

/// Static probes for `hvelf --capabilities` and the board; the chord and
/// tray records are added by whoever tried them.
pub fn capabilities() -> Vec<Capability> {
    let mut out = Vec::new();
    let s = session();
    out.push(cap("session", "available", format!("{s:?} session"), "XDG_SESSION_TYPE, WAYLAND_DISPLAY, DISPLAY"));

    let (reg, kind, others) = registry();
    let also = if others.is_empty() {
        String::new()
    } else {
        format!(" Other registries present and not merged: {}.", others.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))
    };
    out.push(match std::fs::read_to_string(&reg) {
        Err(_) => cap("vault-registry", "unavailable", format!("no obsidian.json at {} (Obsidian not installed, or never run)", reg.display()), reg.display().to_string()),
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(_) => cap("vault-registry", "available", format!("{kind} install registry.{also}"), reg.display().to_string()),
            Err(e) => cap("vault-registry", "degraded", format!("obsidian.json is malformed: {e}"), reg.display().to_string()),
        },
    });

    let path_var = env("PATH");
    out.push(match (opener_argv("obsidian://", path_var.as_deref()), scheme_handler(path_var.as_deref())) {
        (None, _) => cap("open-vault", "unavailable", "neither xdg-open nor gio is on PATH; install xdg-utils", "PATH"),
        (Some(_), Handler::Missing) => cap("open-vault", "unavailable", "no handler for x-scheme-handler/obsidian; install or run Obsidian once", "xdg-mime"),
        (Some((p, _)), Handler::Registered(h)) => cap("open-vault", "available", format!("obsidian:// handled by {h}"), p.display().to_string()),
        (Some((p, _)), Handler::Unknown(why)) => cap("open-vault", "degraded", format!("handler not verified: {why}"), p.display().to_string()),
    });

    let wc = window_control();
    out.push(if wc.observe {
        cap("open-state", "available", "observed from the window manager's _NET_CLIENT_LIST", "EWMH")
    } else {
        cap("open-state", "degraded", format!("reported from Obsidian's own open flags, which can be stale: {}", wc.reason), "obsidian.json")
    });
    out.push(match (wc.activate, s) {
        (true, _) => cap("focus-window", "available", "raised through _NET_ACTIVE_WINDOW", "EWMH"),
        (false, Session::Wayland) => cap("focus-window", "degraded", "delegated to Obsidian through obsidian://; the compositor may only flag the window instead of raising it", "obsidian://"),
        (false, _) => cap("focus-window", "unavailable", wc.reason.clone(), "EWMH"),
    });
    out.push(if wc.close {
        cap("close-window", "available", "polite close through _NET_CLOSE_WINDOW; Obsidian saves as on its own close button", "EWMH")
    } else if s == Session::Wayland {
        cap("close-window", "unsupported", wc.reason.clone(), "EWMH")
    } else {
        cap("close-window", "unavailable", wc.reason.clone(), "EWMH")
    });
    out.push(if wc.observe {
        cap("focus-recency", "available", "active window sampled every 2 s", "_NET_ACTIVE_WINDOW")
    } else {
        cap("focus-recency", if s == Session::Wayland { "unsupported" } else { "unavailable" }, format!("ranked by Obsidian's last-opened stamps only: {}", wc.reason), "obsidian.json ts")
    });

    out.push(match portal_global_shortcuts() {
        Ok(v) => cap("global-hotkey-portal", "disabled", format!("xdg-desktop-portal GlobalShortcuts version {v} is present; binding through it is deferred in this build, bind `hvelf --toggle` as a desktop shortcut instead"), "org.freedesktop.portal.GlobalShortcuts"),
        Err(why) => cap("global-hotkey-portal", "unavailable", format!("GlobalShortcuts portal not reachable: {why}"), "org.freedesktop.portal.GlobalShortcuts"),
    });
    if let Ok(d) = crate::gnome::read() {
        let mine = d.hvelf_shortcuts();
        if !mine.is_empty() {
            let list = mine.iter().map(|(c, cmd)| format!("{c} runs `{cmd}`")).collect::<Vec<_>>().join(", ");
            out.push(cap(
                "global-hotkey-desktop",
                "available",
                format!("{list}, as GNOME custom shortcuts; GNOME handles those chords, so hvelf does not grab them itself"),
                "org.gnome.settings-daemon.plugins.media-keys",
            ));
        }
    }
    out.push(match session_bus_reachable() {
        Ok(()) => cap("toggle-command", "available", "`hvelf --toggle` (also --show, --hide, --quick-launch, --quit) reaches the running board over D-Bus; bind it as a desktop shortcut", "tauri-plugin-single-instance"),
        Err(why) => cap("toggle-command", "unavailable", format!("a second hvelf cannot reach the running one: {why}"), "DBUS_SESSION_BUS_ADDRESS"),
    });
    out
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn session_prefers_declared_type_over_xwayland_display() {
        assert_eq!(detect_session_from(fake_env(&[("XDG_SESSION_TYPE", "wayland"), ("DISPLAY", ":0")])), Session::Wayland);
        assert_eq!(detect_session_from(fake_env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")])), Session::Wayland);
        assert_eq!(detect_session_from(fake_env(&[("XDG_SESSION_TYPE", "tty"), ("DISPLAY", ":1")])), Session::X11);
        assert_eq!(detect_session_from(fake_env(&[("DISPLAY", "")])), Session::Headless);
        assert_eq!(detect_session_from(fake_env(&[])), Session::Headless);
    }

    #[test]
    fn xdg_config_home_must_be_absolute() {
        let home = Path::new("/h/o m é");
        assert_eq!(config_home_from(home, Some("/x/cfg")), PathBuf::from("/x/cfg"));
        assert_eq!(config_home_from(home, Some("relative/cfg")), home.join(".config"));
        assert_eq!(config_home_from(home, None), home.join(".config"));
    }

    #[test]
    fn registry_candidates_keep_the_standard_location() {
        let home = Path::new("/home/p q");
        let c = registry_candidates(home, Some("/elsewhere/cfg"));
        let kinds: Vec<&str> = c.iter().map(|(_, k)| *k).collect();
        assert_eq!(kinds, ["xdg", "standard", "flatpak", "snap"]);
        assert_eq!(c[1].0, home.join(".config/obsidian/obsidian.json"));
        // Unset or equal XDG does not list the same file twice.
        let c = registry_candidates(home, None);
        assert_eq!(c.iter().filter(|(_, k)| *k == "standard").count(), 1);
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn first_present_registry_wins_and_others_are_reported() {
        let dir = std::env::temp_dir().join(format!("hvelf-reg-{}-ü \"q\"", std::process::id()));
        let flat = dir.join(".var/app/md.obsidian.Obsidian/config/obsidian/obsidian.json");
        let snap = dir.join("snap/obsidian/current/.config/obsidian/obsidian.json");
        for p in [&flat, &snap] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "{\"vaults\":{}}").unwrap();
        }
        let (p, kind, others) = pick_registry(&registry_candidates(&dir, None));
        assert_eq!((p, kind), (flat.clone(), "flatpak"));
        assert_eq!(others, vec![snap.clone()]);
        std::fs::remove_file(&flat).unwrap();
        std::fs::remove_file(&snap).unwrap();
        let (p, kind, _) = pick_registry(&registry_candidates(&dir, None));
        assert_eq!((p, kind), (dir.join(".config/obsidian/obsidian.json"), "missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fake_bin(dir: &Path, name: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn bin_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hvelf-bin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn uri_is_one_argument_even_with_quotes_and_spaces() {
        let d = bin_dir("argv");
        let log = d.join("argv.log");
        fake_bin(&d, "xdg-mime", "echo obsidian.desktop");
        fake_bin(&d, "xdg-open", &format!("printf '%s\\n' \"$#\" \"$1\" > '{}'", log.display()));
        let uri = "obsidian://open?vault=a%22b%20c%3B%24%28x%29";
        open_uri(uri, Some(d.to_str().unwrap())).unwrap();
        let got = std::fs::read_to_string(&log).unwrap();
        assert_eq!(got, format!("1\n{uri}\n"));
    }

    #[test]
    fn missing_handler_and_opener_are_reported_not_ignored() {
        let d = bin_dir("nohandler");
        fake_bin(&d, "xdg-mime", "exit 0");
        fake_bin(&d, "xdg-open", "exit 0");
        let f = open_uri("obsidian://open?vault=x", Some(d.to_str().unwrap())).unwrap_err();
        assert_eq!((f.code, f.feature), ("no-handler", "open-vault"));

        let empty = bin_dir("empty");
        let f = open_uri("obsidian://open?vault=x", Some(empty.to_str().unwrap())).unwrap_err();
        assert_eq!(f.code, "not-installed");

        let d = bin_dir("fails");
        fake_bin(&d, "xdg-mime", "echo obsidian.desktop");
        fake_bin(&d, "xdg-open", "echo 'no method available' >&2; exit 4");
        let f = open_uri("obsidian://open?vault=x", Some(d.to_str().unwrap())).unwrap_err();
        assert_eq!(f.code, "no-handler");
        assert!(f.message.contains("no method available"), "{}", f.message);
    }

    #[test]
    fn gio_is_the_fallback_opener() {
        let d = bin_dir("gio");
        fake_bin(&d, "gio", "exit 0");
        let (p, args) = opener_argv("obsidian://x", Some(d.to_str().unwrap())).unwrap();
        assert_eq!(p, d.join("gio"));
        assert_eq!(args, ["open", "obsidian://x"]);
    }

    #[test]
    fn slow_opener_is_reaped_not_awaited() {
        let d = bin_dir("slow");
        fake_bin(&d, "xdg-mime", "echo obsidian.desktop");
        fake_bin(&d, "xdg-open", "sleep 3");
        let t = Instant::now();
        open_uri("obsidian://x", Some(d.to_str().unwrap())).unwrap();
        assert!(t.elapsed() < Duration::from_millis(2500));
    }

    #[test]
    fn chords_parse_to_physical_grave_and_mapped_keys() {
        use x11::{parse_chord, Chord, GRAVE_KEYCODE};
        let map = |sym: u32| match sym {
            0x31 => Some(10u8),   // '1'
            0x71 => Some(24u8),   // 'q'
            0xffbe => Some(67u8), // F1
            _ => None,
        };
        assert_eq!(parse_chord("alt+grave", map), Some(Chord { mods: 1 << 3, keycode: GRAVE_KEYCODE }));
        assert_eq!(parse_chord("ctrl+shift+Q", map), Some(Chord { mods: (1 << 2) | 1, keycode: 24 }));
        assert_eq!(parse_chord("super+digit1", map), Some(Chord { mods: 1 << 6, keycode: 10 }));
        assert_eq!(parse_chord("alt+f1", map), Some(Chord { mods: 1 << 3, keycode: 67 }));
        assert_eq!(parse_chord("alt+nosuchkey", map), None);
        assert_eq!(parse_chord("alt", map), None);
    }

    #[test]
    fn lock_modifiers_come_from_the_keymap() {
        let keysyms = |k: u8| match k {
            77 => vec![0xff7f],  // Num_Lock
            78 => vec![0xff14],  // Scroll_Lock
            66 => vec![0xffe5],  // Caps_Lock
            64 => vec![0xffe9],  // Alt_L
            133 => vec![0xffeb], // Super_L
            _ => vec![],
        };
        let map = |slots: [&[u8]; 8]| slots.iter().map(|s| s.to_vec()).collect::<Vec<_>>();
        // The usual keymap: Num Lock on Mod2, Scroll Lock on nothing.
        assert_eq!(x11::lock_bits(&map([&[50], &[66], &[37], &[64], &[77], &[], &[133], &[92]]), keysyms), 0b1_0010);
        // Scroll Lock on Mod3 as well.
        assert_eq!(x11::lock_bits(&map([&[50], &[66], &[37], &[64], &[77], &[78], &[133], &[92]]), keysyms), 0b11_0010);
        // A lock key on Mod1 or Mod4 is never ignored: chords are made of Alt and Super.
        assert_eq!(x11::lock_bits(&map([&[], &[66], &[], &[77], &[], &[], &[78], &[]]), keysyms), 0b10);
        assert_eq!(x11::submasks(0b1_0010), vec![0, 0b10, 0b1_0000, 0b1_0010]);
        assert_eq!(x11::submasks(0b11_0010).len(), 8);
        assert_eq!(x11::submasks(0), vec![0]);
    }

    /// Needs an X server without a window manager: run under
    /// `xvfb-run -a` with HVELF_X11_TEST=1. The test plays the window
    /// manager itself, so it can check what hvelf publishes and asks for.
    #[test]
    fn x11_ewmh_and_grab_against_a_real_server() {
        if std::env::var("HVELF_X11_TEST").is_err() {
            eprintln!("skipped: set HVELF_X11_TEST=1 under xvfb-run");
            return;
        }
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, EventMask, PropMode, WindowClass, ChangeWindowAttributesAux};
        use x11rb::protocol::Event;
        use x11rb::wrapper::ConnectionExt as _;

        let (wm, screen) = x11rb::connect(None).unwrap();
        let root = wm.setup().roots[screen].root;
        let atom = |n: &[u8]| wm.intern_atom(false, n).unwrap().reply().unwrap().atom;
        // The test plays the window manager and injects keys: never on a
        // display where a real window manager runs.
        let check = atom(b"_NET_SUPPORTING_WM_CHECK");
        let real_wm = wm.get_property(false, root, check, x11rb::protocol::xproto::AtomEnum::WINDOW, 0, 1).unwrap().reply().unwrap();
        assert!(real_wm.value.is_empty(), "a window manager runs on this display: run this test on a private Xvfb only");
        let (cl, act, close, sup, name, utf8) = (
            atom(b"_NET_CLIENT_LIST"), atom(b"_NET_ACTIVE_WINDOW"), atom(b"_NET_CLOSE_WINDOW"),
            atom(b"_NET_SUPPORTED"), atom(b"_NET_WM_NAME"), atom(b"UTF8_STRING"),
        );
        let win = wm.generate_id().unwrap();
        wm.create_window(0, win, root, 0, 0, 10, 10, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap();
        let title = "Ideas \"draft\" - Vault Ω é - Obsidian v1.6.7";
        wm.change_property8(PropMode::REPLACE, win, name, utf8, title.as_bytes()).unwrap();

        // Before a window manager publishes anything, nothing is observed.
        let ewmh = x11::Ewmh::connect(None).unwrap();
        assert!(ewmh.client_list().is_err());

        wm.change_property32(PropMode::REPLACE, root, cl, x11rb::protocol::xproto::AtomEnum::WINDOW, &[win]).unwrap();
        wm.change_property32(PropMode::REPLACE, root, sup, x11rb::protocol::xproto::AtomEnum::ATOM, &[cl, act, close]).unwrap();
        wm.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT)).unwrap();
        wm.flush().unwrap();

        let wins = ewmh.titled_windows().unwrap();
        assert_eq!(wins, vec![(win, title.to_string())]);
        assert_eq!(crate::vault_from_title(&wins[0].1).as_deref(), Some("Vault Ω é"));
        assert!(ewmh.supports("active") && ewmh.supports("close"));

        ewmh.request_close(win).unwrap();
        ewmh.request_activate(win).unwrap();
        let mut got = Vec::new();
        while got.len() < 2 {
            if let Event::ClientMessage(ev) = wm.wait_for_event().unwrap() {
                got.push((ev.type_, ev.window, ev.data.as_data32()[1]));
            }
        }
        assert_eq!(got, vec![(close, win, 2), (act, win, 0)]);

        // Chords: hvelf's grab works, a second client's is a reported conflict,
        // and a synthetic press of the grave key reaches hvelf.
        let g = x11::Grabber::connect(None).unwrap();
        let chord = x11::parse_chord("alt+grave", |s| g.keycode_for(s)).unwrap();
        g.grab(chord).unwrap();
        let other = x11::Grabber::connect(None).unwrap();
        assert_eq!(other.grab(chord), Err(x11::GrabFailure::Conflict));
        use x11rb::protocol::xtest::ConnectionExt as _;
        let alt = g.keycode_for(0xffe9).expect("Alt_L in keymap");
        for (kind, key) in [(2u8, alt), (2, chord.keycode), (3, chord.keycode), (3, alt)] {
            other.conn.xtest_fake_input(kind, key, 0, root, 0, 0, 0).unwrap();
        }
        other.conn.flush().unwrap();
        assert_eq!(g.next_press(&[chord]), Some(0));

        // With Num Lock on, then Caps Lock as well, then both off again, the chord still reaches hvelf
        // (DELIVERY-11). XTEST presses the lock keys as a person does, and the live state is checked first, so
        // the test cannot pass with a lock that never came on.
        let tap = |k: u8| {
            for kind in [2u8, 3] {
                other.conn.xtest_fake_input(kind, k, 0, root, 0, 0, 0).unwrap();
            }
            other.conn.flush().unwrap();
        };
        let press = || {
            for (kind, key) in [(2u8, alt), (2, chord.keycode), (3, chord.keycode), (3, alt)] {
                other.conn.xtest_fake_input(kind, key, 0, root, 0, 0, 0).unwrap();
            }
            other.conn.flush().unwrap();
        };
        // The lock state as the server has it once it has handled the taps: asked on the injecting connection,
        // and waited for, since injected input is processed after the request that carried it.
        let locks_become = |want: u16| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let m = u16::from(other.conn.query_pointer(root).unwrap().reply().unwrap().mask) & 0b1_0010;
                if m == want || std::time::Instant::now() > deadline {
                    return m;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let soon = || std::time::Instant::now() + std::time::Duration::from_secs(5);
        let num = g.keycode_for(0xff7f).expect("Num_Lock in keymap");
        let caps = g.keycode_for(0xffe5).expect("Caps_Lock in keymap");
        assert_eq!(g.ignored & (1 << 4), 1 << 4, "Num Lock's modifier, Mod2 here, is ignored");
        tap(num);
        assert_eq!(locks_become(0b1_0000), 0b1_0000, "Num Lock is on");
        press();
        assert_eq!(g.next_press_until(&[chord], soon()), Some(0), "the chord with Num Lock on");
        tap(caps);
        assert_eq!(locks_become(0b1_0010), 0b1_0010, "Num Lock and Caps Lock are on");
        press();
        assert_eq!(g.next_press_until(&[chord], soon()), Some(0), "the chord with Num Lock and Caps Lock on");
        tap(num);
        tap(caps);
        assert_eq!(locks_become(0), 0, "both are off again");
        press();
        assert_eq!(g.next_press_until(&[chord], soon()), Some(0), "the chord with both off");
    }
}
