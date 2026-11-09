//! The home window: the window of the AI app this server was started from, where the user is
//! taken back when a task ends, and which is never read or driven as an ordinary work window.
//!
//! It is found in layers, whichever AI app and terminal the user has:
//!   0. `homeApp` in config.json (an exe name) overrides everything.
//!   1. The parent chain: the first ancestor process with a visible top-level window. GUI clients
//!      that spawn the server (VS Code, Cursor, Claude Desktop, the Codex or ChatGPT apps) and
//!      third-party terminals (WezTerm, Alacritty) are found here. Helper processes (node,
//!      Electron utilities) own no such window, so they are skipped by themselves.
//!   2. The console owner, for CLI clients (Claude Code, Codex CLI, Gemini CLI, opencode) in
//!      Windows Terminal, conhost or any other terminal: attach to the console of the nearest
//!      ancestor that has one, take its console window and walk to the root owner, which is the
//!      real terminal window (Windows Terminal's default-terminal hand-off parents the hidden
//!      pseudo-console window to the exact terminal window of the tab). AttachConsole replaces the
//!      console of the process that calls it, so it runs in a short-lived helper process
//!      (`--console-owner`, no console of its own) and never touches our stdin/stdout.
//!   3. The foreground window when the first action of a task starts (see session::touch).
//! The exact window is chosen, not just the app: with several Windows Terminal windows only the
//! one whose console runs the client is the host.

use crate::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible, GA_ROOTOWNER, GWL_EXSTYLE,
    GW_OWNER, WS_EX_TOOLWINDOW,
};

use crate::win;

#[derive(Default)]
struct State {
    /// processes whose windows are the AI app's (the parent chain; a terminal shared by several
    /// windows is left out when its exact window is known)
    pids: Vec<(u32, Option<u64>)>,
    /// the pids above that are still the same processes (checked at most every 2 s), and when
    live: Vec<u32>,
    live_at: Option<Instant>,
    /// exact windows (console owner), as root windows
    exact: Vec<isize>,
    /// homeApp from config.json, lower-case exe name with ".exe"
    home_app: Option<String>,
    source: &'static str,
    /// layer 3: the foreground window when the task started (a guess: returned to, not hidden)
    fallback: Option<isize>,
    resolved_at: Option<Instant>,
    exe_cache: Vec<(u32, String)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    pids: Vec::new(),
    live: Vec::new(),
    live_at: None,
    exact: Vec::new(),
    home_app: None,
    source: "",
    fallback: None,
    resolved_at: None,
    exe_cache: Vec::new(),
});

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap()
}

