//! Input injection: pointer motion along a gentle eased arc, clicks, scrolling, dragging,
//! key chords (pyautogui names, X keysyms, "+" chords) and text (clipboard paste with the
//! user's clipboard saved and restored, or Unicode keystrokes).

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW, GetClipboardSequenceNumber, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, VkKeyScanW, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};
use windows::core::w;

pub fn send(events: &[INPUT]) {
    unsafe { SendInput(events, std::mem::size_of::<INPUT>() as i32) };
}

fn key_event(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(flags),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_event(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe { let _ = GetCursorPos(&mut p); }
    (p.x, p.y)
}

/// Move the pointer to a virtual-desktop point with SendInput (injected, so the hooks know it is ours).
pub fn move_abs(x: i32, y: i32) {
    unsafe {
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2), GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2));
        let nx = ((x - vx) as f64 * 65535.0 / (vw - 1) as f64).round() as i32;
        let ny = ((y - vy) as f64 * 65535.0 / (vh - 1) as f64).round() as i32;
        send(&[mouse_event(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK)]);
    }
}

/// Move the pointer to (x, y) along a gentle arc: a cubic Bezier whose two inner control points
/// sit a third and two thirds of the way, both pushed sideways by the same small bow (8 % of the
/// distance, at most 36 px), eased in and out with a smoothstep. Hops under 6 px just jump.
pub fn glide(x: i32, y: i32, duration: Option<f64>) {
    let (x0, y0) = cursor_pos();
    let (fx0, fy0, fx, fy) = (x0 as f64, y0 as f64, x as f64, y as f64);
    let d = ((fx - fx0).powi(2) + (fy - fy0).powi(2)).sqrt();
    if d >= 6.0 {
        let duration = duration.unwrap_or_else(|| (0.12 + d / 3000.0).min(0.45));
        // unit vector across the path (to the left of the direction of travel)
        let (nx, ny) = (-(fy - fy0) / d, (fx - fx0) / d);
        let bow = (d * 0.08).min(36.0);
        let p1 = (fx0 + (fx - fx0) / 3.0 + nx * bow, fy0 + (fy - fy0) / 3.0 + ny * bow);
        let p2 = (fx0 + (fx - fx0) * 2.0 / 3.0 + nx * bow, fy0 + (fy - fy0) * 2.0 / 3.0 + ny * bow);
        let t0 = Instant::now();
        loop {
            let u = (t0.elapsed().as_secs_f64() / duration).min(1.0);
            let e = u * u * (3.0 - 2.0 * u);
            let (a, b, c, dd) = ((1.0 - e).powi(3), 3.0 * (1.0 - e).powi(2) * e, 3.0 * (1.0 - e) * e * e, e.powi(3));
            let px = (a * fx0 + b * p1.0 + c * p2.0 + dd * fx).round() as i32;
            let py = (a * fy0 + b * p1.1 + c * p2.1 + dd * fy).round() as i32;
            move_abs(px, py);
            if u >= 1.0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
    }
    unsafe { let _ = SetCursorPos(x, y); }
}

pub fn button_flags(button: &str) -> Result<(MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS), String> {
    Ok(match button {
        "left" | "l" => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        "right" | "r" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        "middle" | "m" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        other => return Err(format!("button must be left, right or middle, not {other:?}")),
    })
}

/// A mouse button held down by us. Whatever happens next (an error, a panic caught by the tool
/// registry), dropping it lets the button go, so a button is never left pressed.
pub struct ButtonHold {
    up: Option<MOUSE_EVENT_FLAGS>,
}

impl ButtonHold {
    pub fn press(button: &str) -> Result<ButtonHold, String> {
        let (down, up) = button_flags(button)?;
        send(&[mouse_event(0, 0, 0, down)]);
        Ok(ButtonHold { up: Some(up) })
    }

    pub fn release(mut self) {
        if let Some(u) = self.up.take() {
            send(&[mouse_event(0, 0, 0, u)]);
        }
    }
}

impl Drop for ButtonHold {
    fn drop(&mut self) {
        if let Some(u) = self.up.take() {
            send(&[mouse_event(0, 0, 0, u)]);
        }
    }
}

/// Click at a screen point; `check` runs right before every button press (the last look).
pub fn click_at(x: i32, y: i32, button: &str, clicks: u32, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
    button_flags(button)?;
    unsafe { let _ = SetCursorPos(x, y); }
    for i in 0..clicks.max(1) {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(80));
        }
        check()?;
        let hold = ButtonHold::press(button)?;
        std::thread::sleep(Duration::from_millis(15));
        hold.release();
    }
    Ok(())
}

