//! Global hotkeys on macOS, through Carbon's RegisterEventHotKey.
//!
//! Carbon is deprecated and this is still the right call. The alternative,
//! a CGEventTap, sees every keystroke in the system and demands the
//! accessibility permission to do it; RegisterEventHotKey asks the window
//! server for one chord and is handed only that chord, with no permission
//! and no prompt. hvelf should not need to watch the user type in order to
//! answer one key.
//!
//! Keys are identified by virtual keycode, never by character, for the same
//! reason the Windows side goes through scancode 0x29: the code is the
//! physical key, not the character printed on it. One code is not enough
//! though. Scancode 0x29 is the key under Esc on every PC keyboard, while
//! macOS splits that key in two: 0x32 on an ANSI board, 0x0A on an ISO one,
//! where the backtick legend moves down beside left shift. Binding only
//! 0x32 on a UK MacBook binds a key the user is not pressing, and the press
//! goes through as a character.

use std::ffi::c_void;
use std::os::raw::c_int;

use tauri::AppHandle;

use crate::{do_launch, most_recent_vault, toggle_window, Config, FocusHistory};

// ------------------------------------------------------------------ carbon

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

type EventRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventHandlerUPP =
    extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> c_int;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: EventHandlerUPP,
        num_types: u32,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        handler_ref: *mut *mut c_void,
    ) -> c_int;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        hot_key_id: EventHotKeyID,
        target: *mut c_void,
        options: u32,
        out_ref: *mut *mut c_void,
    ) -> c_int;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        param_type: u32,
        out_actual_type: *mut u32,
        buffer_size: usize,
        out_actual_size: *mut usize,
        out_data: *mut c_void,
    ) -> c_int;
}

/// Four-character codes, spelled out rather than left as magic numbers.
const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

const K_EVENT_CLASS_KEYBOARD: u32 = fourcc(b"keyb");
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = fourcc(b"----");
const TYPE_EVENT_HOT_KEY_ID: u32 = fourcc(b"hkid");
const HVELF_SIGNATURE: u32 = fourcc(b"hvlf");

// Carbon modifier masks. Not the same values as the Cocoa ones, and not the
// Windows ones either, so they are written out rather than borrowed.
const CMD_KEY: u32 = 0x0100;
const SHIFT_KEY: u32 = 0x0200;
const OPTION_KEY: u32 = 0x0800;
const CONTROL_KEY: u32 = 0x1000;

// ------------------------------------------------------------------ parsing

pub struct HotSpec {
    pub mods: u32,
    /// Every physical key that should answer to this name. Usually one, but
    /// "the key under Esc" is two different codes depending on the keyboard.
    pub codes: Vec<u32>,
}

/// Virtual keycode for a key name. Physical positions on the ANSI layout,
/// which is what the codes mean: 0x00 is where `a` sits on a US keyboard and
/// stays there whatever that key produces under the current layout.
fn keycode(name: &str) -> Option<Vec<u32>> {
    // The key under Esc is 0x32 on an ANSI board and 0x0A (kVK_ISO_Section) on
    // an ISO one, where the backtick legend moves to the key beside left
    // shift. Windows needs no such care: scancode 0x29 is that key on both.
    // Registering the pair is what makes `grave` mean the same physical key
    // here as it does there, on a UK MacBook as on a US one.
    if matches!(name, "grave" | "backquote" | "`" | "section") {
        return Some(vec![0x32, 0x0A]);
    }
    let code = match name {
        "a" => 0x00, "s" => 0x01, "d" => 0x02, "f" => 0x03, "h" => 0x04,
        "g" => 0x05, "z" => 0x06, "x" => 0x07, "c" => 0x08, "v" => 0x09,
        "b" => 0x0B, "q" => 0x0C, "w" => 0x0D, "e" => 0x0E, "r" => 0x0F,
        "y" => 0x10, "t" => 0x11, "1" => 0x12, "2" => 0x13, "3" => 0x14,
        "4" => 0x15, "6" => 0x16, "5" => 0x17, "9" => 0x19, "7" => 0x1A,
        "8" => 0x1C, "0" => 0x1D, "o" => 0x1F, "u" => 0x20, "i" => 0x22,
        "p" => 0x23, "l" => 0x25, "j" => 0x26, "k" => 0x28, "n" => 0x2D,
        "m" => 0x2E,
        "space" => 0x31,
        "f1" => 0x7A, "f2" => 0x78, "f3" => 0x63, "f4" => 0x76,
        "f5" => 0x60, "f6" => 0x61, "f7" => 0x62, "f8" => 0x64,
        "f9" => 0x65, "f10" => 0x6D, "f11" => 0x67, "f12" => 0x6F,
        _ => return None,
    };
    Some(vec![code])
}

