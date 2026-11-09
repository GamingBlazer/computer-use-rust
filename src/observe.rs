//! What an action tool returns afterwards (`observe`): a short text summary (the default), a
//! screenshot, both, or nothing; plus the unchanged-screenshot suppression and the image size.
//!
//! text       message, active window, focused control, and a compact before/after diff of the
//!            work window's interactive controls (UI Automation; bounded by time and nodes)
//! screenshot message, monitor note, active window, the image
//! both       the text summary and the image
//! none       the message only
//! brief      message, active window, focused control (no change list; cheap). Not in the schema:
//!            it is what the legacy `screenshot_after: false` means, like before the observe modes.
//! The legacy `screenshot_after: true/false` maps to both/brief and beats the default, but not
//! an explicit `observe`.

use crate::sync::Mutex;

use serde_json::Value;

use crate::tools::{self, Content};
use crate::{settle, uia, win};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Text,
    Screenshot,
    Both,
    None,
    Brief,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s.trim().to_lowercase().as_str() {
            "text" => Mode::Text,
            "screenshot" | "image" => Mode::Screenshot,
            "both" => Mode::Both,
            "none" => Mode::None,
            "brief" => Mode::Brief,
            _ => return None,
        })
    }
    fn wants_text(self) -> bool {
        matches!(self, Mode::Text | Mode::Both)
    }
    fn wants_image(self) -> bool {
        matches!(self, Mode::Screenshot | Mode::Both)
    }
}

/// observe > screenshot_after > `default`.
pub fn mode_of(a: &Value, default: Mode) -> Mode {
    if let Some(m) = a.get("observe").and_then(Value::as_str).and_then(Mode::parse) {
        return m;
    }
    match a.get("screenshot_after").and_then(Value::as_bool) {
        Some(true) => Mode::Both,
        Some(false) => Mode::Brief,
        None => default,
    }
}

/// `size`: "small" = 1024 px wide images, "normal" = 1366 px. `screenshot` calls it with
/// `reset = true` (absent means normal); an action keeps the size of the latest screenshot.
pub fn apply_size(a: &Value, reset: bool) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    match a.get("size").and_then(Value::as_str).map(|s| s.trim().to_lowercase()) {
        Some(s) if s == "small" => win::SMALL.store(true, Ordering::Relaxed),
        Some(s) if s == "normal" => win::SMALL.store(false, Ordering::Relaxed),
        Some(s) if !s.is_empty() => return Err(format!("size must be \"normal\" or \"small\", not {s:?}")),
        _ => {
            if reset {
                win::SMALL.store(false, Ordering::Relaxed);
            }
        }
    }
    Ok(())
}

// ---------- unchanged-screenshot suppression ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewKey {
    Window(isize),
    Monitor(isize),
}

pub struct Shot {
    pub rgb: image::RgbImage,
    pub key: ViewKey,
    /// "window \"Untitled - Notepad\"" or "monitor 1"
    pub label: String,
    /// the capture at full resolution (what OCR reads)
    pub full: image::RgbaImage,
}

static LAST: Mutex<Option<(ViewKey, image::RgbImage)>> = Mutex::new(None);
/// The "after" snapshot of the previous text observation: the next action's "before", so a run
/// of actions takes one snapshot each instead of two. Dropped after an action without a text
/// observation (the screen may have changed unseen) and when a task ends.
static LAST_SNAP: Mutex<Option<(uia::Snap, std::time::Instant)>> = Mutex::new(None);
const SNAP_REUSE: std::time::Duration = std::time::Duration::from_secs(90);

/// A new task starts with no image and no snapshot on record.
pub fn reset_last_image() {
    *LAST.lock().unwrap() = None;
    *LAST_SNAP.lock().unwrap() = None;
}

/// The image content for a shot, or, when its downscaled pixels are exactly those of the last
/// image sent for the same view (same window or monitor, same size), a text saying so. Exact
/// equality only: a one-character change always gets through. `force` always sends the image.
pub fn image(shot: Shot, force: bool) -> Content {
    let mut last = LAST.lock().unwrap();
    if !force {
        if let Some((k, prev)) = last.as_ref() {
            if *k == shot.key && prev.dimensions() == shot.rgb.dimensions() && prev.as_raw() == shot.rgb.as_raw() {
                return Content::text(format!(
                    "Screen unchanged since the last screenshot ({}, {}x{}); coordinates still refer to it. Use screenshot(force=true) to get the image again.",
                    shot.label,
                    shot.rgb.width(),
                    shot.rgb.height()
                ));
            }
        }
    }
    let c = Content::jpeg(&crate::capture::jpeg(&shot.rgb));
    *last = Some((shot.key, shot.rgb));
    c
}