pub fn scroll(vertical: i32, horizontal: i32) {
    if vertical != 0 {
        send(&[mouse_event(0, 0, vertical * 120, MOUSEEVENTF_WHEEL)]);
    }
    if horizontal != 0 {
        send(&[mouse_event(0, 0, horizontal * 120, MOUSEEVENTF_HWHEEL)]);
    }
}

// ---------- keys ----------

fn key_table() -> &'static HashMap<&'static str, u16> {
    static T: OnceLock<HashMap<&'static str, u16>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m: HashMap<&'static str, u16> = HashMap::new();
        let pairs: &[(&str, u16)] = &[
            ("backspace", 0x08), ("tab", 0x09), ("clear", 0x0C), ("enter", 0x0D), ("return", 0x0D), ("shift", 0x10),
            ("ctrl", 0x11), ("alt", 0x12), ("pause", 0x13), ("capslock", 0x14), ("esc", 0x1B), ("escape", 0x1B),
            ("space", 0x20), ("pageup", 0x21), ("pagedown", 0x22), ("end", 0x23), ("home", 0x24), ("left", 0x25),
            ("up", 0x26), ("right", 0x27), ("down", 0x28), ("select", 0x29), ("print", 0x2A), ("execute", 0x2B),
            ("printscreen", 0x2C), ("prtsc", 0x2C), ("insert", 0x2D), ("delete", 0x2E), ("del", 0x2E), ("help", 0x2F),
            ("win", 0x5B), ("winleft", 0x5B), ("winright", 0x5C), ("apps", 0x5D), ("sleep", 0x5F),
            ("num0", 0x60), ("num1", 0x61), ("num2", 0x62), ("num3", 0x63), ("num4", 0x64), ("num5", 0x65),
            ("num6", 0x66), ("num7", 0x67), ("num8", 0x68), ("num9", 0x69), ("multiply", 0x6A), ("add", 0x6B),
            ("separator", 0x6C), ("subtract", 0x6D), ("decimal", 0x6E), ("divide", 0x6F),
            ("numlock", 0x90), ("scrolllock", 0x91), ("shiftleft", 0xA0), ("shiftright", 0xA1), ("ctrlleft", 0xA2),
            ("ctrlright", 0xA3), ("altleft", 0xA4), ("altright", 0xA5), ("browserback", 0xA6), ("browserforward", 0xA7),
            ("browserrefresh", 0xA8), ("browserstop", 0xA9), ("browsersearch", 0xAA), ("browserfavorites", 0xAB),
            ("browserhome", 0xAC), ("volumemute", 0xAD), ("volumedown", 0xAE), ("volumeup", 0xAF), ("nexttrack", 0xB0),
            ("prevtrack", 0xB1), ("stop", 0xB2), ("playpause", 0xB3), ("launchmail", 0xB4), ("launchmediaselect", 0xB5),
            ("launchapp1", 0xB6), ("launchapp2", 0xB7),
        ];
        m.extend(pairs.iter().copied());
        for i in 1..=24u16 {
            m.insert(Box::leak(format!("f{i}").into_boxed_str()), 0x70 + i - 1);
        }
        m
    })
}

