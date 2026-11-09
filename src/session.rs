//! Task state shared by the tools and (later) the overlay: whether a task is running, the window
//! to return the user to (the AI app's, see `home`), and the turn-ended file a client's Stop hook touches.

use std::sync::atomic::Ordering;
use crate::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use crate::{guards, home, hooks, win};

pub struct Task {
    pub active: bool,
    pub home: Option<isize>,
    pub started: SystemTime,
    pub last: Option<Instant>,
    pub busy: u32,
    pub status: String,
    /// windows the task brought to the front, screenshotted or sent input to
    pub targets: Vec<isize>,
}

pub static TASK: Mutex<Task> = Mutex::new(Task { active: false, home: None, started: SystemTime::UNIX_EPOCH, last: None, busy: 0, status: String::new(), targets: Vec::new() });

/// A task has started and no AI app window was identified: say so in its first result.
static WARN_HOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The no-host warning, once per task.
pub fn take_host_warning() -> Option<String> {
    if WARN_HOST.swap(false, Ordering::Relaxed) {
        home::unresolved_warning()
    } else {
        None
    }
}

pub fn turn_ended_path() -> std::path::PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("turn-ended"))).unwrap_or_else(|| "turn-ended".into())
}

pub fn turn_ended(since: SystemTime) -> bool {
    std::fs::metadata(turn_ended_path()).and_then(|m| m.modified()).map(|t| t > since).unwrap_or(false)
}

/// When the turn-ended file was last touched (unix milliseconds), if that was after `since_ms`.
pub fn turn_ended_at_ms(since_ms: u64) -> Option<u64> {
    let t = std::fs::metadata(turn_ended_path()).and_then(|m| m.modified()).ok()?;
    let ms = t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis() as u64;
    (ms > since_ms).then_some(ms)
}

/// `desktop-control.exe turn-ended`: create the turn-ended file next to the exe if needed and set its
/// modification time to now. Exit code 0 when it worked.
pub fn touch_turn_ended() -> i32 {
    let path = turn_ended_path();
    let r = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&path).and_then(|f| f.set_modified(SystemTime::now()));
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("turn-ended: cannot touch {}: {e}", path.display());
            1
        }
    }
}

/// Mark activity; the first call of a task remembers the user's window. Returns true when this
/// starts a new task.
pub fn touch() -> bool {
    // the home window is looked up before the task lock is taken (it may have to look again)
    let home_now = if TASK.lock().unwrap().active { None } else { Some(start_home()) };
    let mut t = TASK.lock().unwrap();
    let starting = !t.active;
    t.active = true;
    t.last = Some(Instant::now());
    if starting {
        t.home = home_now.unwrap_or_else(start_home);
        t.started = SystemTime::now();
        t.targets.clear();
        hooks::ACTIVE.store(true, Ordering::Relaxed);
        WARN_HOST.store(home::unresolved(), Ordering::Relaxed);
        {
            let mut v = win::VIEW.lock().unwrap();
            v.pinned = None;
            v.hwnd = None;
            v.frame = None;
        }
        guards::acknowledge();
        crate::uia::clear_elements();
        crate::overlay::show();
    }
    // the AI app's windows for the hooks (the fallback window is known by now). Outside the task lock: it enumerates and
    // queries windows, which must never happen while a lock the overlay thread waits for is held.
    drop(t);
    if starting {
        home::refresh_host_cache();
    }
    starting
}

/// Where the user is taken back to when the task ends: the AI app's window (the foreground one
/// when it is a window of that app, else the exact window found at start-up), else, when no AI app
/// could be identified, the window in front when the first action starts.
fn start_home() -> Option<isize> {
    let fg = win::foreground();
    if let Some(h) = home::resolved_window(Some(fg)) {
        return Some(h.0 as isize);
    }
    if home::plausible_fallback(fg) {
        home::set_fallback(Some(fg));
        return Some(fg.0 as isize);
    }
    None
}

/// After a tool panicked (caught by the registry): the busy counter its activity never lowered.
pub fn recover_after_panic() {
    TASK.lock().unwrap().busy = 0;
}

pub fn set_status(s: &str) {
    TASK.lock().unwrap().status = s.to_string();
    crate::overlay::status(s);
}