/// An image that is not tracked for suppression (all-monitors view).
pub fn untracked_image(rgb: &image::RgbImage) -> Content {
    *LAST.lock().unwrap() = None;
    Content::jpeg(&crate::capture::jpeg(rgb))
}

// ---------- the observation ----------

pub struct Obs {
    mode: Mode,
    before: Option<uia::Snap>,
}

impl Obs {
    /// Call before the action: applies `size` and, for text modes, takes the "before" snapshot.
    pub fn begin(a: &Value, default: Mode) -> Result<Obs, String> {
        apply_size(a, false)?;
        let mode = mode_of(a, default);
        let t = std::time::Instant::now();
        let before = if mode.wants_text() {
            let reuse = LAST_SNAP.lock().unwrap().take().filter(|(_, at)| at.elapsed() < SNAP_REUSE).map(|(s, _)| s);
            reuse.or_else(uia::snapshot)
        } else {
            *LAST_SNAP.lock().unwrap() = None;
            None
        };
        trace("snapshot before", t);
        Ok(Obs { mode, before })
    }

    /// Call after the action with its message.
    pub fn finish(self, message: String) -> Result<Vec<Content>, String> {
        if self.mode == Mode::None {
            *LAST_SNAP.lock().unwrap() = None;
            return Ok(vec![Content::text(message)]);
        }
        if self.mode == Mode::Brief {
            *LAST_SNAP.lock().unwrap() = None;
            // no settle wait, like screenshot_after=false before the observe modes: the caller
            // asked for speed, and the next step settles or waits itself
            let mut text = format!("{message}\nActive window now: {}", win::active_title());
            if let Some(f) = uia::focus_line() {
                text.push('\n');
                text += &f;
            }
            return Ok(vec![Content::text(text)]);
        }
        let t = std::time::Instant::now();
        let wants_text = self.mode.wants_text();
        // the after-snapshot runs inside the settle wait, overlapping with its quiet period
        let (_, probed) = settle::after_action(self.mode.wants_image(), || if wants_text { Some((uia::focus_line(), uia::snapshot())) } else { None });
        trace("settle (with snapshot)", t);
        let mut text = message;
        if self.mode.wants_text() {
            text.push('\n');
            let (s, after) = summary(self.before.as_ref(), probed.flatten());
            text += &s;
            *LAST_SNAP.lock().unwrap() = after.map(|a| (a, std::time::Instant::now()));
        }
        if self.mode.wants_image() {
            if !self.mode.wants_text() {
                text += &format!("\nActive window now: {}", win::active_title());
            }
            // the action has been done; a picture that is refused (a window that must not be read) must not
            // turn that into an error
            let shot = match tools::grab(None, None, true) {
                Ok(s) => s,
                Err(e) => return Ok(vec![Content::text(format!("{text}\n(no screenshot: {e})"))]),
            };
            let note = tools::monitor_note(false);
            if !note.is_empty() {
                text.push('\n');
                text += note.trim_end();
            }
            return Ok(vec![Content::text(text), image(shot, false)]);
        }
        Ok(vec![Content::text(text)])
    }
}

/// Active window, focused control, and the change list.
fn summary(before: Option<&uia::Snap>, probed: Option<(Option<String>, Option<uia::Snap>)>) -> (String, Option<uia::Snap>) {
    let mut lines = vec![format!("Active window now: {}", win::active_title())];
    // reuse the snapshot taken during the settle wait when nothing changed since; else take it now
    let (focus, after) = probed.unwrap_or_else(|| (uia::focus_line(), uia::snapshot()));
    if let Some(f) = focus {
        lines.push(f);
    }
    match (before, after.as_ref()) {
        (Some(b), Some(a)) => {
            let d = uia::diff(b, a, 15);
            if d.len() == 1 && d[0].starts_with("no control changes") {
                lines.push("Changes: none detected in the interactive controls (take a screenshot if the page content may have changed).".into());
            } else {
                lines.push("Changes (+ added, - removed, ~ changed):".into());
                lines.extend(d.into_iter().map(|l| format!("  {l}")));
            }
        }
        (None, _) => lines.push("Changes: unavailable (no readable window before the action).".into()),
        (_, None) => lines.push("Changes: unavailable (the window can't be read after the action).".into()),
    }
    (lines.join("
"), after)
}

/// DC_TRACE=1 logs where the time of an observation goes (stderr).
fn trace(what: &str, since: std::time::Instant) {
    if std::env::var_os("DC_TRACE").is_some() {
        eprintln!("observe: {what}: {} ms", since.elapsed().as_millis());
    }
}