fn aliases() -> &'static HashMap<&'static str, &'static str> {
    static A: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    A.get_or_init(|| {
        let mut m: HashMap<&'static str, &'static str> = HashMap::new();
        let pairs: &[(&str, &str)] = &[
            ("control", "ctrl"), ("control_l", "ctrlleft"), ("control_r", "ctrlright"), ("ctrl_l", "ctrlleft"),
            ("ctrl_r", "ctrlright"), ("alt_l", "altleft"), ("alt_r", "altright"), ("option", "alt"), ("option_l", "altleft"),
            ("option_r", "altright"), ("shift_l", "shiftleft"), ("shift_r", "shiftright"), ("super", "win"),
            ("super_l", "winleft"), ("super_r", "winright"), ("meta", "win"), ("meta_l", "winleft"), ("meta_r", "winright"),
            ("windows", "win"), ("win_l", "winleft"), ("win_r", "winright"), ("cmd", "win"), ("command", "win"), ("os", "win"),
            ("linefeed", "enter"), ("kp_enter", "enter"), ("numpad_enter", "enter"), ("back_space", "backspace"),
            ("prior", "pageup"), ("page_up", "pageup"), ("next", "pagedown"), ("page_down", "pagedown"), ("ins", "insert"),
            ("caps_lock", "capslock"), ("num_lock", "numlock"), ("scroll_lock", "scrolllock"), ("sys_req", "printscreen"),
            ("menu", "apps"), ("kp_add", "add"), ("kp_plus", "add"), ("numpad_add", "add"), ("kp_subtract", "subtract"),
            ("kp_minus", "subtract"), ("numpad_subtract", "subtract"), ("kp_multiply", "multiply"),
            ("numpad_multiply", "multiply"), ("kp_divide", "divide"), ("numpad_divide", "divide"), ("kp_decimal", "decimal"),
            ("numpad_decimal", "decimal"), ("kp_separator", "separator"), ("numpad_separator", "separator"),
            ("period", "."), ("dot", "."), ("comma", ","), ("slash", "/"), ("backslash", "\\"), ("minus", "-"),
            ("hyphen", "-"), ("plus", "+"), ("equal", "="), ("equals", "="), ("semicolon", ";"), ("colon", ":"),
            ("apostrophe", "'"), ("quote", "'"), ("grave", "`"), ("backtick", "`"), ("tilde", "~"), ("less", "<"),
            ("greater", ">"), ("question", "?"), ("bracketleft", "["), ("bracketright", "]"), ("braceleft", "{"),
            ("braceright", "}"), ("bar", "|"), ("pipe", "|"), ("underscore", "_"), ("exclam", "!"), ("at", "@"),
            ("numbersign", "#"), ("dollar", "$"), ("percent", "%"), ("asciicircum", "^"), ("ampersand", "&"),
            ("asterisk", "*"), ("parenleft", "("), ("parenright", ")"),
        ];
        m.extend(pairs.iter().copied());
        for i in 0..10 {
            for pat in ["kp_{}", "kp{}", "numpad_{}", "numpad{}"] {
                m.insert(Box::leak(pat.replace("{}", &i.to_string()).into_boxed_str()), Box::leak(format!("num{i}").into_boxed_str()));
            }
        }
        m
    })
}

/// Normalise key names: pyautogui names, X keysym names and "+" chords.
pub fn key_names(keys: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for key in keys {
        let parts: Vec<&str> = if key.chars().count() > 1 && key.contains('+') { key.split('+').map(str::trim).collect() } else { vec![key.as_str()] };
        for p in parts {
            if p.is_empty() {
                continue;
            }
            let low = p.to_lowercase();
            out.push(aliases().get(low.as_str()).map(|s| s.to_string()).unwrap_or(low));
        }
    }
    out
}

/// (virtual key, needs shift) for a key name, or None when unknown.
pub fn vk_for(name: &str) -> Option<(u16, bool)> {
    if let Some(&vk) = key_table().get(name) {
        return Some((vk, false));
    }
    let mut chars = name.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let r = unsafe { VkKeyScanW(ch as u16) };
    if r == -1 {
        return None;
    }
    Some(((r & 0xFF) as u16, (r >> 8) & 1 != 0))
}

/// Is this virtual key a modifier (held while the other keys of a chord are pressed)?
fn is_modifier(vk: u16) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

/// Keys whose scan code needs the extended-key prefix (E0).
fn is_extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2C | 0x2D | 0x2E | 0x5B | 0x5C | 0x5D | 0x6F | 0x90 | 0xA3 | 0xA5 | 0xA6..=0xB7)
}

/// A key event for a virtual key, with its hardware scan code (and the extended-key flag) filled
/// in, as a real keyboard sends it: apps and frameworks that read the scan code or rebuild the
/// modifier state from it then see an ordinary key press.
fn vk_event(vk: u16, up: bool) -> INPUT {
    let scan = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::MapVirtualKeyW(vk as u32, windows::Win32::UI::Input::KeyboardAndMouse::MAPVK_VK_TO_VSC) } as u16;
    let mut flags = if up { KEYEVENTF_KEYUP.0 } else { 0 };
    if is_extended(vk) {
        flags |= windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_EXTENDEDKEY.0;
    }
    key_event(vk, scan, flags)
}

/// Give a busy target window a chance to catch up: a message sent to the foreground window is only
/// answered once its thread is pumping messages again (bounded, a hung window is not waited for).
fn wait_target_responsive(max_ms: u32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL};
    unsafe {
        let fg = GetForegroundWindow();
        if !fg.0.is_null() {
            let _ = SendMessageTimeoutW(fg, WM_NULL, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0), SMTO_ABORTIFHUNG, max_ms, None);
        }
    }
}

