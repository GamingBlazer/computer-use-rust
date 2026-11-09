//! One find that tries each method in turn, and the OCR tools built on it.
//!
//! `locate(target)`: UI Automation first (an exact name, an exact automation id, a name that
//! contains the target, any element whose name contains it, in the window being worked in),
//! then the text on screen (OCR) in that window, then an error that says what was tried.
//! `find_text` / `click_text` work on the current view (the latest screenshot's coordinates).
//! A click is never retried here: a repeated click can double-submit.

use std::sync::Arc;

use image::RgbaImage;
use serde_json::{json, Value};

use crate::ocr::{self, MatchMode, Recognised, TextMatch};
use crate::tools;
use crate::{actions, capture, guards, settle, uia, win};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Uia,
    Ocr,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::Uia => "UI Automation",
            Method::Ocr => "on-screen text (OCR)",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Method::Uia => "uia",
            Method::Ocr => "ocr",
        }
    }
}

/// A place on screen found by `locate`.
#[derive(Clone, Debug)]
pub struct Located {
    pub method: Method,
    /// element name or matched text
    pub name: String,
    /// control type, or "text" for OCR
    pub kind: String,
    /// centre in screen pixels
    pub sx: i32,
    pub sy: i32,
    /// size in screen pixels (OCR only)
    pub w: i32,
    pub h: i32,
    /// the OCR line the text was in
    pub line: String,
    /// OCR: how many places matched (1 for UI Automation), and the centres of all of them
    pub count: usize,
    /// several places match and none is the single whole-word one: not safe to click
    pub ambiguous: bool,
    pub all: Vec<(i32, i32, String)>,
    /// the whole window picture OCR read (to tell afterwards whether a click changed anything)
    pub before: Option<Arc<RgbaImage>>,
    pub hwnd: isize,
}

/// Screen point -> the latest screenshot's coordinates, when it lies inside that view.
pub fn view_point(sx: i32, sy: i32) -> Option<(f64, f64)> {
    let v = *win::VIEW.lock().unwrap();
    v.rect.contains(sx, sy).then(|| ((sx - v.rect.left) as f64 / v.scale, (sy - v.rect.top) as f64 / v.scale))
}

/// "(x, y)" in screenshot coordinates, or the screen position when the point is outside the view.
pub fn where_is(sx: i32, sy: i32) -> String {
    match view_point(sx, sy) {
        Some((x, y)) => format!("({}, {})", x.round(), y.round()),
        None => {
            let mons = win::monitors();
            format!("screen ({sx}, {sy}) on monitor {} (outside the latest screenshot)", win::monitor_at(sx, sy, &mons) + 1)
        }
    }
}

impl Located {
    pub fn describe(&self) -> String {
        let at = where_is(self.sx, self.sy);
        match self.method {
            Method::Uia => format!("found via {}: {} {:?} at {at}", self.method.label(), self.kind, self.name),
            Method::Ocr => {
                let mut s = format!("found via {}: {:?} at {at}, {}x{} px", self.method.label(), self.name, self.w, self.h);
                if self.line != self.name {
                    s += &format!(", in the line {:?}", self.line.chars().take(80).collect::<String>());
                }
                if self.ambiguous {
                    s += &format!("; AMBIGUOUS: {} places match, this is only the first in reading order", self.count);
                } else if self.count > 1 {
                    s += &format!("; {} places match, this is the only whole-word one", self.count);
                }
                s
            }
        }
    }

    pub fn to_json(&self) -> Value {
        let view = view_point(self.sx, self.sy);
        json!({
            "found": true,
            "method": self.method.key(),
            "name": self.name,
            "type": self.kind,
            "x": view.map(|p| p.0.round()),
            "y": view.map(|p| p.1.round()),
            "screen_x": self.sx,
            "screen_y": self.sy,
            "w": self.w,
            "h": self.h,
            "line": self.line,
            "count": self.count,
            "ambiguous": self.ambiguous,
            "text": self.describe(),
        })
    }
}

fn host_or_gone(hwnd: windows::Win32::Foundation::HWND) -> Option<&'static str> {
    if hwnd.0.is_null() || !win::is_window(hwnd) {
        return Some("no window to read");
    }
    if win::is_host(hwnd) {
        return Some("the window is the AI app's own and is never read");
    }
    if unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd).as_bool() } {
        return Some("the window is minimized");
    }
    None
}

