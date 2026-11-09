//! Actions shared by the single tools and run_actions.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsZoomed, SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOZORDER, SW_MAXIMIZE, SW_RESTORE};

use crate::{guards, input, overlay, win};

fn on(monitor: Option<usize>) -> String {
    monitor.map(|m| format!(" on monitor {}", m + 1)).unwrap_or_default()
}

/// Seconds from the model as a Duration: negative, NaN and absurd values are clamped (a negative or
/// NaN value would panic in Duration::from_secs_f64).
pub fn secs(x: f64, max: f64) -> Duration {
    Duration::from_secs_f64(if x.is_nan() { 0.0 } else { x.clamp(0.0, max) })
}

/// 1-based monitor number from the model -> 0-based index, with a clear error.
pub fn monitor_index(monitor: Option<u64>) -> Result<Option<usize>, String> {
    match monitor {
        None => Ok(None),
        Some(n) => {
            let count = win::monitors().len() as u64;
            if n == 0 || n > count {
                Err(format!("monitor must be 1 to {count}"))
            } else {
                Ok(Some((n - 1) as usize))
            }
        }
    }
}

pub fn click(x: f64, y: f64, button: &str, clicks: u32, monitor: Option<usize>) -> Result<String, String> {
    guards::keep_work_window()?;
    if monitor.is_none() {
        guards::require_view()?;
    }
    let (sx, sy) = win::to_screen(x, y, monitor);
    if monitor.is_none() {
        guards::assert_over_view(sx, sy, x, y)?;
    }
    guards::guard_point(sx, sy)?;
    input::glide(sx, sy, None);
    overlay::ripple(sx, sy);
    // the last look is right before each button press, after the glide
    input::click_at(sx, sy, button, clicks, &|| guards::recheck(Some((sx, sy))))?;
    let prefix = match clicks { 2 => "double-", 3 => "triple-", _ => "" };
    Ok(format!("{prefix}{}-clicked ({x}, {y}){}", input::button_flags(button).map(|_| button.to_string())?, on(monitor)))
}

/// Click at a screen point (physical pixels) through the normal guards: the user's stop and
/// takeover checks, the window in front, forbidden apps, and the window under the point.
pub fn click_screen(sx: i32, sy: i32, button: &str, clicks: u32) -> Result<(), String> {
    guards::keep_work_window()?;
    guards::guard_point(sx, sy)?;
    overlay::sync_monitor();
    input::glide(sx, sy, None);
    overlay::ripple(sx, sy);
    input::click_at(sx, sy, button, clicks, &|| guards::recheck(Some((sx, sy))))
}

pub fn drag(x: f64, y: f64, to_x: f64, to_y: f64, monitor: Option<usize>, to_monitor: Option<usize>, button: &str) -> Result<String, String> {
    guards::keep_work_window()?;
    if monitor.is_none() {
        guards::require_view()?;
    }
    input::button_flags(button)?;
    let (sx, sy) = win::to_screen(x, y, monitor);
    let (ex, ey) = win::to_screen(to_x, to_y, to_monitor.or(monitor));
    if monitor.is_none() {
        guards::assert_over_view(sx, sy, x, y)?;
    }
    guards::guard_point(sx, sy)?;
    guards::guard_point(ex, ey)?;
    input::glide(sx, sy, None);
    overlay::ripple(sx, sy);
    guards::recheck(Some((sx, sy)))?;
    // the button is released whatever happens next (an error, a caught panic): see ButtonHold
    let hold = input::ButtonHold::press(button)?;
    std::thread::sleep(Duration::from_millis(100)); // let the app notice the press before the move starts
    let d = (((ex - sx) as f64).powi(2) + ((ey - sy) as f64).powi(2)).sqrt();
    input::glide(ex, ey, Some((0.12 + d / 1500.0).clamp(0.5, 1.2)));
    std::thread::sleep(Duration::from_millis(100));
    if let Err(e) = guards::recheck(Some((ex, ey))) {
        // do not drop onto what is there now: go back to where the drag began and let go there
        input::glide(sx, sy, Some(0.2));
        hold.release();
        return Err(format!("{e}; the drag was undone (released back at its start)"));
    }
    hold.release();
    Ok(format!("dragged ({x}, {y}){} to ({to_x}, {to_y}){}", on(monitor), on(to_monitor.or(monitor))))
}