/// How a wait for an idle target ended.
pub enum Idle {
    /// the target answered promptly three times in a row
    Idle,
    /// the time ran out while it was still busy
    TimedOut,
    /// the user's stop or the client's cancellation came first (the text is what the guard said)
    Aborted(String),
}

/// A window answered WM_NULL within 120 ms (a busy thread does not answer until it is back at its message loop).
fn answers_promptly(h: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL};
    let t = Instant::now();
    let answered = unsafe { SendMessageTimeoutW(h, WM_NULL, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0), SMTO_ABORTIFHUNG, 150, None) }.0 != 0;
    answered && t.elapsed() < Duration::from_millis(120)
}

/// The windows whose threads must be idle: the one in front and, when the focused control belongs to another thread than
/// the window in front (an embedded or hosted control with its own thread), that control's window as well.
fn idle_probe_windows() -> Vec<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO};
    let mut out = Vec::new();
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return out;
        }
        out.push(fg);
        let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(0, &mut gti).is_ok() && !gti.hwndFocus.0.is_null() && gti.hwndFocus != fg {
            if GetWindowThreadProcessId(gti.hwndFocus, None) != GetWindowThreadProcessId(fg, None) {
                out.push(gti.hwndFocus);
            }
        }
    }
    out
}

/// Waits until the window in front, and the focused control's window when that belongs to another thread, are idle: each
/// answers WM_NULL promptly three times in a row, 60 ms apart. A busy thread does not answer until it is back at its message
/// loop, and input it received meanwhile (a Ctrl+V) is handled right after the first answer, which is why one answer is not
/// enough. Gives up after `max`, and at once when the user's stop or the client's cancellation comes (checked between probes).
fn wait_target_idle(max: Duration) -> Idle {
    let start = Instant::now();
    let mut in_a_row = 0;
    while start.elapsed() < max {
        if let Err(e) = crate::guards::check_failsafe() {
            return Idle::Aborted(e);
        }
        let targets = idle_probe_windows();
        if targets.is_empty() {
            return Idle::Idle; // nothing in front to be busy
        }
        if targets.iter().all(|h| answers_promptly(*h)) {
            in_a_row += 1;
            if in_a_row >= 3 {
                return Idle::Idle;
            }
            std::thread::sleep(Duration::from_millis(60));
        } else {
            in_a_row = 0;
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Idle::TimedOut
}

/// How long one typing may wait in total for a busy target before it gives the clipboard back anyway.
const IDLE_BUDGET: Duration = Duration::from_secs(12);

/// Set when the last action was keyboard input: the pointer may be hidden by Windows a moment later.
pub static KEYBOARD_ACTIVITY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keys held down by us (modifiers of a chord, the chord's keys). Dropping it releases whatever is
/// still down, in reverse, so an error or a caught panic never leaves Ctrl, Alt, Shift or Win pressed.
#[derive(Default)]
pub struct KeysDown {
    vks: Vec<u16>,
}

impl KeysDown {
    pub fn down(&mut self, vks: &[u16]) {
        let ev: Vec<INPUT> = vks.iter().map(|&v| vk_event(v, false)).collect();
        send(&ev);
        self.vks.extend_from_slice(vks);
    }

    pub fn up(&mut self, vks: &[u16]) {
        let ev: Vec<INPUT> = vks.iter().rev().map(|&v| vk_event(v, true)).collect();
        send(&ev);
        self.vks.retain(|v| !vks.contains(v));
    }
}

impl Drop for KeysDown {
    fn drop(&mut self) {
        if !self.vks.is_empty() {
            let ev: Vec<INPUT> = self.vks.iter().rev().map(|&v| vk_event(v, true)).collect();
            send(&ev);
        }
    }
}

/// Press a chord. Like a real keyboard: the modifiers go down first, then the other keys, which are
/// held until the target has had time to read the modifier state (WinUI apps such as the new
/// Notepad and busy Chromium windows read it late, so a chord released within a few
/// milliseconds arrives as the plain key: "ctrl+a" typed "a", "alt+F4" did nothing), and the
/// modifiers are released last. A lone key or a lone modifier is just pressed and released.
/// `check` runs after the wait for a busy target and right before the first key goes down.
pub fn press_checked(keys: &[String], check: &dyn Fn() -> Result<(), String>) -> Result<String, String> {
    let names = key_names(keys);
    let mut vks = Vec::new();
    for n in &names {
        match vk_for(n) {
            Some(v) => vks.push(v),
            None => return Err(format!("Unknown key(s): [{n:?}]")),
        }
    }
    KEYBOARD_ACTIVITY.store(true, std::sync::atomic::Ordering::Relaxed);
    if std::env::var_os("DC_LEGACY_KEYS").is_some() {
        check()?;
        press_legacy(&vks);
        return Ok(format!("pressed {}", names.join("+")));
    }
    let mut mods: Vec<u16> = Vec::new();
    let mut rest: Vec<u16> = Vec::new();
    for (vk, shift) in &vks {
        if *shift && !mods.contains(&0x10) {
            mods.push(0x10);
        }
        if is_modifier(*vk) {
            if !mods.contains(vk) {
                mods.push(*vk);
            }
        } else {
            rest.push(*vk);
        }
    }
    chord(&mods, &rest, check)?;
    Ok(format!("pressed {}", names.join("+")))
}

/// Modifiers down, keys down, hold, keys up, modifiers up (see `press_checked`).
fn chord(mods: &[u16], rest: &[u16], check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
    let mut held = KeysDown::default();
    if rest.is_empty() {
        // only modifiers: press them all together, then release in reverse
        check()?;
        held.down(mods);
        std::thread::sleep(Duration::from_millis(40));
        held.up(mods);
        return Ok(());
    }
    let is_chord = !mods.is_empty();
    if is_chord {
        wait_target_responsive(250);
    }
    check()?;
    if is_chord {
        held.down(mods);
        std::thread::sleep(Duration::from_millis(10));
    }
    held.down(rest);
    // hold: long enough for a late reader, after confirming the target is not stuck in a long handler
    std::thread::sleep(Duration::from_millis(if is_chord { 30 } else { 20 }));
    held.up(rest);
    if is_chord {
        std::thread::sleep(Duration::from_millis(10));
        held.up(mods);
    }
    Ok(())
}

/// The v0.3.0 behaviour (everything down at once, 20 ms, everything up), kept behind the
/// DC_LEGACY_KEYS environment variable so tests can compare it with the new one.
fn press_legacy(vks: &[(u16, bool)]) {
    let mut down = Vec::new();
    for (vk, shift) in vks {
        if *shift {
            down.push(key_event(0x10, 0, 0));
        }
        down.push(key_event(*vk, 0, 0));
    }
    send(&down);
    std::thread::sleep(Duration::from_millis(20));
    let mut up = Vec::new();
    for (vk, shift) in vks.iter().rev() {
        up.push(key_event(*vk, 0, KEYEVENTF_KEYUP.0));
        if *shift {
            up.push(key_event(0x10, 0, KEYEVENTF_KEYUP.0));
        }
    }
    send(&up);
}

// ---------- the pointer ----------
// Windows' "Hide pointer while typing" (on by default) hides the pointer after key presses and only
// a real mouse movement brings it back. During a task the pointer must stay visible, so after keyboard actions it is woken with a small injected movement and put back
// at exactly the saved point with SetCursorPos (which does not hide it again). A 1 px relative
// move is below the acceleration threshold and does nothing; 3 px works.

fn pointer_showing() -> Option<bool> {
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorInfo, CURSORINFO};
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    unsafe { GetCursorInfo(&mut ci) }.ok()?;
    Some(ci.flags.0 & 1 != 0)
}

fn wake_disabled() -> bool {
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("DC_NO_POINTER_WAKE").is_some())
}

/// Bring a hidden pointer back, only while a task is active and only when the user is not moving
/// the mouse themselves. Returns true when it had to be woken and is visible again.
pub fn wake_pointer() -> bool {
    wake(false)
}

/// At the end of a task (finish, turn ended, idle): same, but the task is already over, and the
/// user typing themselves (their own hidden pointer) is left alone.
pub fn wake_pointer_at_end() -> bool {
    wake(true)
}

fn wake(at_end: bool) -> bool {
    use std::sync::atomic::Ordering;
    if wake_disabled() || (!at_end && !crate::hooks::ACTIVE.load(Ordering::Relaxed)) {
        return false;
    }
    // Windows hides the pointer a moment after the key reaches the app: after keyboard actions look
    // for a short while before concluding that it is still showing
    if KEYBOARD_ACTIVITY.swap(false, Ordering::Relaxed) {
        let end = Instant::now() + Duration::from_millis(15);
        while pointer_showing() == Some(true) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(3));
        }
    }
    if at_end && crate::hooks::user_typing_within(1500) {
        return false;
    }
    wake_now()
}

