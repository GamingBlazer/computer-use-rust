//! Windows helpers: monitors, windows, processes, the current "view" (what the latest
//! screenshot showed, so click coordinates can be mapped back to the screen).

use std::ffi::c_void;
use crate::sync::Mutex;

use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_F24};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
    SetForegroundWindow, SetWindowPos, ShowWindow, WindowFromPoint, GA_ROOT, GA_ROOTOWNER, GWL_EXSTYLE, HWND_NOTOPMOST,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_RESTORE, WS_EX_TOOLWINDOW,
};
use windows::core::PWSTR;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.left + self.width && y >= self.top && y < self.top + self.height
    }
    pub fn right(&self) -> i32 {
        self.left + self.width - 1
    }
    pub fn bottom(&self) -> i32 {
        self.top + self.height - 1
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Monitor {
    pub rect: Rect,
    pub handle: isize,
}

/// Monitors: the primary first, then left to right.
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(hmon: HMONITOR, _hdc: HDC, _r: *mut RECT, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<Monitor>);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(hmon, &mut info).as_bool() {
            let r = info.rcMonitor;
            out.push(Monitor {
                rect: Rect { left: r.left, top: r.top, width: r.right - r.left, height: r.bottom - r.top },
                handle: hmon.0 as isize,
            });
        }
        BOOL(1)
    }
    let mut out: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out.sort_by_key(|m| (!m.rect.contains(0, 0), m.rect.left, m.rect.top));
    out
}

/// Index of the monitor containing the point, else the nearest one.
pub fn monitor_at(x: i32, y: i32, mons: &[Monitor]) -> usize {
    let dist = |r: &Rect| {
        let dx = (r.left - x).max(0).max(x - r.right());
        let dy = (r.top - y).max(0).max(y - r.bottom());
        (dx as i64) * (dx as i64) + (dy as i64) * (dy as i64)
    };
    (0..mons.len()).min_by_key(|&i| dist(&mons[i].rect)).unwrap_or(0)
}

pub fn monitor_handle_at(x: i32, y: i32) -> HMONITOR {
    unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) }
}

pub fn title(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// What is written in place of the title of a window that must not be read (security and password-manager apps, the lock
/// screen, a program that can't be identified): the title can name an account, a vault or a document.
pub const HIDDEN_TITLE: &str = "(hidden: protected window)";

/// The title of a window as it may be shown to the AI: `HIDDEN_TITLE` for a window of a forbidden, locked or unidentified
/// program. Every window title that goes into a tool result or an error message comes through here.
pub fn shown_title(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    if crate::guards::hides_title(&app_exe(hwnd)) {
        return HIDDEN_TITLE.to_string();
    }
    title(hwnd)
}

pub fn active_title() -> String {
    let fg = unsafe { GetForegroundWindow() };
    let t = shown_title(fg);
    if t.is_empty() { "(no title)".into() } else { t }
}

pub fn foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}

pub fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// Lower-case executable file name of a process, or "" when it can't be read (callers that apply a
/// policy treat "" as unknown and refuse).
pub fn exe_name(pid: u32) -> String {
    // debug builds only: a test makes one process "unidentifiable" (as an elevated or protected program is)
    #[cfg(debug_assertions)]
    if std::env::var("DC_TEST_UNKNOWN_EXE_PID").ok().and_then(|v| v.parse::<u32>().ok()) == Some(pid) {
        return String::new();
    }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut out = String::new();
        // 1024 characters first; a longer path (up to 32767) needs the bigger buffer
        for cap in [1024usize, 32768] {
            let mut buf = vec![0u16; cap];
            let mut size = buf.len() as u32;
            if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size).is_ok() {
                let full = String::from_utf16_lossy(&buf[..size as usize]);
                out = full.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
                break;
            }
        }
        let _ = CloseHandle(h);
        out
    }
}

/// The program a window really belongs to. A Store (UWP) app's top-level window belongs to
/// ApplicationFrameHost.exe; the app itself lives in the `Windows.UI.Core.CoreWindow` child, whose
/// process is the one that counts for every policy decision. When that child can't be found the
/// answer stays "applicationframehost.exe", which the policy treats as unknown.
pub fn app_exe(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    let pid = pid_of(hwnd);
    let exe = exe_name(pid);
    if exe == FRAME_HOST || class_name(hwnd) == "ApplicationFrameWindow" {
        if let Some(core_pid) = core_window_pid(hwnd, pid) {
            let e = exe_name(core_pid);
            if !e.is_empty() {
                return e;
            }
        }
        return if exe.is_empty() { FRAME_HOST.to_string() } else { exe };
    }
    exe
}