/// The picture of a window (its frame, with its popups), without touching the latest screenshot's view.
fn window_image(hwnd: windows::Win32::Foundation::HWND) -> Result<(RgbaImage, win::Rect), String> {
    let outer = win::window_rect(hwnd);
    let frame = win::frame_rect(hwnd);
    let img = capture::capture_window(hwnd, outer, frame)?;
    Ok((img, frame))
}

/// OCR matches in the window being worked in, in screen pixels. Err: OCR itself failed.
fn ocr_window(query: &str, mode: MatchMode) -> Result<(Vec<TextMatch>, win::Rect, Arc<RgbaImage>, Arc<Recognised>, isize), String> {
    let hwnd = uia::root_window();
    if let Some(why) = host_or_gone(hwnd) {
        return Err(format!("not read: {why}"));
    }
    guards::guard_target(hwnd)?;
    let (img, frame) = window_image(hwnd)?;
    let rec = ocr::recognise(&img, None)?;
    let m = ocr::find_matches(&rec, query, mode)?;
    Ok((m, frame, Arc::new(img), rec, hwnd.0 as isize))
}

/// Wait loops: does any of the names show as text in the work window? (Cached per picture.)
pub fn ocr_visible(names: &[&str]) -> Option<String> {
    let hwnd = uia::root_window();
    if host_or_gone(hwnd).is_some() || guards::guard_target(hwnd).is_err() {
        return None;
    }
    let (img, _) = window_image(hwnd).ok()?;
    let rec = ocr::recognise(&img, None).ok()?;
    for n in names {
        if let Ok(m) = ocr::find_matches(&rec, n, MatchMode::Contains) {
            if let Some(first) = m.into_iter().next() {
                return Some(first.text);
            }
        }
    }
    None
}

/// Find a target by id or name with UI Automation, then by on-screen text. Err: not found,
/// with what was tried.
pub fn locate(target: &str) -> Result<Located, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("target is required".into());
    }
    if uia::is_id(target) {
        // a list_elements id: only UI Automation knows it (use click_text for visible text)
        let e = uia::resolve_id(target, true)?;
        return Ok(Located {
            method: Method::Uia, name: e.name, kind: e.ctype, sx: e.cx, sy: e.cy, w: 0, h: 0, line: String::new(),
            count: 1, ambiguous: false, all: vec![], before: None, hwnd: 0,
        });
    }
    let mut tried: Vec<String> = Vec::new();
    match uia::locate_named(target) {
        Ok(Some(e)) => {
            return Ok(Located {
                method: Method::Uia, name: e.name, kind: e.ctype, sx: e.cx, sy: e.cy, w: 0, h: 0, line: String::new(),
                count: 1, ambiguous: false, all: vec![], before: None, hwnd: 0,
            })
        }
        Ok(None) => tried.push(format!("UI Automation (control names and automation ids in {:?}): no match", win::shown_title(uia::root_window()).chars().take(50).collect::<String>())),
        Err(e) => tried.push(format!("UI Automation: unavailable ({e})")),
    }
    match ocr_window(target, MatchMode::Contains) {
        Ok((m, frame, img, _rec, hwnd)) if !m.is_empty() => {
            let all: Vec<(i32, i32, String)> = m
                .iter()
                .map(|t| (frame.left + (t.x + t.w / 2.0).round() as i32, frame.top + (t.y + t.h / 2.0).round() as i32, t.line.clone()))
                .collect();
            // an exact (whole-word) match wins over partial ones when it is the only one
            let exact: Vec<usize> = m.iter().enumerate().filter(|(_, t)| t.exact).map(|(i, _)| i).collect();
            let pick = if exact.len() == 1 { exact[0] } else { 0 };
            let t = &m[pick];
            return Ok(Located {
                method: Method::Ocr,
                name: t.text.clone(),
                kind: "text".into(),
                sx: all[pick].0,
                sy: all[pick].1,
                w: t.w.round() as i32,
                h: t.h.round() as i32,
                line: t.line.clone(),
                count: m.len(),
                // ambiguity: several matches and no single whole-word one
                ambiguous: m.len() > 1 && exact.len() != 1,
                all,
                before: Some(img),
                hwnd,
            });
        }
        Ok((_, _, _, rec, _)) => tried.push(format!("on-screen text (OCR, {}): no match among {} lines", rec.lang, rec.lines.len())),
        Err(e) => tried.push(format!("on-screen text (OCR): {e}")),
    }
    Err(format!(
        "Nothing found for {target:?}. Tried: {}. Take a screenshot to see the window, then click by coordinates, or try another wording.",
        tried.join("; ")
    ))
}