fn wake_now() -> bool {
    if pointer_showing() != Some(false) {
        return false; // visible already (or unknown)
    }
    // never while the user is moving the mouse themselves (their own movement shows it anyway)
    let user_at = crate::hooks::user_mouse_at_us();
    if crate::hooks::user_mouse_within(500) && user_at > crate::guards::BASELINE_US.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    let (x, y) = cursor_pos();
    for step in [3, 6] {
        // toward the inside of the screen: away from a right-hand edge
        let right_edge = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) } - 8;
        let dx = if x >= right_edge { -step } else { step };
        send(&[mouse_event(dx, 0, 0, MOUSEEVENTF_MOVE)]);
        let end = Instant::now() + Duration::from_millis(30);
        while pointer_showing() == Some(false) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(2));
        }
        unsafe { let _ = SetCursorPos(x, y); }
        if pointer_showing() == Some(true) {
            if std::env::var_os("DC_TRACE").is_some() {
                eprintln!("wake_pointer: pointer was hidden, shown again at ({x}, {y}) (now {:?})", cursor_pos());
            }
            return true;
        }
    }
    if std::env::var_os("DC_TRACE").is_some() {
        eprintln!("wake_pointer: pointer still hidden after the nudge");
    }
    false
}

/// Text as it is typed: control characters other than tab and line breaks are dropped. A Unicode
/// key event for U+0003 can arrive in a terminal as Ctrl+C, U+001B as Esc and so on; text never
/// needs them. Returns the text and how many characters were dropped.
pub fn sanitize_text(text: &str) -> (String, usize) {
    let mut dropped = 0;
    let clean: String = text
        .chars()
        .filter(|&c| {
            let keep = !c.is_control() || matches!(c, '\n' | '\r' | '\t');
            if !keep {
                dropped += 1;
            }
            keep
        })
        .collect();
    (clean, dropped)
}