pub const FRAME_HOST: &str = "applicationframehost.exe";

/// Process id of the CoreWindow inside a Store-app frame window (a pid other than the frame host's).
fn core_window_pid(frame: HWND, frame_pid: u32) -> Option<u32> {
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let st = &mut *(lp.0 as *mut (u32, Option<u32>));
        if class_name(h) == "Windows.UI.Core.CoreWindow" {
            let p = pid_of(h);
            if p != st.0 && p != 0 {
                st.1 = Some(p);
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut st: (u32, Option<u32>) = (frame_pid, None);
    unsafe {
        let _ = EnumChildWindows(Some(frame), Some(cb), LPARAM(&mut st as *mut _ as isize));
    }
    st.1
}

pub fn window_rect(hwnd: HWND) -> Rect {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    Rect { left: r.left, top: r.top, width: r.right - r.left, height: r.bottom - r.top }
}

/// Visible bounds without the invisible resize borders.
pub fn frame_rect(hwnd: HWND) -> Rect {
    let mut r = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut c_void, std::mem::size_of::<RECT>() as u32)
    }
    .is_ok();
    if !ok {
        return window_rect(hwnd);
    }
    Rect { left: r.left, top: r.top, width: r.right - r.left, height: r.bottom - r.top }
}

pub fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe {
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut c_void, 4);
    }
    cloaked != 0
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// All top-level windows, front to back.
pub fn windows_z() -> Vec<HWND> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        (*(lp.0 as *mut Vec<HWND>)).push(hwnd);
        BOOL(1)
    }
    let mut out: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// Processes above this server, nearest first: (pid, lower-case exe name). Stops at explorer.exe and
/// the system's own launchers (they own unrelated windows), at a parent that has gone, and at a
/// "parent" id that was recycled for a younger process.
pub fn ancestor_chain() -> Vec<(u32, String)> {
    let mut procs: std::collections::HashMap<u32, (u32, String)> = Default::default();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return vec![] };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
            procs.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    const STOP: &[&str] = &["explorer.exe", "services.exe", "wininit.exe", "winlogon.exe", "userinit.exe", "sihost.exe", "svchost.exe", "csrss.exe", "smss.exe", "system"];
    let mut out: Vec<(u32, String)> = Vec::new();
    let mut pid = unsafe { GetCurrentProcessId() };
    let mut child_started = process_start(pid);
    while let Some((parent, _)) = procs.get(&pid) {
        if out.len() >= 20 {
            break;
        }
        let parent = *parent;
        let Some((_, name)) = procs.get(&parent) else { break };
        if STOP.contains(&name.as_str()) || out.iter().any(|(p, _)| *p == parent) {
            break;
        }
        let started = process_start(parent);
        if let (Some(c), Some(p)) = (child_started, started) {
            if p > c {
                break; // the id was reused by a process younger than its "child"
            }
        }
        out.push((parent, name.clone()));
        child_started = started;
        pid = parent;
    }
    out
}

pub fn process_start(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
    }
}

/// Is this window part of the AI app the server runs under (see `home`)? Never read or driven as
/// an ordinary work window.
pub fn is_host(hwnd: HWND) -> bool {
    crate::home::is_host(hwnd)
}

/// Top-level window that owns `hwnd` (itself when it has no owner).
pub fn root_owner(hwnd: HWND) -> HWND {
    let r = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
    if r.0.is_null() { hwnd } else { r }
}