/// A normal app window: visible, titled, no owner, not a tool window, not the shell.
fn is_candidate(h: HWND) -> bool {
    unsafe {
        IsWindowVisible(h).as_bool()
            && GetWindowTextLengthW(h) > 0
            && GetWindow(h, GW_OWNER).map(|o| o.0.is_null()).unwrap_or(true)
            && (GetWindowLongPtrW(h, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW.0 == 0
            && !win::is_cloaked(h)
            && !matches!(win::class_name(h).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
    }
}

fn windows_of(pid: u32) -> Vec<HWND> {
    win::windows_z().into_iter().filter(|&h| win::pid_of(h) == pid && is_candidate(h)).collect()
}

fn configured_app() -> Option<String> {
    let raw = crate::config().get("homeApp").and_then(|v| v.as_str())?.trim().to_lowercase();
    if raw.is_empty() {
        return None;
    }
    Some(if raw.ends_with(".exe") { raw } else { format!("{raw}.exe") })
}

/// Layer 2 in this process, without attaching to any console: the hidden pseudo-console window
/// (Windows Terminal, ConPTY) or the console window (conhost) is reported by Windows as belonging
/// to the process that started the console, which is almost always an ancestor.
fn console_window_by_owner(chain: &[(u32, String)]) -> Option<HWND> {
    let all = win::windows_z();
    for (pid, _) in chain {
        for &h in &all {
            if win::pid_of(h) != *pid || !matches!(win::class_name(h).as_str(), "PseudoConsoleWindow" | "ConsoleWindowClass") {
                continue;
            }
            if let Some(root) = visible_root(h) {
                return Some(root);
            }
        }
    }
    None
}

/// The visible root-owner window of a console window, when it is a real terminal window.
fn visible_root(console: HWND) -> Option<HWND> {
    let root = unsafe { GetAncestor(console, GA_ROOTOWNER) };
    let w = if root.0.is_null() { console } else { root };
    (unsafe { IsWindowVisible(w).as_bool() } && w != console || win::class_name(console) == "ConsoleWindowClass" && unsafe { IsWindowVisible(console).as_bool() }).then_some(w)
}

/// Layer 2 through a helper process: `desktop-control.exe --console-owner pid,pid,...` attaches to
/// the console of the first pid that has one and prints the terminal window's handle.
fn console_window_by_attach(chain: &[(u32, String)]) -> Option<HWND> {
    if chain.is_empty() {
        return None;
    }
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let pids = chain.iter().map(|(p, _)| p.to_string()).collect::<Vec<_>>().join(",");
    let mut child = Command::new(std::env::current_exe().ok()?)
        .arg("--console-owner")
        .arg(pids)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x0000_0008) // DETACHED_PROCESS: no console, so AttachConsole can attach
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let text = rx.recv_timeout(Duration::from_millis(3000)).unwrap_or_default();
    let _ = child.kill();
    let _ = child.wait();
    let id = text.trim().parse::<isize>().ok()?;
    let h = win::hwnd_from(id);
    (win::is_window(h) && win::pid_of(h) != std::process::id()).then_some(h)
}

/// Body of `--console-owner`: print the handle of the terminal window of the first listed process
/// that has a console, or "none". Runs in a process without a console of its own.
pub fn console_owner_main(pids: &str) {
    use windows::Win32::System::Console::{AttachConsole, FreeConsole, GetConsoleWindow};
    for pid in pids.split(',').filter_map(|p| p.trim().parse::<u32>().ok()) {
        unsafe {
            let _ = FreeConsole();
            if AttachConsole(pid).is_err() {
                continue;
            }
            let console = GetConsoleWindow();
            let _ = FreeConsole();
            if console.0.is_null() {
                continue;
            }
            if let Some(w) = visible_root(console) {
                println!("{}", w.0 as isize);
                return;
            }
        }
    }
    println!("none");
}

/// Work out the home window from scratch (at start-up, and again when it has gone).
pub fn resolve() {
    let chain = win::ancestor_chain();
    // debug builds only: pretend no AI app could be found (tests of the fallback and of the warning)
    #[cfg(debug_assertions)]
    let chain = if std::env::var_os("DC_TEST_NO_HOST").is_some() { Vec::new() } else { chain };
    let home_app = configured_app();
    // layer 1: the first ancestor with a normal window
    let layer1 = chain.iter().find(|(pid, _)| !windows_of(*pid).is_empty()).cloned();
    let l1_windows = layer1.as_ref().map(|(pid, _)| windows_of(*pid)).unwrap_or_default();
    // layer 2: the console owner; needed when layer 1 found nothing, or the process has several windows
    let mut exact = None;
    if layer1.is_none() || l1_windows.len() > 1 || home_app.is_some() {
        exact = console_window_by_attach(&chain).or_else(|| console_window_by_owner(&chain));
    }
    let mut st = lock();
    st.home_app = home_app.clone();
    st.exact.clear();
    st.pids = chain.iter().map(|(p, _)| (*p, win::process_start(*p))).collect();
    st.live_at = None;
    st.source = "none";
    if let Some(h) = exact {
        let root = win::root_owner(h);
        st.exact.push(root.0 as isize);
        st.source = "console owner";
        // a terminal process shared by all its windows (Windows Terminal) hosts only the exact window
        let rp = win::pid_of(root);
        st.pids.retain(|(p, _)| *p != rp);
    }
    if let Some((pid, _)) = &layer1 {
        if st.exact.is_empty() {
            st.source = "parent chain";
        } else if !st.pids.iter().any(|(p, _)| p == pid) && win::pid_of(win::hwnd_from(st.exact[0])) != *pid {
            st.pids.push((*pid, win::process_start(*pid))); // both: the exact terminal window and the GUI process above it
        }
    }
    if let Some(app) = &home_app {
        // the override: that app's windows, plus the exact window when it belongs to that app
        st.pids.clear();
        let keep: Vec<isize> = st.exact.iter().copied().filter(|&h| win::exe_name(win::pid_of(win::hwnd_from(h))) == *app).collect();
        st.exact = keep;
        st.source = "config.json homeApp";
    }
    st.exe_cache.clear();
    st.resolved_at = Some(Instant::now());
    eprintln!(
        "home window: source {}, pids {:?}, exact {:?}, homeApp {:?}",
        st.source,
        st.pids.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        st.exact,
        st.home_app
    );
}

pub fn init() {
    resolve();
}

/// The chain pids that still belong to the same processes as when the chain was resolved (an exited
/// process, or a pid reused by a younger one, no longer makes its windows the AI app's). Re-checked at
/// most every 2 s.
fn live_pids(st: &mut State) -> &[u32] {
    if st.live_at.map(|t| t.elapsed() > Duration::from_secs(2)).unwrap_or(true) {
        st.live = st.pids
            .iter()
            .filter(|(p, started)| match (win::process_start(*p), started) {
                (Some(now), Some(then)) => now == *then,
                (Some(_), None) => true,
                (None, None) => true, // never readable: can't be verified, so it stays protected
                (None, Some(_)) => false, // it has exited
            })
            .map(|(p, _)| *p)
            .collect();
        st.live_at = Some(Instant::now());
    }
    &st.live
}

/// True when no AI app window could be identified (layers 0 to 2 found nothing): only the window in
/// front when the task started is protected, as a guess.
pub fn unresolved() -> bool {
    let st = lock();
    // layers 0 to 2 found nothing: the ancestors are known but none of them has a window of its own
    st.exact.is_empty() && st.home_app.is_none() && (st.source == "none" || st.source.is_empty())
}

/// The warning that goes into the first result of a task when no AI app window was identified.
pub fn unresolved_warning() -> Option<String> {
    if !unresolved() {
        return None;
    }
    let fb = fallback();
    Some(format!(
        "Warning: the AI app's own window could not be identified. {} is treated as the AI app's own and is never driven; if that is wrong, ask the user to set homeApp in config.json.",
        match fb {
            Some(h) => format!("The window that was in front when this task started ({:?})", win::shown_title(h).chars().take(50).collect::<String>()),
            None => "Only the window in front when a task starts".to_string(),
        }
    ))
}

fn exe_of_cached(st: &mut State, pid: u32) -> String {
    if let Some((_, e)) = st.exe_cache.iter().find(|(p, _)| *p == pid) {
        return e.clone();
    }
    let e = win::exe_name(pid);
    if st.exe_cache.len() > 256 {
        st.exe_cache.clear();
    }
    st.exe_cache.push((pid, e.clone()));
    e
}

/// Is this window the AI app's? Its root owner is one of the exact windows, or it belongs to a
/// process of the parent chain, or to the `homeApp` exe.
pub fn is_host(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let root = win::root_owner(hwnd).0 as isize;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let mut st = lock();
    // the layer-3 guess counts too: when the AI app could not be identified, the window in front when the task
    // started is protected as if it were the AI app (failing open would let the agent type into the user chat)
    if st.exact.contains(&root) || st.fallback == Some(root) || live_pids(&mut st).contains(&pid) {
        return true;
    }
    match st.home_app.clone() {
        Some(app) => exe_of_cached(&mut st, pid) == app,
        None => false,
    }
}

/// The window to return to: an exact window that is still there, else the topmost normal window of
/// a host process, else None. `fg` (the foreground window at the task start) wins when it is a host window.
pub fn resolved_window(fg: Option<HWND>) -> Option<HWND> {
    if let Some(f) = fg {
        if !f.0.is_null() && is_host(f) && win::is_window(f) {
            return Some(win::root_owner(f));
        }
    }
    let alive = |st: &State| st.exact.iter().copied().map(win::hwnd_from).find(|h| win::is_window(*h));
    if let Some(h) = alive(&lock()) {
        return Some(h);
    }
    let found = win::windows_z().into_iter().find(|&h| is_candidate(h) && is_host(h));
    if found.is_some() {
        return found;
    }
    // everything known has gone (terminal closed and reopened?): look again, at most every 10 s
    let stale = lock().resolved_at.map(|t| t.elapsed() > Duration::from_secs(10)).unwrap_or(true);
    if stale {
        resolve();
        return alive(&lock());
    }
    None
}

/// Layer 3: remember the foreground window at the start of a task as the place to return to.
/// Not marked as a host (it is a guess).
pub fn set_fallback(h: Option<HWND>) {
    lock().fallback = h.map(|h| h.0 as isize);
}

pub fn fallback() -> Option<HWND> {
    lock().fallback.map(win::hwnd_from).filter(|h| win::is_window(*h))
}

/// Usable as the layer 3 guess: an ordinary app window that is not ours.
pub fn plausible_fallback(h: HWND) -> bool {
    !h.0.is_null() && win::pid_of(h) != std::process::id() && is_candidate(h)
}

/// "Windows Terminal", "VS Code", ... for a window, from its app; "your app" when unknown.
pub fn label(h: HWND) -> String {
    let stem = win::exe_stem(win::pid_of(h));
    let known = match stem.as_str() {
        "windowsterminal" | "wt" => "Windows Terminal",
        "code" | "code - insiders" => "VS Code",
        "cursor" => "Cursor",
        "windsurf" => "Windsurf",
        "claude" => "Claude",
        "chatgpt" => "ChatGPT",
        "codex" => "Codex",
        "conhost" | "openconsole" => "the console window",
        "powershell" | "pwsh" => "PowerShell",
        "cmd" => "the console window",
        "wezterm-gui" => "WezTerm",
        "alacritty" => "Alacritty",
        "mintty" => "mintty",
        "devenv" => "Visual Studio",
        "zed" => "Zed",
        "" => "",
        other => {
            let mut c = other.chars();
            return match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => "your app".into(),
            };
        }
    };
    if win::class_name(h) == "ConsoleWindowClass" {
        return "the console window".into();
    }
    if known.is_empty() { "your app".into() } else { known.into() }
}

/// One line for `status`.
pub fn describe() -> String {
    let (source, fb) = {
        let st = lock();
        (st.source, st.fallback)
    };
    match resolved_window(None) {
        Some(h) => format!("home window: {:?} ({}, found via {source})", win::shown_title(h).chars().take(60).collect::<String>(), label(h)),
        None => match fb.map(win::hwnd_from).filter(|h| win::is_window(*h)) {
            Some(h) => format!("home window: {:?} (the window in front when the task started)", win::shown_title(h).chars().take(60).collect::<String>()),
            None => "home window: not known yet (the window in front when a task starts will be used)".into(),
        },
    }
}