pub fn scroll(x: f64, y: f64, amount: i32, horizontal: i32, monitor: Option<usize>) -> Result<String, String> {
    guards::keep_work_window()?;
    if monitor.is_none() {
        guards::require_view()?;
    }
    let (sx, sy) = win::to_screen(x, y, monitor);
    if monitor.is_none() {
        guards::assert_over_view(sx, sy, x, y)?;
    }
    guards::guard_point(sx, sy)?;
    input::glide(sx, sy, None);
    guards::recheck(Some((sx, sy)))?;
    input::scroll(amount, horizontal);
    let what: Vec<String> = [(amount, "vertically"), (horizontal, "horizontally")].iter().filter(|(a, _)| *a != 0).map(|(a, w)| format!("{a} {w}")).collect();
    Ok(format!("scrolled {} at ({x}, {y}){}", if what.is_empty() { "0".into() } else { what.join(" and ") }, on(monitor)))
}

/// The most text one type_text takes (bytes).
pub const MAX_TYPE_BYTES: usize = 1_000_000;

pub fn type_text(text: &str) -> Result<String, String> {
    if text.len() > MAX_TYPE_BYTES {
        return Err(format!("text is {} bytes; one type_text takes at most {MAX_TYPE_BYTES} (1 MB): split it into several calls", text.len()));
    }
    guards::check_failsafe()?;
    guards::keep_work_window()?;
    input::KEYBOARD_ACTIVITY.store(true, std::sync::atomic::Ordering::Relaxed);
    let r = type_text_inner(text);
    input::wake_pointer();
    r
}

fn type_text_inner(text: &str) -> Result<String, String> {
    let (clean, dropped) = input::sanitize_text(text);
    let norm = clean.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = norm.split('\n').collect();
    let total_chars = norm.chars().count();
    let start = win::foreground();
    if start.0.is_null() {
        return Err("no window is in front, so there is nowhere to type".into());
    }
    let start_title = win::shown_title(start);
    // before every line and chunk: the stop, the user, and the window still being the one checked at the start
    let check = || -> Result<(), String> {
        guards::check_failsafe()?;
        guards::check_takeover()?;
        let fg = win::foreground();
        if fg != start {
            return Err(format!("the window in front changed from {:?} to {:?}", start_title.chars().take(50).collect::<String>(), win::shown_title(fg).chars().take(50).collect::<String>()));
        }
        Ok(())
    };
    let before = || crate::uia::focused_text();
    let settle = |was: Option<String>, secs: f64| -> bool {
        // Wait until the field shows a change when UI Automation can read it (true: it did; false: it did not in time);
        // a field that can't be read gets a short pause (false).
        match was {
            Some(was) => crate::uia::wait_focused_text_change_from(&was, secs),
            None => {
                std::thread::sleep(Duration::from_millis(100));
                false
            }
        }
    };
    let short = norm.trim().chars().count() <= 2;
    let private_field = !short && crate::uia::focused_is_password();
    let mut prog = input::Progress::default();
    let mut how = String::new();
    let result: Result<(), String> = if short || private_field {
        if private_field {
            how = " (as keystrokes: the field is a password field, so the clipboard was not used)".into();
        }
        input::type_lines_keys(&lines, 0, &check, &mut prog)
    } else {
        match input::paste_lines(&lines, 0, &before, &settle, &check, &mut prog) {
            Ok(()) => Ok(()),
            Err(input::Stop::Check(e)) => Err(e),
            Err(stop) => {
                how = match stop {
                    input::Stop::Sensitive => " (as keystrokes: the clipboard holds content a password manager marked private, so it was left alone)".into(),
                    _ => " (as keystrokes; the clipboard was busy)".into(),
                };
                // continue after the lines already typed; nothing is typed twice
                let from = prog.lines;
                input::type_lines_keys(&lines, from, &check, &mut prog)
            }
        }
    };
    let mut notes = String::new();
    if dropped > 0 {
        notes += &format!(" {dropped} control character(s) were not typed.");
    }
    for n in &prog.notes {
        notes += &format!(" Note: {n}.");
    }
    match result {
        Ok(()) => {
            let mut m = format!("typed {total_chars} characters{how}");
            if !notes.is_empty() {
                m += ".";
                m += &notes;
            }
            Ok(m)
        }
        Err(e) if e == guards::CANCELLED => Err(e),
        Err(e) => Err(format!(
            "{e}. Typing stopped after {} of {} characters ({} of {} lines complete); the rest was NOT typed. Do not retype it blindly: read_text or take a screenshot to see what is there, then continue from that point.{notes}",
            prog.chars.min(total_chars), total_chars, prog.lines, lines.len()
        )),
    }
}