/// A visible app window the AI could be working in: not the AI app's own window, not the
/// desktop or taskbar, not a tool window.
pub fn is_work_window(hwnd: HWND) -> bool {
    unsafe {
        if hwnd.0.is_null() || !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() || pid_of(hwnd) == GetCurrentProcessId() {
            return false; // (our own windows first: their title query would be a message to our overlay thread)
        }
        if GetWindowTextLengthW(hwnd) == 0 || (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        let pid = pid_of(hwnd);
        if pid == GetCurrentProcessId() || is_host(hwnd) || is_cloaked(hwnd) {
            return false;
        }
    }
    !matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

/// The window the AI is working in: the foreground one, or, while the AI app is in front, the
/// most recently used other window.
pub fn work_window() -> Option<HWND> {
    let fg = foreground();
    if is_work_window(fg) {
        return Some(fg);
    }
    windows_z().into_iter().find(|&h| is_work_window(h))
}

/// Visible top-level windows with a title, front to back, including owned dialogs.
pub fn titled_windows() -> Vec<(HWND, String)> {
    let me = unsafe { GetCurrentProcessId() };
    windows_z()
        .into_iter()
        .filter(|&h| unsafe { IsWindowVisible(h).as_bool() } && pid_of(h) != me && !is_cloaked(h))
        .map(|h| (h, title(h)))
        .filter(|(_, t)| !t.is_empty())
        .collect()
}

/// Unicode format characters (category Cf: zero-width space and joiners, bidi marks, BOM, soft
/// hyphen, ...): invisible, and browsers put some in their window titles ("Microsoft\u{200B} Edge").
fn is_format_char(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x08E2 | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E
        | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// Window title or name as compared by name: format characters removed, whitespace runs
/// collapsed to one space, trimmed, lower-case.
pub fn normalize_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars().filter(|&c| !is_format_char(c)) {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// "msedge" for "C:\...\MSEdge.exe" (the lower-case exe name without ".exe"), "" when unknown.
pub fn exe_stem(pid: u32) -> String {
    let e = exe_name(pid);
    e.strip_suffix(".exe").unwrap_or(&e).to_string()
}

/// Names people use for an app besides its exe name.
fn exe_aliases(stem: &str) -> &'static [&'static str] {
    match stem {
        "msedge" => &["edge", "microsoft edge"],
        "chrome" => &["google chrome"],
        "firefox" => &["mozilla firefox"],
        "code" | "code - insiders" => &["vs code", "vscode", "visual studio code"],
        "windowsterminal" => &["terminal", "windows terminal", "wt"],
        "explorer" => &["file explorer", "windows explorer"],
        "winword" => &["word", "microsoft word"],
        "excel" => &["microsoft excel"],
        "powerpnt" => &["powerpoint", "microsoft powerpoint"],
        "wezterm-gui" => &["wezterm"],
        _ => &[],
    }
}

/// The front-most window chosen by `name`: "#<id>" names one window exactly. Otherwise titles and app (exe) names are compared after normalising them
/// (invisible format characters removed, whitespace collapsed, case ignored); the order is: the
/// whole title equals the name, the app's exe name (or a known alias such as "edge" for
/// msedge.exe) equals it, the title contains it, the exe name contains it (3+ letters).
///
/// Windows of forbidden or locked programs and of programs that can't be identified are not candidates at all (`hides_title`):
/// the name is compared with the titles of the other windows only, so neither a refusal nor a wait can confirm what a
/// password manager, the lock screen or an unreadable program shows. A name that only such a window matches is answered
/// exactly like a name nothing matches.
pub fn match_window(name: &str) -> Option<(HWND, String)> {
    let hit = match_window_raw(name)?;
    let shown = shown_title(hit.0);
    Some((hit.0, shown))
}

fn match_window_raw(name: &str) -> Option<(HWND, String)> {
    if let Some(id) = name.strip_prefix('#').and_then(|s| s.parse::<isize>().ok()) {
        let h = HWND(id as *mut c_void);
        let ok = unsafe { IsWindow(Some(h)).as_bool() && IsWindowVisible(h).as_bool() };
        return (ok && !crate::guards::hides_title(&app_exe(h))).then(|| (h, title(h)));
    }
    let want = normalize_title(name);
    if want.is_empty() {
        return None;
    }
    let want_exe = want.strip_suffix(".exe").unwrap_or(&want).to_string();
    let mut wins = titled_windows();
    wins.retain(|(h, _)| !crate::guards::hides_title(&app_exe(*h)));
    let norm: Vec<String> = wins.iter().map(|(_, t)| normalize_title(t)).collect();
    if let Some(i) = norm.iter().position(|t| *t == want) {
        return Some(wins[i].clone());
    }
    let stems: Vec<String> = wins.iter().map(|(h, _)| exe_stem(pid_of(*h))).collect();
    if let Some(i) = stems.iter().position(|s| !s.is_empty() && (*s == want_exe || exe_aliases(s).contains(&want.as_str()))) {
        return Some(wins[i].clone());
    }
    if let Some(i) = norm.iter().position(|t| t.contains(&want)) {
        return Some(wins[i].clone());
    }
    if want_exe.chars().count() >= 3 {
        if let Some(i) = stems.iter().position(|s| s.contains(&want_exe)) {
            return Some(wins[i].clone());
        }
    }
    None
}

/// Like match_window, retrying briefly: a window that is just opening can be missed for a moment.
pub fn find_window(name: &str, timeout: f64) -> Option<(HWND, String)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
    loop {
        if let Some(hit) = match_window(name) {
            return Some(hit);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Windows only lets the process that received the last input event change the foreground window, so
/// a lone key tap lifts that lock before SetForegroundWindow. It must be a key nothing reacts to:
/// v0.3.0 tapped Alt, which put the newly focused window into menu / access-key mode (Notepad's
/// "File" menu was left focused) and made the next shortcut in a script, "ctrl+a" or "alt+F4",
/// arrive as a plain letter or do nothing. F24 does not exist on keyboards and no app binds it.
fn tap_unused_key() {
    let mut ev = [INPUT::default(), INPUT::default()];
    for (i, e) in ev.iter_mut().enumerate() {
        e.r#type = INPUT_KEYBOARD;
        e.Anonymous = INPUT_0 {
            ki: KEYBDINPUT { wVk: VK_F24, dwFlags: if i == 1 { KEYEVENTF_KEYUP } else { Default::default() }, ..Default::default() },
        };
    }
    unsafe { SendInput(&ev, std::mem::size_of::<INPUT>() as i32) };
}

pub fn bring_to_front(hwnd: HWND) -> bool {
    if !is_host(hwnd) {
        crate::session::note_target(hwnd);
    }
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        for _ in 0..3 {
            if GetForegroundWindow() == hwnd {
                return true;
            }
            let t0 = std::time::Instant::now();
            let tr = |w: &str| {
                if std::env::var_os("DC_TRACE").is_some() {
                    eprintln!("bring_to_front: {w} at {} ms", t0.elapsed().as_millis());
                }
            };
            tap_unused_key();
            tr("key tapped");
            let _ = SetForegroundWindow(hwnd);
            tr("SetForegroundWindow");
            let keep = SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE;
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, keep);
            tr("topmost");
            let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, keep);
            tr("notopmost");
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
            while GetForegroundWindow() != hwnd && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        GetForegroundWindow() == hwnd
    }
}

pub enum Switch {
    /// the normal way worked
    Direct,
    /// Windows' foreground lock refused it; attaching to the foreground thread's input queue worked
    Attached,
    Failed,
}

/// bring_to_front, and when Windows refuses (foreground lock) one retry with AttachThreadInput
/// to the current foreground thread, detached afterwards.
pub fn bring_to_front_robust(hwnd: HWND) -> Switch {
    let t0 = std::time::Instant::now();
    let trace = |what: &str| {
        if std::env::var_os("DC_TRACE").is_some() {
            eprintln!("bring_to_front_robust: {what} at {} ms", t0.elapsed().as_millis());
        }
    };
    if bring_to_front(hwnd) {
        trace("direct ok");
        return Switch::Direct;
    }
    trace("direct failed, attaching");
    unsafe {
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let me = windows::Win32::System::Threading::GetCurrentThreadId();
        let attached = fg_tid != 0 && fg_tid != me && windows::Win32::System::Threading::AttachThreadInput(me, fg_tid, true).as_bool();
        let _ = SetForegroundWindow(hwnd);
        let _ = windows::Win32::UI::WindowsAndMessaging::BringWindowToTop(hwnd);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        while GetForegroundWindow() != hwnd && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        trace("waited after attach");
        if attached {
            let _ = windows::Win32::System::Threading::AttachThreadInput(me, fg_tid, false);
        }
        if GetForegroundWindow() == hwnd { Switch::Attached } else { Switch::Failed }
    }
}

/// Flash a window's taskbar button until the user brings it to the front.
pub fn flash(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{FlashWindowEx, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY};
    let info = FLASHWINFO { cbSize: std::mem::size_of::<FLASHWINFO>() as u32, hwnd, dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG, uCount: 0, dwTimeout: 0 };
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

pub fn root_window_at(x: i32, y: i32) -> HWND {
    unsafe { GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOT) }
}

/// The deepest window (a child control, not its top-level window) at a screen point.
pub fn window_at(x: i32, y: i32) -> HWND {
    unsafe { WindowFromPoint(POINT { x, y }) }
}

pub fn is_window(hwnd: HWND) -> bool {
    unsafe { IsWindow(Some(hwnd)).as_bool() && IsWindowVisible(hwnd).as_bool() }
}

pub fn handle_is(h: HANDLE) -> bool {
    !h.is_invalid()
}

// ---------- the view: what the latest screenshot showed ----------

#[derive(Clone, Copy, Debug)]
pub struct View {
    pub index: usize,       // monitor index of the view
    pub rect: Rect,         // screen rect the screenshot covers
    pub scale: f64,         // screen px per screenshot px
    pub hwnd: Option<isize>, // window-scoped view
    pub frame: Option<Rect>, // the window's frame when captured (bounds-changed guard)
    pub pinned: Option<usize>, // monitor chosen with screenshot(monitor=N)
    pub pin_hwnd: Option<isize>,
}

pub static VIEW: Mutex<View> = Mutex::new(View {
    index: 0,
    rect: Rect { left: 0, top: 0, width: 1, height: 1 },
    scale: 1.0,
    hwnd: None,
    frame: None,
    pinned: None,
    pin_hwnd: None,
});

pub const MAX_WIDTH: f64 = 1366.0;
pub const SMALL_WIDTH: f64 = 1024.0;

/// Image size mode of the latest screenshot (`size: "small"` = 1024 px wide instead of 1366).
/// Coordinates always refer to the latest screenshot, so every conversion uses this mode.
pub static SMALL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn max_width() -> f64 {
    if SMALL.load(std::sync::atomic::Ordering::Relaxed) { SMALL_WIDTH } else { MAX_WIDTH }
}

pub fn scale_for(width: i32) -> f64 {
    (width as f64 / max_width()).max(1.0)
}

pub fn set_monitor_view(index: usize, m: &Monitor) {
    let mut v = VIEW.lock().unwrap();
    v.index = index;
    v.rect = m.rect;
    v.scale = scale_for(m.rect.width);
    v.hwnd = None;
    v.frame = None;
}

/// Monitor to work on: the one chosen with screenshot(monitor=N) until the work window
/// changes, otherwise the work window's monitor.
pub fn follow(mons: &[Monitor]) -> usize {
    let hwnd = work_window();
    let mut v = VIEW.lock().unwrap();
    if let (Some(p), Some(ph)) = (v.pinned, v.pin_hwnd) {
        if hwnd.map(|h| h.0 as isize) == Some(ph) {
            return p.min(mons.len().saturating_sub(1));
        }
    }
    v.pinned = None;
    match hwnd {
        Some(h) => {
            let r = window_rect(h);
            monitor_at(r.left + r.width / 2, r.top + r.height / 2, mons)
        }
        None => 0,
    }
}

/// Screenshot coordinates -> screen pixels, on the latest screenshot (or monitor N's own
/// screenshot space), kept on a monitor and clear of the fail-safe corners.
pub fn to_screen(x: f64, y: f64, monitor: Option<usize>) -> (i32, i32) {
    let mons = monitors();
    let (rect, scale) = match monitor {
        None => {
            let v = VIEW.lock().unwrap();
            (v.rect, v.scale)
        }
        Some(n) => {
            let m = &mons[n.min(mons.len() - 1)];
            (m.rect, scale_for(m.rect.width))
        }
    };
    let sx = (rect.left + (x * scale).round() as i32).clamp(rect.left, rect.right());
    let sy = (rect.top + (y * scale).round() as i32).clamp(rect.top, rect.bottom());
    let m = &mons[monitor_at(sx, sy, &mons)].rect;
    (sx.clamp(m.left + 1, m.right() - 1), sy.clamp(m.top + 1, m.bottom() - 1))
}

pub fn hwnd_from(id: isize) -> HWND {
    HWND(id as *mut c_void)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_normalised() {
        assert_eq!(normalize_title("Microsoft\u{200B} Edge"), "microsoft edge");
        assert_eq!(normalize_title("  Micro\u{200B}soft \u{00A0}  Edge\t- Work "), "microsoft edge - work");
        assert_eq!(normalize_title("a\u{FEFF}b\u{200D}c\u{2060}d\u{00AD}e"), "abcde");
        assert_eq!(normalize_title("\u{200B}\u{200B}"), "");
        assert_eq!(normalize_title("Notepad"), normalize_title("NOTEPAD"));
        assert_eq!(normalize_title("Straße"), "straße");
    }

    #[test]
    fn exe_aliases_cover_common_names() {
        assert!(exe_aliases("msedge").contains(&"edge"));
        assert!(exe_aliases("msedge").contains(&"microsoft edge"));
        assert!(exe_aliases("code").contains(&"vs code"));
        assert!(exe_aliases("notepad").is_empty());
    }
}