/// How far a typing got.
#[derive(Default, Debug, Clone)]
pub struct Progress {
    /// lines complete (with the line break before each of them)
    pub lines: usize,
    /// characters typed, line breaks included
    pub chars: usize,
    /// things the user should hear about (a clipboard that was not restored ...)
    pub notes: Vec<String>,
}

/// Why pasting line by line stopped.
pub enum Stop {
    /// the clipboard is held by another app
    Busy,
    /// the clipboard holds content a password manager marked private: it is not touched
    Sensitive,
    /// a guard refused (the user, the stop, the window changed): nothing more is typed
    Check(String),
}

/// Type lines (from line `from`) as Unicode key events; a line break goes before every line but the
/// first. `check` runs before every line and every chunk of 100 characters.
pub fn type_lines_keys(lines: &[&str], from: usize, check: &dyn Fn() -> Result<(), String>, prog: &mut Progress) -> Result<(), String> {
    for n in from..lines.len() {
        check()?;
        if n > 0 {
            send(&[key_event(0x0D, 0, 0), key_event(0x0D, 0, KEYEVENTF_KEYUP.0)]);
            prog.chars += 1;
        }
        let chars: Vec<char> = lines[n].chars().collect();
        for chunk in chars.chunks(100) {
            check()?;
            let mut events = Vec::with_capacity(chunk.len() * 2);
            for ch in chunk {
                let mut buf = [0u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    events.push(key_event(0, *unit, KEYEVENTF_UNICODE.0));
                    events.push(key_event(0, *unit, KEYEVENTF_UNICODE.0 | KEYEVENTF_KEYUP.0));
                }
            }
            send(&events);
            prog.chars += chunk.len();
            std::thread::sleep(Duration::from_millis(10));
        }
        prog.lines = n + 1;
    }
    Ok(())
}

// ---------- clipboard paste ----------
// Some apps (WinUI Notepad and friends) read keyboard state late while busy, so fast keystrokes
// turn into repeated or wrong characters. Pasting delivers the text in one step; the user's
// clipboard is saved first, kept out of clipboard history, and put back after every line.

const CF_UNICODETEXT: u32 = 13;
const NOT_MEMORY: [u32; 7] = [2, 3, 9, 14, 0x82, 0x83, 0x8E];