/// Parse the same hotkey strings the Windows side accepts, so one config
/// file serves both machines. "alt" is spelled as the user thinks of it;
/// on this platform it is the option key.
pub fn parse_hotkey(s: &str) -> Option<HotSpec> {
    let mut mods = 0u32;
    let mut code: Option<Vec<u32>> = None;
    for tok in s.split('+') {
        let t = tok.trim().to_lowercase();
        match t.as_str() {
            "alt" | "option" | "opt" => mods |= OPTION_KEY,
            "ctrl" | "control" => mods |= CONTROL_KEY,
            "shift" => mods |= SHIFT_KEY,
            // A Windows config saying "win" means the command key here, which
            // is the same chord under the same thumb.
            "win" | "super" | "meta" | "cmd" | "command" => mods |= CMD_KEY,
            key => {
                let key = key.strip_prefix("digit").unwrap_or(key);
                let key = key.strip_prefix("key").unwrap_or(key);
                code = keycode(key);
            }
        }
    }
    code.map(|codes| HotSpec { mods, codes })
}

// ------------------------------------------------------------------ handler

struct Ctx {
    app: AppHandle,
    cfg: Config,
    hist: FocusHistory,
}

extern "C" fn on_hotkey(
    _call: EventHandlerCallRef,
    event: EventRef,
    user: *mut c_void,
) -> c_int {
    // The context outlives the app: it is leaked deliberately at install
    // time, so this pointer is valid for as long as events can arrive.
    let ctx = unsafe { &*(user as *const Ctx) };

    let mut id = EventHotKeyID { signature: 0, id: 0 };
    let ok = unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyID>(),
            std::ptr::null_mut(),
            &mut id as *mut EventHotKeyID as *mut c_void,
        )
    };
    if ok != 0 || id.signature != HVELF_SIGNATURE {
        return 0;
    }

    eprintln!("hvelf: hotkey fired (id {})", id.id);
    // Ids are allocated per action; the second code registered for one action
    // gets id + 10, so both grave keys drive the same thing.
    match id.id % 10 {
        1 => toggle_window(&ctx.app),
        2 => {
            if let Some(name) = most_recent_vault(&ctx.cfg, &ctx.hist) {
                do_launch(&ctx.app, &ctx.cfg, &name);
            }
        }
        _ => {}
    }
    0
}

// ------------------------------------------------------------------ install

/// Register the chords. Must run on the main thread: the handler is attached
/// to the application event target, and it is the app's own run loop that
/// dispatches to it. Unlike the Windows path there is no private thread and
/// no message pump of our own to write.
pub fn install(app: AppHandle, cfg: Config, hist: FocusHistory) {
    let ctx = Box::into_raw(Box::new(Ctx {
        app,
        cfg: cfg.clone(),
        hist,
    })) as *mut c_void;

    let spec = EventTypeSpec {
        event_class: K_EVENT_CLASS_KEYBOARD,
        event_kind: K_EVENT_HOT_KEY_PRESSED,
    };

    unsafe {
        let target = GetApplicationEventTarget();
        let mut handler_ref: *mut c_void = std::ptr::null_mut();
        if InstallEventHandler(target, on_hotkey, 1, &spec, ctx, &mut handler_ref) != 0 {
            eprintln!("hvelf: could not install the hotkey handler");
            return;
        }

        let register = |name: &str, spec_str: &str, id: u32| {
            if spec_str.is_empty() {
                return;
            }
            match parse_hotkey(spec_str) {
                Some(h) => {
                  for (n, code) in h.codes.iter().enumerate() {
                    let this_id = id + (n as u32) * 10;
                    let mut hk: *mut c_void = std::ptr::null_mut();
                    let status = RegisterEventHotKey(
                        *code,
                        h.mods,
                        EventHotKeyID { signature: HVELF_SIGNATURE, id: this_id },
                        target,
                        0,
                        &mut hk,
                    );
                    // eventHotKeyExistsErr (-9878) is the specific and common
                    // failure: another app already owns the chord. Say which,
                    // rather than reporting a bare number.
                    if status != 0 {
                        if status == -9878 {
                            eprintln!("hvelf: {name} '{spec_str}' (code {code:#04x}) is taken by another app");
                        } else {
                            eprintln!("hvelf: {name} '{spec_str}' (code {code:#04x}) failed ({status})");
                        }
                    } else {
                        eprintln!("hvelf: {name} '{spec_str}' bound to code {code:#04x}");
                    }
                  }
                }
                None => eprintln!("hvelf: cannot parse {name} '{spec_str}'"),
            }
        };

        register("hotkey", &cfg.hotkey, 1);
        register("quickLaunch", &cfg.quick_launch, 2);
    }
}
