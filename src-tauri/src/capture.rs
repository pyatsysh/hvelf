// capture: read the user's current selection out of whichever app has the
// foreground, and say what that app is. Windows only.
//
// Two tiers. UI Automation first: ask the focused element for its
// TextPattern selection, which costs no clipboard and no injected keys and
// covers Chromium, Firefox, Electron, Word, Notepad, Acrobat and the
// terminal. If that yields nothing, a clipboard round trip: remember the
// clipboard, send Ctrl+C, wait for the sequence number to move, read the
// text, put the old clipboard back.

use std::ptr::null_mut;

use windows::core::Interface;
use windows::Win32::Foundation::HWND as WHWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern,
    IUIAutomationValuePattern, TreeScope_Descendants, UIA_ControlTypePropertyId,
    UIA_EditControlTypeId, UIA_TextPatternId, UIA_ValuePatternId,
};

// ------------------------------------------------------------- source

/// The window that had the foreground when the chord was pressed.
pub struct Source {
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
    /// Executable base name, e.g. "chrome.exe".
    pub exe: String,
}

pub fn snapshot() -> Option<Source> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };
    unsafe {
        let h = GetForegroundWindow();
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let got = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
        let title = if got > 0 {
            String::from_utf16_lossy(&buf[..got as usize])
        } else {
            String::new()
        };
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, &mut pid);
        Some(Source {
            hwnd: h as isize,
            pid,
            title,
            exe: process_exe(pid),
        })
    }
}

fn process_exe(pid: u32) -> String {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let hp = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if hp.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(hp, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(hp);
        if ok == 0 {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string()
    }
}

/// Terminals send Ctrl+C to the running program when nothing is selected,
/// so they never get the clipboard tier; UI Automation serves them.
pub fn is_terminal(exe: &str) -> bool {
    matches!(
        exe.to_ascii_lowercase().as_str(),
        "windowsterminal.exe"
            | "openconsole.exe"
            | "conhost.exe"
            | "mintty.exe"
            | "alacritty.exe"
            | "wezterm-gui.exe"
            | "putty.exe"
    )
}

pub fn is_browser(exe: &str) -> bool {
    matches!(
        exe.to_ascii_lowercase().as_str(),
        "chrome.exe"
            | "msedge.exe"
            | "firefox.exe"
            | "brave.exe"
            | "vivaldi.exe"
            | "opera.exe"
            | "librewolf.exe"
            | "zen.exe"
    )
}

// ------------------------------------------------------ UI Automation

/// Once per thread, before any UIA call. A multithreaded apartment: UIA
/// clients are told to avoid the single-threaded kind, whose outgoing calls
/// pump messages and can swallow the hotkey thread's own queue.
pub fn init_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn automation() -> Option<IUIAutomation> {
    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok() }
}

fn selection_of(el: &IUIAutomationElement) -> Option<String> {
    unsafe {
        let pat: IUIAutomationTextPattern = el.GetCurrentPattern(UIA_TextPatternId).ok()?.cast().ok()?;
        let ranges = pat.GetSelection().ok()?;
        let n = ranges.Length().ok()?;
        let mut out = String::new();
        for i in 0..n {
            if let Ok(r) = ranges.GetElement(i) {
                if let Ok(t) = r.GetText(-1) {
                    out.push_str(&String::from_utf16_lossy(&t));
                }
            }
        }
        Some(out)
    }
}

/// The selected text via UI Automation, or None if the focused element and
/// the foreground window both report nothing.
pub fn selection_uia(hwnd: isize) -> Option<String> {
    let ua = automation()?;
    let mut text = unsafe { ua.GetFocusedElement().ok() }
        .and_then(|el| selection_of(&el))
        .unwrap_or_default();
    if text.is_empty() {
        text = unsafe { ua.ElementFromHandle(WHWND(hwnd as *mut _)).ok() }
            .and_then(|el| selection_of(&el))
            .unwrap_or_default();
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn variant_i4(v: i32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: v },
            }),
        },
    }
}

/// Best effort: the first edit control in the browser window is the
/// address bar, and its value is the page URL, usually with the scheme
/// trimmed off for display.
pub fn browser_url(hwnd: isize) -> Option<String> {
    let ua = automation()?;
    unsafe {
        let root = ua.ElementFromHandle(WHWND(hwnd as *mut _)).ok()?;
        let cond = ua
            .CreatePropertyCondition(UIA_ControlTypePropertyId, &variant_i4(UIA_EditControlTypeId.0))
            .ok()?;
        let el = root.FindFirst(TreeScope_Descendants, &cond).ok()?;
        let vp: IUIAutomationValuePattern = el.GetCurrentPattern(UIA_ValuePatternId).ok()?.cast().ok()?;
        let v = String::from_utf16_lossy(&vp.CurrentValue().ok()?);
        normalise_url(&v)
    }
}

pub fn normalise_url(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() || v.contains(char::is_whitespace) {
        return None;
    }
    if v.contains("://") {
        return Some(v.to_string());
    }
    // "example.org/path" as browsers display it; a bare word is a search.
    let host = v.split('/').next().unwrap_or("");
    if host.contains('.') || host.starts_with("localhost") {
        Some(format!("https://{v}"))
    } else {
        None
    }
}