/// End the task; returns the window to return the user to.
pub fn end() -> Option<isize> {
    let mut t = TASK.lock().unwrap();
    t.active = false;
    hooks::ACTIVE.store(false, Ordering::Relaxed);
    // a new task starts with no image or snapshot on record and the normal image size
    crate::observe::reset_last_image();
    win::SMALL.store(false, Ordering::Relaxed);
    crate::uia::clear_elements();
    t.targets.clear();
    t.home.take()
}

/// Remember a window the task worked in (brought to the front, screenshotted, sent input), so
/// that an idle task end can tell it from a window the user went to on their own.
pub fn note_target(h: windows::Win32::Foundation::HWND) {
    if h.0.is_null() {
        return;
    }
    let mut t = TASK.lock().unwrap();
    let id = h.0 as isize;
    if !t.targets.contains(&id) {
        if t.targets.len() >= 32 {
            t.targets.remove(0);
        }
        t.targets.push(id);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// the finish tool
    Finish,
    /// the turn ended (a client hook touched the turn-ended file) without finish
    TurnEnded,
    /// 30 s without a screen call
    Idle,
}

/// A task that has just ended: where to return the user, and what the task worked in.
pub struct Ended {
    pub reason: Reason,
    pub home: Option<isize>,
    pub targets: Vec<isize>,
}

fn end_locked(t: &mut Task, reason: Reason) -> Ended {
    t.active = false;
    // a stop by Esc is NOT cleared here: finishing is the agent's act, and only the user's side
    // (the turn ending, or a quiet period) lifts it; see hooks::try_clear_stop / note_tool_call
    hooks::ACTIVE.store(false, Ordering::Relaxed);
    crate::uia::clear_elements();
    crate::observe::reset_last_image();
    win::SMALL.store(false, Ordering::Relaxed);
    Ended { reason, home: t.home.take(), targets: std::mem::take(&mut t.targets) }
}

/// The finish tool: end the task (whether or not one is marked active).
pub fn end_task(reason: Reason) -> Ended {
    let mut t = TASK.lock().unwrap();
    end_locked(&mut t, reason)
}

/// The overlay's poll: end the task when it is over (the turn ended, or 30 s without a call)
/// and nothing is running.
pub fn poll_end() -> Option<Ended> {
    let mut t = TASK.lock().unwrap();
    if !t.active || t.busy != 0 {
        return None;
    }
    let quiet = t.last.map(|l| l.elapsed()).unwrap_or_default();
    let reason = if turn_ended(t.started) {
        Reason::TurnEnded
    } else if quiet > IDLE_HIDE {
        Reason::Idle
    } else {
        return None;
    };
    Some(end_locked(&mut t, reason))
}

/// What finish and the turn-ended path both do once the task has ended: the green banner and
/// the chime, release the capture session, and take the user back to the window they started in
/// (the AI app's), or flash its taskbar button when switching would be rude or impossible. An idle
/// end shows no banner. Returns a truthful one-paragraph account of what happened.
pub fn complete(ended: Ended, summary: Option<&str>) -> String {
    let banner = match ended.reason {
        Reason::Idle => None,
        _ => Some(crate::client::done_banner(summary)),
    };
    // the pointer must be there when the user takes over again (Windows hides it while typing)
    crate::input::wake_pointer_at_end();
    if let Some(b) = &banner {
        crate::overlay::end(b, crate::overlay::DONE_COLOR);
        unsafe {
            let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(windows::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK);
        }
    }
    crate::capture::release();
    // Switching windows sends messages to the target window's thread; a hung AI app would block
    // the caller for good, so it runs on a helper thread with a deadline.
    let reason = ended.reason;
    let home = home_window(ended.home);
    let banner_owned = banner.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("go-home".into()).spawn(move || {
        let _ = tx.send(go_home(&ended, banner_owned.as_deref()));
    });
    let msg = match rx.recv_timeout(Duration::from_secs(4)) {
        Ok(m) => m,
        Err(_) => {
            if let Some(h) = home {
                win::flash(h);
            }
            "Indicator hidden; the home window did not answer within 4 s (not responding?), so focus was not moved; its taskbar button was flashed.".to_string()
        }
    };
    // the log gets the outcome without the quoted window titles (those and the finish summary are the user's business)
    eprintln!("task completed ({reason:?}): {}", redact_quoted(&msg));
    msg
}

/// Replace every double-quoted piece of a message (the window titles in it are Debug-formatted) by "..."; a
/// backslash escapes the next character inside quotes.
pub fn redact_quoted(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut in_quote = false;
    let mut escaped = false;
    for c in msg.chars() {
        if in_quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_quote = false;
                out.push_str("...\"");
            }
        } else {
            out.push(c);
            if c == '"' {
                in_quote = true;
            }
        }
    }
    if in_quote {
        out.push_str("...\"");
    }
    out
}

