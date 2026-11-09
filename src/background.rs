//! Opt-in background input (`background: true`): clicks, text, keys and scrolling delivered by
//! posting window messages to the deepest child window at the point, in a window-scoped view.
//! The real cursor, the focus and the foreground window are not touched, so the user can keep
//! working. Delivery is best-effort: classic Win32 controls (edit controls, WinForms, MFC,
//! dialogs) take it; Chromium, Electron, WinUI/XAML and many games ignore posted input, and
//! shortcuts that depend on modifier key state (Ctrl+A) usually do nothing because posted
//! messages do not change the keyboard state. Verify with read_text or a screenshot.

use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC};
use windows::Win32::UI::WindowsAndMessaging::{
    ChildWindowFromPointEx, EnumChildWindows, GetGUIThreadInfo, GetWindowThreadProcessId, IsChild, IsWindowVisible, PostMessageW,
    SendMessageTimeoutW, CWP_SKIPDISABLED, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT, GUITHREADINFO, SMTO_ABORTIFHUNG,
};
use windows::core::BOOL;

use crate::{guards, input, win};

const WM_CHAR: u32 = 0x102;
const WM_KEYDOWN: u32 = 0x100;
const WM_KEYUP: u32 = 0x101;
const WM_SYSKEYDOWN: u32 = 0x104;
const WM_SYSKEYUP: u32 = 0x105;
const WM_MOUSEMOVE: u32 = 0x200;
const WM_MOUSEWHEEL: u32 = 0x20A;
const WM_MOUSEHWHEEL: u32 = 0x20E;
const WM_GETTEXT: u32 = 0x0D;
const WM_GETTEXTLENGTH: u32 = 0x0E;
const EM_REPLACESEL: u32 = 0xC2;

const NOTE: &str = "Delivered in the background by posting window messages (best-effort: Chromium, Electron and WinUI apps often ignore posted input); verify with read_text or a screenshot.";

fn post(h: HWND, msg: u32, w: usize, l: isize) -> Result<(), String> {
    unsafe { PostMessageW(Some(h), msg, WPARAM(w), LPARAM(l)) }.map_err(|e| format!("PostMessage to {}: {e}", win::class_name(h)))
}

fn lparam_xy(x: i32, y: i32) -> isize {
    (((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF)) as i32 as isize
}

/// The window of the latest window-scoped screenshot; background input needs one.
fn target() -> Result<HWND, String> {
    let v = *win::VIEW.lock().unwrap();
    v.hwnd.map(win::hwnd_from).filter(|h| win::is_window(*h)).ok_or_else(|| {
        "background=true needs a window-scoped view: call screenshot(window=...) first (and not monitor=N)".to_string()
    })
}

/// The deepest visible, enabled child window containing the screen point.
fn child_at(top: HWND, sx: i32, sy: i32) -> HWND {
    let mut cur = top;
    for _ in 0..16 {
        let mut pt = POINT { x: sx, y: sy };
        unsafe {
            let _ = ScreenToClient(cur, &mut pt);
            let c = ChildWindowFromPointEx(cur, pt, CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT);
            if c.0.is_null() || c == cur {
                break;
            }
            cur = c;
        }
    }
    cur
}

fn is_edit_class(h: HWND) -> bool {
    win::class_name(h).to_lowercase().contains("edit")
}

/// Where keyboard input goes: the window's focus control when it has one in this window, else
/// the first edit-like child, else the window itself.
fn keyboard_target(top: HWND) -> HWND {
    unsafe {
        let tid = GetWindowThreadProcessId(top, None);
        let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(tid, &mut gti).is_ok() {
            let f = gti.hwndFocus;
            if !f.0.is_null() && (f == top || IsChild(top, f).as_bool()) {
                return f;
            }
        }
        unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
            let found = &mut *(lp.0 as *mut Option<HWND>);
            if is_edit_class(h) && IsWindowVisible(h).as_bool() {
                *found = Some(h);
                return BOOL(0);
            }
            BOOL(1)
        }
        let mut found: Option<HWND> = None;
        let _ = EnumChildWindows(Some(top), Some(cb), LPARAM(&mut found as *mut _ as isize));
        found.unwrap_or(top)
    }
}