pub fn keys(keys: &[String]) -> Result<String, String> {
    guards::keep_work_window()?;
    guards::refuse_close_on_host(keys, win::foreground())?;
    // the last look comes inside press, after the wait for a busy target and right before the key goes down
    let r = input::press_checked(keys, &|| guards::recheck(None));
    input::wake_pointer();
    r
}

pub fn wait(seconds: f64) -> Result<String, String> {
    let end = Instant::now() + secs(seconds, 30.0);
    while Instant::now() < end {
        guards::check_failsafe()?;
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(format!("waited {seconds}s"))
}

/// Where a wait looks: UI Automation and window titles, on-screen text (OCR), or both in turn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Auto,
    Uia,
    Ocr,
}

impl Source {
    pub fn parse(s: Option<&str>) -> Result<Source, String> {
        match s.map(|s| s.trim().to_lowercase()).as_deref() {
            None | Some("") | Some("auto") => Ok(Source::Auto),
            Some("uia") => Ok(Source::Uia),
            Some("ocr") => Ok(Source::Ocr),
            Some(o) => Err(format!("source must be auto, uia or ocr, not {o:?}")),
        }
    }
}

/// OCR in a wait loop runs no faster than every 250 ms; in between, the last answer stands.
struct OcrPoll {
    last: Option<Instant>,
    hit: Option<String>,
}

const OCR_EVERY: Duration = Duration::from_millis(250);

impl OcrPoll {
    fn new() -> OcrPoll {
        OcrPoll { last: None, hit: None }
    }
    fn poll(&mut self, names: &[&str]) -> Option<String> {
        if self.last.map(|t| t.elapsed() >= OCR_EVERY).unwrap_or(true) {
            self.hit = crate::finder::ocr_visible(names);
            self.last = Some(Instant::now());
        }
        self.hit.clone()
    }
}

/// One look for any of the names: windows (by title), controls or plain text through UI
/// Automation, and, when UI Automation finds nothing, the text on screen (OCR). Window titles
/// are only checked for scope window/any; OCR only for scope element/any.
fn find_any(names: &[&str], scope: &str, source: Source, ocr: &mut OcrPoll) -> Option<String> {
    if source != Source::Ocr {
        for name in names {
            if scope != "element" {
                if let Some((_, t)) = win::match_visible_window(name) {
                    return Some(t);
                }
            }
            if scope != "window" {
                if let Some(hit) = crate::uia::find_text(name) {
                    return Some(hit);
                }
            }
        }
    } else if scope != "element" {
        for name in names {
            if let Some((_, t)) = win::match_visible_window(name) {
                return Some(t);
            }
        }
    }
    if source != Source::Uia && scope != "window" {
        return ocr.poll(names).map(|t| format!("{t} (on-screen text, OCR)"));
    }
    None
}