fn go_home(ended: &Ended, banner: Option<&str>) -> String {
    use crate::win::Switch;
    let trace = |what: &str| {
        if std::env::var_os("DC_TRACE").is_some() {
            eprintln!("go_home: {what}");
        }
    };
    trace("start");
    let Some(home) = home_window(ended.home) else {
        return "Indicator hidden, but your app's window wasn't found to switch back to.".into();
    };
    let label = home::label(home);
    trace("home found");
    let home_title = win::shown_title(home);
    let fg = win::foreground();
    let now = || win::active_title();
    // a new task started meanwhile: leave the screen to it
    if TASK.lock().unwrap().active {
        return "Indicator hidden; a new task already started, so the screen was left alone.".into();
    }
    if fg == home || win::is_host(fg) {
        guards::note_fg(true);
        return format!("Indicator hidden; you are already in {:?}.", now());
    }
    let flash = |why: &str| {
        win::flash(home);
        if let Some(b) = banner {
            crate::overlay::end_for(b, crate::overlay::DONE_COLOR, 4000);
        }
        format!("Indicator hidden; {why}; flashed the taskbar button of {home_title:?} instead (active window: {}).", now())
    };
    // never steal the keyboard from someone typing
    if hooks::user_typing_within(1500) {
        return flash("you were typing, so focus was not moved");
    }
    // idle: don't yank the user out of an unrelated window they went to themselves
    if ended.reason == Reason::Idle && win::is_work_window(fg) && !ended.targets.contains(&(fg.0 as isize)) {
        return flash("you are in another window of your own");
    }
    trace("switching");
    match win::bring_to_front_robust(home) {
        Switch::Direct => {
            guards::note_fg(true);
            format!("Indicator hidden; back in {label} ({:?}).", now())
        }
        Switch::Attached => {
            guards::note_fg(true);
            format!("Indicator hidden; back in {label} ({:?}; Windows' foreground lock refused the first try, the retry attached to the foreground thread and worked).", now())
        }
        Switch::Failed => flash("Windows refused to switch back, even after retrying with an attached input queue"),
    }
}

pub fn home_window(home: Option<isize>) -> Option<windows::Win32::Foundation::HWND> {
    home.map(win::hwnd_from).filter(|h| win::is_window(*h)).or_else(|| home::resolved_window(None)).or_else(home::fallback)
}

/// Runs a tool body as task activity: shows the indicator with what the AI is doing, resets
/// the input baseline afterwards, and turns a stop into the stop message.
pub fn activity<T>(doing: &str, body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    touch();
    {
        let mut t = TASK.lock().unwrap();
        t.busy += 1;
    }
    set_status(doing);
    crate::overlay::sync_monitor();
    // while the user's stop holds, no screen tool runs at all (reading included); finish and status stay open
    let gate = if hooks::STOP.load(Ordering::Relaxed) && !hooks::try_clear_stop() { Err(guards::STOP_MESSAGE.to_string()) } else { Ok(()) };
    let out = match gate {
        Ok(()) => body(),
        Err(e) => Err(e),
    };
    crate::input::wake_pointer();
    guards::reset_baseline();
    {
        let mut t = TASK.lock().unwrap();
        t.last = Some(Instant::now());
        t.busy -= 1;
    }
    guards::note_fg(false);
    if let Err(e) = &out {
        if e.starts_with("Computer Use was stopped") || e.contains("screen corner to stop") {
            crate::overlay::end(crate::overlay::STOPPED_TEXT, crate::overlay::STOP_COLOR);
            end();
            return out;
        }
    }
    crate::overlay::sync_monitor();
    set_status("thinking…");
    out
}

pub const IDLE_HIDE: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_titles_are_removed_from_the_log_line() {
        assert_eq!(redact_quoted("Indicator hidden; back in Windows Terminal (\"Secret chat - x\")."), "Indicator hidden; back in Windows Terminal (\"...\").");
        assert_eq!(redact_quoted("a \"b \\\" c\" d \"e\""), "a \"...\" d \"...\"");
        assert_eq!(redact_quoted("no quotes"), "no quotes");
        assert_eq!(redact_quoted("open \"unterminated"), "open \"...\"");
    }
}