// ---------------------------------------------------------- clipboard

const CF_UNICODETEXT: u32 = 13;

/// Formats whose handle is not an HGLOBAL; copying them byte-wise would
/// be wrong, so the snapshot skips them.
fn is_global_format(f: u32) -> bool {
    !matches!(f, 2 | 3 | 14 | 0x80 | 0x82 | 0x83 | 0x8E) && !(0x200..=0x3FF).contains(&f)
}

fn open_clipboard() -> bool {
    use windows_sys::Win32::System::DataExchange::OpenClipboard;
    for _ in 0..20 {
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

struct Snapshot(Vec<(u32, Vec<u8>)>);

fn clipboard_snapshot() -> Option<Snapshot> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EnumClipboardFormats, GetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    if !open_clipboard() {
        return None;
    }
    let mut v = Vec::new();
    unsafe {
        let mut fmt = EnumClipboardFormats(0);
        while fmt != 0 {
            if is_global_format(fmt) {
                let h = GetClipboardData(fmt);
                if !h.is_null() {
                    let size = GlobalSize(h);
                    if size > 0 {
                        let p = GlobalLock(h);
                        if !p.is_null() {
                            v.push((fmt, std::slice::from_raw_parts(p as *const u8, size).to_vec()));
                            GlobalUnlock(h);
                        }
                    }
                }
            }
            fmt = EnumClipboardFormats(fmt);
        }
        CloseClipboard();
    }
    Some(Snapshot(v))
}

fn clipboard_restore(snap: Snapshot) {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    if !open_clipboard() {
        eprintln!("hvelf: could not reopen the clipboard to restore it");
        return;
    }
    unsafe {
        EmptyClipboard();
        for (fmt, bytes) in snap.0 {
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if h.is_null() {
                continue;
            }
            let p = GlobalLock(h);
            if p.is_null() {
                GlobalFree(h);
                continue;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
            GlobalUnlock(h);
            if SetClipboardData(fmt, h).is_null() {
                GlobalFree(h);
            }
        }
        CloseClipboard();
    }
}

fn clipboard_text() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    if !open_clipboard() {
        return None;
    }
    let mut out = None;
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT);
        if !h.is_null() {
            let cap = GlobalSize(h) / 2;
            let p = GlobalLock(h) as *const u16;
            if !p.is_null() {
                let mut len = 0;
                while len < cap && *p.add(len) != 0 {
                    len += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
                GlobalUnlock(h);
            }
        }
        CloseClipboard();
    }
    out
}

/// Ctrl+C as the app would see it from the keyboard. The user is still
/// holding the chord's modifiers, so those are released first (Ctrl goes
/// down before Alt comes up, which keeps Alt's release from opening a
/// menu bar); the physical release that follows later is a no-op.
fn send_ctrl_c() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        VK_CONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
    };
    fn key(vk: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
    let mut seq = vec![key(VK_CONTROL, false)];
    for vk in [VK_MENU, VK_LMENU, VK_RMENU, VK_SHIFT, VK_LSHIFT, VK_RSHIFT, VK_LWIN, VK_RWIN] {
        if unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0 {
            seq.push(key(vk, true));
        }
    }
    seq.push(key(0x43, false)); // C
    seq.push(key(0x43, true));
    seq.push(key(VK_CONTROL, true));
    unsafe {
        SendInput(seq.len() as u32, seq.as_ptr(), std::mem::size_of::<INPUT>() as i32);
    }
}

/// The clipboard round trip. None if the app put nothing new on the
/// clipboard within the wait, which is what "no selection" looks like.
pub fn selection_clipboard() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
    let before = clipboard_snapshot();
    let seq0 = unsafe { GetClipboardSequenceNumber() };
    send_ctrl_c();
    let mut changed = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        if unsafe { GetClipboardSequenceNumber() } != seq0 {
            changed = true;
            break;
        }
    }
    let text = if changed { clipboard_text() } else { None };
    if changed {
        if let Some(snap) = before {
            clipboard_restore(snap);
        }
    }
    text.filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_normalisation() {
        assert_eq!(normalise_url("https://a.org/x"), Some("https://a.org/x".into()));
        assert_eq!(normalise_url("a.org/x?y=1"), Some("https://a.org/x?y=1".into()));
        assert_eq!(normalise_url("localhost:8080/p"), Some("https://localhost:8080/p".into()));
        assert_eq!(normalise_url("search terms"), None);
        assert_eq!(normalise_url("word"), None);
        assert_eq!(normalise_url(""), None);
    }

    #[test]
    fn format_filter() {
        assert!(is_global_format(13));
        assert!(is_global_format(8)); // CF_DIB
        assert!(is_global_format(49999)); // a registered format
        assert!(!is_global_format(2)); // CF_BITMAP
        assert!(!is_global_format(0x250)); // private
        assert!(!is_global_format(0x300)); // GDI object
    }

    #[test]
    fn exe_classes() {
        assert!(is_terminal("WindowsTerminal.exe"));
        assert!(!is_terminal("chrome.exe"));
        assert!(is_browser("MSEDGE.EXE"));
        assert!(!is_browser("WINWORD.EXE"));
    }
}