pub fn wait_for(name: &str, timeout: f64, scope: &str, source: Source) -> Result<String, String> {
    if !["window", "element", "any"].contains(&scope) {
        return Err("scope must be window, element or any".into());
    }
    let names: Vec<&str> = name.split('|').map(str::trim).filter(|s| !s.is_empty()).collect();
    let deadline = Instant::now() + secs(timeout, 60.0);
    let mut ocr = OcrPoll::new();
    loop {
        if let Some(hit) = find_any(&names, scope, source, &mut ocr) {
            return Ok(format!("found {scope} {hit:?}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("{scope} named like {name:?} did not appear within {timeout}s (active window: {})", win::active_title()));
        }
        guards::check_failsafe()?;
        std::thread::sleep(Duration::from_millis(80)); // was 150 ms; the native search itself takes 50-400 ms
    }
}

pub fn wait_gone(name: &str, timeout: f64, scope: &str, source: Source) -> Result<String, String> {
    if !["window", "element", "any"].contains(&scope) {
        return Err("scope must be window, element or any".into());
    }
    let names: Vec<&str> = name.split('|').map(str::trim).filter(|s| !s.is_empty()).collect();
    let start = Instant::now();
    let deadline = start + secs(timeout, 300.0);
    let mut ocr = OcrPoll::new();
    while find_any(&names, scope, source, &mut ocr).is_some() {
        if Instant::now() >= deadline {
            return Err(format!("{name:?} was still showing after {timeout}s (active window: {})", win::active_title()));
        }
        guards::check_failsafe()?;
        std::thread::sleep(Duration::from_millis(120)); // was 250 ms
    }
    Ok(format!("{name:?} is gone after {:.1}s", start.elapsed().as_secs_f64()))
}

pub fn focus_window(name: &str) -> Result<String, String> {
    let (hwnd, title) = win::find_window(name, 1.0).ok_or_else(|| format!("No open window with {name:?} in its title."))?;
    if win::is_host(hwnd) {
        return Err(format!(
            "{title:?} is your AI app's own window: the user is already there and nothing can be typed, pressed or clicked into it, so it is never focused; finish takes the user back to it"
        ));
    }
    guards::guard_target(hwnd)?;
    if !matches!(win::bring_to_front_robust(hwnd), win::Switch::Failed) {
        guards::note_fg(true);
        return Ok(format!("focused {title:?}"));
    }
    Err(format!("Windows refused to bring {title:?} to the front (active window: {})", win::active_title()))
}

pub fn move_window(name: &str, monitor: usize, maximize: bool) -> Result<String, String> {
    let (hwnd, title) = win::find_window(name, 1.0).ok_or_else(|| format!("No open window with {name:?} in its title."))?;
    guards::guard_target(hwnd)?;
    let mons = win::monitors();
    let m = mons.get(monitor).ok_or_else(|| format!("monitor must be 1 to {}", mons.len()))?;
    unsafe {
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let centre = POINT { x: m.rect.left + m.rect.width / 2, y: m.rect.top + m.rect.height / 2 };
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(centre, MONITOR_DEFAULTTONEAREST), &mut info);
        let area = info.rcWork;
        let r = win::window_rect(hwnd);
        let w = r.width.min(area.right - area.left);
        let h = r.height.min(area.bottom - area.top);
        let x = area.left + (area.right - area.left - w) / 2;
        let y = area.top + (area.bottom - area.top - h) / 2;
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        if maximize {
            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        }
    }
    win::bring_to_front(hwnd);
    Ok(format!("moved {title:?} to monitor {}{}", monitor + 1, if maximize { " (maximized)" } else { "" }))
}

/// Launch an app by name from Windows' own app list or by .exe path, wait for its window and
/// bring it to the front. Only what `apps::classify` allows is started; a name that is no installed app is an error
/// (nothing is ever typed into Start or a search box).
pub fn open_app(name: &str) -> Result<String, String> {
    guards::guard_launch()?;
    let allowed = guards::ALLOWED.lock().unwrap().clone();
    let plan = crate::apps::classify(name, &allowed)?;
    let before: Vec<isize> = win::titled_windows().iter().map(|(h, _)| h.0 as isize).collect();
    let mut app_name: Option<String> = None;
    let how = match plan {
        crate::apps::Launch::Exe(path) => {
            // the resolved path is checked once more right before the shell is asked to run it
            crate::apps::check_resolved_exe(&path, &allowed)?;
            crate::apps::start_file(&path)?;
            format!("started {path:?}")
        }
        crate::apps::Launch::Installed(display, id) => {
            crate::apps::start_file(&format!("shell:AppsFolder\\{id}"))?;
            app_name = Some(display.clone());
            format!("launched {display:?}")
        }
    };
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut hwnd: Option<HWND> = None;
    while Instant::now() < deadline {
        if let Some((h, _)) = win::titled_windows().into_iter().find(|(h, _)| !before.contains(&(h.0 as isize)) && win::is_work_window(*h)) {
            hwnd = Some(h);
            break;
        }
        guards::check_failsafe()?;
        std::thread::sleep(Duration::from_millis(100));
    }
    if hwnd.is_none() {
        let stem = app_name.clone().unwrap_or_else(|| name.rsplit(['\\', '/']).next().unwrap_or(name).trim_end_matches(".exe").to_string());
        hwnd = win::match_window(&stem).map(|(h, _)| h);
    }
    match hwnd {
        Some(h) => {
            // whatever opened is held to the same policy as any window: not a security app, not outside apps.allowed
            if let Err(e) = guards::guard_target(h) {
                return Err(format!("{how}; but the window that opened is not one to work in: {e}"));
            }
            // was a fixed 200 ms for the new window to finish painting: wait until it stops changing
            crate::settle::wait_for_target(Some(crate::capture::Target::Window(h.0 as isize)), crate::settle::WINDOW, true);
            win::bring_to_front(h);
            guards::note_fg(true);
            Ok(format!("{how}; window {:?} is in front", win::shown_title(h)))
        }
        None => Ok(format!("{how}; no new window appeared within 8 s (active window: {})", win::active_title())),
    }
}