fn describe(h: HWND) -> String {
    format!("{} (#{})", win::class_name(h), h.0 as isize)
}

fn edit_text(h: HWND) -> Option<String> {
    unsafe {
        let mut len = 0usize;
        if SendMessageTimeoutW(h, WM_GETTEXTLENGTH, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 500, Some(&mut len)).0 == 0 {
            return None;
        }
        let len = len.min(100_000);
        let mut buf = vec![0u16; len + 2];
        let mut got = 0usize;
        if SendMessageTimeoutW(h, WM_GETTEXT, WPARAM(buf.len()), LPARAM(buf.as_mut_ptr() as isize), SMTO_ABORTIFHUNG, 500, Some(&mut got)).0 == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..got.min(buf.len())]))
    }
}

pub fn click(x: f64, y: f64, button: &str, clicks: u32) -> Result<String, String> {
    let top = target()?;
    guards::background_target(top)?;
    let (down, up, dbl, mk) = match button {
        "left" | "l" => (0x201u32, 0x202u32, 0x203u32, 0x1usize),
        "right" | "r" => (0x204, 0x205, 0x206, 0x2),
        "middle" | "m" => (0x207, 0x208, 0x209, 0x10),
        other => return Err(format!("button must be left, right or middle, not {other:?}")),
    };
    let (sx, sy) = win::to_screen(x, y, None);
    let child = child_at(top, sx, sy);
    guards::guard_child(top, child)?;
    let mut pt = POINT { x: sx, y: sy };
    unsafe { let _ = ScreenToClient(child, &mut pt); }
    let lp = lparam_xy(pt.x, pt.y);
    post(child, WM_MOUSEMOVE, 0, lp)?;
    for i in 0..clicks.max(1) {
        guards::check_failsafe()?;
        if i > 0 {
            std::thread::sleep(Duration::from_millis(60));
        }
        post(child, if i == 1 { dbl } else { down }, mk, lp)?;
        std::thread::sleep(Duration::from_millis(15));
        post(child, up, 0, lp)?;
    }
    let prefix = match clicks { 2 => "double-", 3 => "triple-", _ => "" };
    Ok(format!("{prefix}{button}-clicked ({x}, {y}) in the background on {}. {NOTE}", describe(child)))
}