/// `find(target)`: where and how a target was found, without clicking.
pub fn find(target: &str) -> Result<Located, String> {
    locate(target)
}

fn list_places(all: &[(i32, i32, String)]) -> String {
    all.iter()
        .take(10)
        .enumerate()
        .map(|(i, (x, y, line))| format!("  {}. {} in {:?}", i + 1, where_is(*x, *y), line.chars().take(60).collect::<String>()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Says whether the screen changed after a click (the click is never repeated). Looks for up to
/// 350 ms: an app may repaint a moment after the click.
fn watch_change(before: &RgbaImage, mut look: impl FnMut() -> Result<RgbaImage, String>) -> String {
    let start = std::time::Instant::now();
    loop {
        match look() {
            Ok(a) => {
                let f = ocr::diff_fraction(before, &a);
                if f >= 0.0005 {
                    return format!("The screen changed after the click (about {:.1}% of the window).", (f * 100.0).max(0.1));
                }
            }
            Err(e) => return format!("Could not look at the screen after the click ({e})."),
        }
        if start.elapsed() > std::time::Duration::from_millis(350) {
            return "Nothing visibly changed after the click (the screen looks the same); it may have had no effect. It was not repeated.".to_string();
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
}

/// click_element: UI Automation, then OCR, then an error that says what was tried.
pub fn click_element(target: &str, button: &str, double: bool) -> Result<String, String> {
    guards::keep_work_window()?;
    let l = locate(target)?;
    if l.method == Method::Ocr && l.ambiguous {
        return Err(format!(
            "{target:?} appears {} times as on-screen text (UI Automation has no such control); not clicking a guess. Matches:\n{}\nUse click_text(text, nth=N) or click by coordinates.",
            l.count,
            list_places(&l.all)
        ));
    }
    actions::click_screen(l.sx, l.sy, button, if double { 2 } else { 1 })?;
    let mut msg = format!("clicked {:?} ({})", l.name, format!("found via {}", l.method.label()));
    if l.method == Method::Ocr {
        if let Some(before) = &l.before {
            let hwnd = win::hwnd_from(l.hwnd);
            settle::wait_for_target(Some(capture::Target::Window(l.hwnd)), settle::ACTION, true);
            msg += ". ";
            msg += &watch_change(before, || window_image(hwnd).map(|p| p.0));
        }
    }
    Ok(msg)
}

// ---------- find_text / click_text on the current view ----------

pub struct ViewMatch {
    pub text: String,
    pub line: String,
    /// box in the latest screenshot's coordinates
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub cx: f64,
    pub cy: f64,
    pub exact: bool,
}

pub struct ViewHits {
    pub label: String,
    pub view_w: u32,
    pub view_h: u32,
    pub rec: Arc<Recognised>,
    pub cached: bool,
    pub matches: Vec<ViewMatch>,
    pub full: RgbaImage,
}

pub fn text_in_view(text: &str, mode: MatchMode, window: Option<&str>, monitor: Option<usize>, lang: Option<&str>) -> Result<ViewHits, String> {
    // the same guard as for reading a window any other way: not a password manager or the lock screen
    if let Some(name) = window {
        if let Some((h, _)) = win::find_window(name, 1.0) {
            guards::guard_target(h)?;
            if win::is_host(h) {
                return Err("not read: the window is the AI app's own and is never read".into());
            }
        }
    } else if monitor.is_none() {
        if let Some(h) = tools::view_window() {
            guards::guard_target(h)?;
            if win::is_host(h) {
                return Err("not read: the window is the AI app's own and is never read".into());
            }
        }
    }
    let mut shot = tools::grab(monitor, window, window.is_none() && monitor.is_none())?;
    let v = *win::VIEW.lock().unwrap();
    if v.hwnd.is_none() {
        // a monitor picture: the AI app's own window on it is never read, so it is blacked out first
        guards::mask_protected(&mut shot.full, v.rect, true);
    }
    let rec = ocr::recognise(&shot.full, lang)?;
    let cached = ocr::LAST_CACHED.load(std::sync::atomic::Ordering::Relaxed);
    let matches = ocr::find_matches(&rec, text, mode)?
        .into_iter()
        .map(|m| {
            let (x, y, w, h) = (m.x as f64 / v.scale, m.y as f64 / v.scale, m.w as f64 / v.scale, m.h as f64 / v.scale);
            ViewMatch { text: m.text, line: m.line, x, y, w, h, cx: x + w / 2.0, cy: y + h / 2.0, exact: m.exact }
        })
        .collect();
    Ok(ViewHits {
        label: shot.label.clone(),
        view_w: shot.rgb.width(),
        view_h: shot.rgb.height(),
        rec,
        cached,
        matches,
        full: shot.full,
    })
}

fn match_line(i: usize, m: &ViewMatch) -> String {
    let line = if m.line == m.text { String::new() } else { format!(", in the line {:?}", m.line.chars().take(80).collect::<String>()) };
    format!("{}. {:?} at ({}, {}) size {}x{}, centre ({}, {}){line}", i, m.text, m.x.round(), m.y.round(), m.w.round(), m.h.round(), m.cx.round(), m.cy.round())
}

pub fn header(h: &ViewHits) -> String {
    format!(
        "OCR ({}, {} lines, {}) on {} (view {}x{}):",
        h.rec.lang,
        h.rec.lines.len(),
        if h.cached { "cached, same picture".to_string() } else { format!("{} ms", h.rec.ms) },
        h.label,
        h.view_w,
        h.view_h
    )
}

/// The tool result of find_text.
pub fn find_text_report(text: &str, hits: &ViewHits, max: usize) -> String {
    let mut out = vec![header(hits)];
    if hits.matches.is_empty() {
        out.push(format!("No on-screen text matches {text:?}. OCR read: {}", ocr::sample_lines(&hits.rec, 8)));
    } else {
        out.push(format!("{} match(es) for {text:?}, reading order; coordinates are those of the latest screenshot:", hits.matches.len()));
        for (i, m) in hits.matches.iter().take(max).enumerate() {
            out.push(match_line(i + 1, m));
        }
        if hits.matches.len() > max {
            out.push(format!("... {} more (raise max)", hits.matches.len() - max));
        }
    }
    out.join("\n")
}

/// Pick the match to click: the only one, or the nth; several without nth: list and refuse.
pub fn pick<'a>(text: &str, hits: &'a ViewHits, nth: Option<usize>) -> Result<&'a ViewMatch, String> {
    if hits.matches.is_empty() {
        return Err(format!("No on-screen text matches {text:?} ({}). OCR read: {}", header(hits), ocr::sample_lines(&hits.rec, 8)));
    }
    match nth {
        Some(n) if n >= 1 && n <= hits.matches.len() => Ok(&hits.matches[n - 1]),
        Some(n) => Err(format!("nth={n} but only {} match(es) for {text:?}", hits.matches.len())),
        None if hits.matches.len() == 1 => Ok(&hits.matches[0]),
        None => {
            let list: Vec<String> = hits.matches.iter().take(12).enumerate().map(|(i, m)| match_line(i + 1, m)).collect();
            Err(format!(
                "{} places match {text:?}; not clicking a guess. Pass nth=N (1 = first in reading order), a more exact text, or match=\"exact\":\n{}",
                hits.matches.len(),
                list.join("\n")
            ))
        }
    }
}

pub struct ClickTextOpts<'a> {
    pub text: &'a str,
    pub nth: Option<usize>,
    pub button: &'a str,
    pub double: bool,
    pub offset_x: f64,
    pub offset_y: f64,
    pub mode: MatchMode,
    pub window: Option<&'a str>,
    pub monitor: Option<usize>,
    pub lang: Option<&'a str>,
}

/// click_text: find by OCR in the view, click the centre through the normal guarded click path.
pub fn click_text(o: &ClickTextOpts) -> Result<String, String> {
    let hits = text_in_view(o.text, o.mode, o.window, o.monitor, o.lang)?;
    let m = pick(o.text, &hits, o.nth)?;
    let (x, y) = (m.cx + o.offset_x, m.cy + o.offset_y);
    let desc = format!("{:?}", m.text);
    let msg = actions::click(x, y, o.button, if o.double { 2 } else { 1 }, None)?;
    // look again, never click again: a repeated click can double-submit
    settle::wait_for_target(settle::view_target(), settle::ACTION, true);
    let note = watch_change(&hits.full, || tools::grab(None, None, true).map(|s| s.full));
    Ok(format!("{msg} on the on-screen text {desc} (found via on-screen text (OCR), {}). {note}", if hits.cached { "cached".to_string() } else { format!("{} ms", hits.rec.ms) }))
}