fn open_clipboard() -> bool {
    for _ in 0..25 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn clipboard_items() -> Vec<(u32, Vec<u8>)> {
    let mut items = Vec::new();
    let mut fmt = 0u32;
    unsafe {
        loop {
            fmt = EnumClipboardFormats(fmt);
            if fmt == 0 {
                break;
            }
            if NOT_MEMORY.contains(&fmt) || (0x200..=0x3FF).contains(&fmt) {
                continue;
            }
            let Ok(handle) = GetClipboardData(fmt) else { continue };
            let h = HGLOBAL(handle.0);
            let ptr = GlobalLock(h);
            if ptr.is_null() {
                continue;
            }
            let size = GlobalSize(h);
            items.push((fmt, std::slice::from_raw_parts(ptr as *const u8, size).to_vec()));
            let _ = GlobalUnlock(h);
        }
    }
    items
}

/// A DWORD-valued registered clipboard format (the "can include in history" flags), clipboard open.
fn clipboard_dword(fmt: u32) -> Option<u32> {
    unsafe {
        let handle = GetClipboardData(fmt).ok()?;
        let h = HGLOBAL(handle.0);
        if GlobalSize(h) < 4 {
            return None;
        }
        let ptr = GlobalLock(h);
        if ptr.is_null() {
            return None;
        }
        let v = std::ptr::read_unaligned(ptr as *const u32);
        let _ = GlobalUnlock(h);
        Some(v)
    }
}

/// True when what is on the clipboard (clipboard open) was marked private by the app that put it
/// there: password managers set these formats so that history and cloud sync skip the content.
fn clipboard_is_private() -> bool {
    let mut fmt = 0u32;
    unsafe {
        loop {
            fmt = EnumClipboardFormats(fmt);
            if fmt == 0 {
                return false;
            }
            if fmt < 0xC000 {
                continue;
            }
            let mut buf = [0u16; 128];
            let n = GetClipboardFormatNameW(fmt, &mut buf);
            if n <= 0 {
                continue;
            }
            match String::from_utf16_lossy(&buf[..n as usize]).as_str() {
                "ExcludeClipboardContentFromMonitorProcessing" | "Clipboard Viewer Ignore" => return true,
                "CanIncludeInClipboardHistory" | "CanUploadToCloudClipboard" => {
                    if clipboard_dword(fmt) == Some(0) {
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
}

fn set_clipboard(items: &[(u32, Vec<u8>)]) {
    unsafe {
        let _ = EmptyClipboard();
        for (fmt, data) in items {
            let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)) else { continue };
            let ptr = GlobalLock(h);
            if ptr.is_null() {
                let _ = GlobalFree(Some(h));
                continue;
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
            let _ = GlobalUnlock(h);
            if SetClipboardData(*fmt, Some(HANDLE(h.0))).is_err() {
                let _ = GlobalFree(Some(h));
            }
        }
    }
}

fn no_history_formats() -> Vec<(u32, Vec<u8>)> {
    unsafe {
        [w!("ExcludeClipboardContentFromMonitorProcessing"), w!("CanIncludeInClipboardHistory"), w!("CanUploadToCloudClipboard")]
            .iter()
            .map(|n| (RegisterClipboardFormatW(*n), vec![0u8; 4]))
            .collect()
    }
}

/// The user's clipboard, saved; put back after every line, and on every way out (an error, a panic).
struct SavedClipboard {
    items: Vec<(u32, Vec<u8>)>,
    /// sequence number right after we wrote our line (None: nothing of ours on the clipboard)
    ours: Option<u32>,
    /// the last paste was not seen to take effect (the field can't be read, or did not change in time): the target may still
    /// be about to read the clipboard, so the user's content must not go back before the target is idle
    unconfirmed: bool,
    /// what is left of the wait for a busy target
    idle_left: Duration,
    /// the wait was ended by a stop or a cancellation: no further waiting
    aborted: bool,
}

impl SavedClipboard {
    /// Wait (bounded, shared by the whole typing) until the target is idle; a stop or cancellation ends the wait at once.
    fn wait_idle(&mut self) -> Idle {
        if self.aborted {
            return Idle::Aborted(String::new());
        }
        let t = Instant::now();
        let idle = wait_target_idle(self.idle_left);
        self.idle_left = self.idle_left.saturating_sub(t.elapsed());
        if matches!(idle, Idle::Aborted(_)) {
            self.aborted = true;
        }
        idle
    }

    /// Put the user's clipboard back, unless another app changed it since our write.
    fn restore(&mut self) -> Option<String> {
        let seq = self.ours.take()?;
        let mut busy_note = None;
        if std::mem::take(&mut self.unconfirmed) {
            match self.wait_idle() {
                Idle::Idle => {}
                Idle::TimedOut => {
                    busy_note = Some("the target was still busy after the wait, so your clipboard was put back while it may still have been about to paste; check what it shows".to_string());
                }
                Idle::Aborted(_) => {
                    busy_note = Some("the wait for the target to finish was ended by a stop or a cancellation, so your clipboard was put back while the target may still have been about to paste; check what it shows".to_string());
                }
            }
        }
        if unsafe { GetClipboardSequenceNumber() } != seq {
            return Some("the clipboard was changed by another app while typing, so your previous clipboard content was not restored".into());
        }
        if open_clipboard() {
            set_clipboard(&self.items);
            unsafe { let _ = CloseClipboard(); }
            busy_note
        } else {
            Some("the clipboard was busy at the end, so your previous clipboard content was not restored".into())
        }
    }
}

impl Drop for SavedClipboard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Type lines (from line `from`) by pasting them one at a time (a line break goes before every line but
/// the first). `before` reads the field before each paste and `settle(before)` waits until it shows the
/// text. `check` runs before every line and again right before its keys. The clipboard is the user's
/// again after every line.
pub fn paste_lines(
    lines: &[&str],
    from: usize,
    before: &dyn Fn() -> Option<String>,
    settle: &dyn Fn(Option<String>, f64) -> bool,
    check: &dyn Fn() -> Result<(), String>,
    prog: &mut Progress,
) -> Result<(), Stop> {
    if from >= lines.len() {
        return Ok(());
    }
    if !open_clipboard() {
        return Err(Stop::Busy);
    }
    if clipboard_is_private() {
        unsafe { let _ = CloseClipboard(); }
        return Err(Stop::Sensitive);
    }
    let items = clipboard_items();
    unsafe { let _ = CloseClipboard(); }
    let mut saved = SavedClipboard { items, ours: None, unconfirmed: false, idle_left: IDLE_BUDGET, aborted: false };
    for n in from..lines.len() {
        check().map_err(Stop::Check)?;
        let line = lines[n];
        if !line.is_empty() {
            if !open_clipboard() {
                return Err(Stop::Busy);
            }
            let mut bytes: Vec<u8> = line.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            bytes.extend_from_slice(&[0, 0]);
            let mut items = vec![(CF_UNICODETEXT, bytes)];
            items.extend(no_history_formats());
            set_clipboard(&items);
            unsafe { let _ = CloseClipboard(); }
            saved.ours = Some(unsafe { GetClipboardSequenceNumber() });
            wait_target_responsive(250);
        }
        check().map_err(Stop::Check)?;
        if n > 0 {
            // wait until the line break has shown up: it must not be taken for the effect of the paste below
            let pre = before();
            send(&[key_event(0x0D, 0, 0), key_event(0x0D, 0, KEYEVENTF_KEYUP.0)]);
            prog.chars += 1;
            if pre.is_some() {
                settle(pre, 0.25);
            }
        }
        if !line.is_empty() {
            let was = before();
            // Ctrl+V as a chord, released again at once (holding Ctrl while waiting for the text left
            // it down for as long as the wait lasted)
            // (the last look is inside the chord, right before the keys go down)
            if let Err(e) = chord(&[0x11], &[0x56], check) {
                if n > 0 {
                    prog.notes.push("a line break was typed after the last complete line, before the stop".into());
                }
                return Err(Stop::Check(e));
            }
            // The clipboard goes back to the user only once the target has taken the paste. Seeing the field change is the
            // evidence. Without it (the field can't be read, or did not change in time) the target may still be busy and about
            // to read the clipboard: wait until it is idle (bounded) before anything else happens to the clipboard, and give it
            // back once, at the end.
            if settle(was, 1.0) {
                saved.unconfirmed = false;
                prog.chars += line.chars().count();
                if let Some(note) = saved.restore() {
                    if !prog.notes.contains(&note) {
                        prog.notes.push(note);
                    }
                }
            } else {
                saved.unconfirmed = true;
                let waited = saved.wait_idle();
                prog.chars += line.chars().count();
                if let Idle::Aborted(why) = waited {
                    // the stop or the cancellation ends the wait (and the typing) at once: the clipboard goes back, the result says so
                    prog.lines = n + 1;
                    if let Some(note) = saved.restore() {
                        if !prog.notes.contains(&note) {
                            prog.notes.push(note);
                        }
                    }
                    return Err(Stop::Check(why));
                }
            }
        }
        prog.lines = n + 1;
    }
    // unreadable fields: the one restore, after the last paste and the target being idle
    if let Some(note) = saved.restore() {
        if !prog.notes.contains(&note) {
            prog.notes.push(note);
        }
    }
    Ok(())
}