pub fn scroll(x: f64, y: f64, amount: i32, horizontal: i32) -> Result<String, String> {
    let top = target()?;
    guards::background_target(top)?;
    let (sx, sy) = win::to_screen(x, y, None);
    let child = child_at(top, sx, sy);
    guards::guard_child(top, child)?;
    let lp = lparam_xy(sx, sy); // wheel messages carry screen coordinates
    for (units, msg) in [(amount, WM_MOUSEWHEEL), (horizontal, WM_MOUSEHWHEEL)] {
        for _ in 0..units.unsigned_abs().min(30) {
            guards::check_failsafe()?;
            let delta: i16 = if units > 0 { 120 } else { -120 };
            post(child, msg, (delta as u16 as usize) << 16, lp)?;
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(format!("scrolled {amount} vertically and {horizontal} horizontally at ({x}, {y}) in the background on {}. {NOTE}", describe(child)))
}

pub fn type_text(text: &str) -> Result<String, String> {
    if text.len() > crate::actions::MAX_TYPE_BYTES {
        return Err(format!("text is {} bytes; one type_text takes at most {} (1 MB): split it", text.len(), crate::actions::MAX_TYPE_BYTES));
    }
    let top = target()?;
    guards::background_target(top)?;
    let h = keyboard_target(top);
    guards::guard_child(top, h)?;
    // control characters (U+0003 and the like) are not text and are never posted
    let (text, _dropped) = input::sanitize_text(text);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if is_edit_class(h) {
        // classic edit control: replace the selection directly (Windows marshals the string across processes)
        let mut wide: Vec<u16> = text.replace('\n', "\r\n").encode_utf16().collect();
        wide.push(0);
        let mut res = 0usize;
        let ok = unsafe { SendMessageTimeoutW(h, EM_REPLACESEL, WPARAM(1), LPARAM(wide.as_ptr() as isize), SMTO_ABORTIFHUNG, 1500, Some(&mut res)) }.0 != 0;
        if !ok {
            return Err(format!("the edit control {} did not answer EM_REPLACESEL", describe(h)));
        }
        let now = edit_text(h).map(|t| format!(" The field now reads {:?}.", t.chars().take(200).collect::<String>())).unwrap_or_default();
        return Ok(format!("typed {} characters into {} with EM_REPLACESEL.{now} {NOTE}", text.chars().count(), describe(h)));
    }
    let mut n = 0;
    for ch in text.chars() {
        if n % 64 == 0 {
            guards::check_failsafe()?;
        }
        n += 1;
        if ch == '\n' {
            post(h, WM_CHAR, 13, 1)?;
            continue;
        }
        let mut buf = [0u16; 2];
        for unit in ch.encode_utf16(&mut buf) {
            post(h, WM_CHAR, *unit as usize, 1)?;
        }
        if n % 32 == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(format!("typed {} characters into {} as WM_CHAR messages. {NOTE}", text.chars().count(), describe(h)))
}

pub fn keys(keys: &[String]) -> Result<String, String> {
    let top = target()?;
    guards::background_target(top)?;
    guards::refuse_close_on_host(keys, top)?;
    let h = keyboard_target(top);
    guards::guard_child(top, h)?;
    let names = input::key_names(keys);
    let mut vks: Vec<(String, u16, bool)> = Vec::new();
    for n in &names {
        let (vk, shift) = input::vk_for(n).ok_or_else(|| format!("Unknown key(s): [{n:?}]"))?;
        vks.push((n.clone(), vk, shift));
    }
    let is_mod = |vk: u16| matches!(vk, 0x10 | 0x11 | 0x12 | 0xA0..=0xA5 | 0x5B | 0x5C);
    let alt = vks.iter().any(|(_, vk, _)| matches!(*vk, 0x12 | 0xA4 | 0xA5));
    let ctrl = vks.iter().any(|(_, vk, _)| matches!(*vk, 0x11 | 0xA2 | 0xA3));
    let shift = vks.iter().any(|(_, vk, s)| *s || matches!(*vk, 0x10 | 0xA0 | 0xA1));
    let lp = |vk: u16, up: bool| -> isize {
        let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) };
        let mut l = 1u32 | (scan << 16);
        if alt {
            l |= 1 << 29;
        }
        if up {
            l |= (1 << 30) | (1 << 31);
        }
        l as isize
    };
    let (kd, ku) = if alt { (WM_SYSKEYDOWN, WM_SYSKEYUP) } else { (WM_KEYDOWN, WM_KEYUP) };
    for (name, vk, needs_shift) in &vks {
        if *needs_shift {
            post(h, kd, 0x10, lp(0x10, false))?;
        }
        post(h, kd, *vk as usize, lp(*vk, false))?;
        // the character a plain key produces (no Ctrl/Alt), as TranslateMessage would post it
        if !ctrl && !alt && !is_mod(*vk) {
            let ch: Option<u32> = match *vk {
                0x0D => Some(13),
                0x09 => Some(9),
                0x08 => Some(8),
                0x1B => Some(27),
                0x20 => Some(32),
                _ => {
                    let mut it = name.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) => Some(if (shift || *needs_shift) && c.is_ascii_lowercase() { c.to_ascii_uppercase() as u32 } else { c as u32 }),
                        _ => None,
                    }
                }
            };
            if let Some(c) = ch {
                post(h, WM_CHAR, c as usize, lp(*vk, false))?;
            }
        }
    }
    std::thread::sleep(Duration::from_millis(20));
    for (_, vk, needs_shift) in vks.iter().rev() {
        post(h, ku, *vk as usize, lp(*vk, true))?;
        if *needs_shift {
            post(h, ku, 0x10, lp(0x10, true))?;
        }
    }
    let caveat = if ctrl || alt { " Shortcuts with Ctrl/Alt rarely work this way: posted messages do not change the keyboard state apps check." } else { "" };
    Ok(format!("pressed {} in the background on {}.{caveat} {NOTE}", names.join("+"), describe(h)))
}
