//! Event-driven settling after an action: instead of a fixed 200 ms sleep, wait until the thing
//! being looked at stops changing. Signals (polled every 10 ms):
//!  * the picture of the target window or monitor: a hash of the newest Windows.Graphics.Capture
//!    frame of the open capture session (the frame counter itself is no use: Windows delivers
//!    frames continuously, changed or not). Watched when a session for the target is open, or
//!    opened for it when a screenshot follows anyway;
//!  * the foreground window and its title, and the target window's rectangle (a dialog opening,
//!    a window moving or resizing).
//! UI Automation structure-changed events are deliberately not used: subscribing to them makes
//! browsers switch their accessibility tree on for the whole process.
//! Done when nothing changed for `quiet`, but not before `min`; never later than `cap`. When
//! there is no target to watch at all, the old fixed wait applies.

use std::time::{Duration, Instant};

use crate::capture::{self, Target};
use crate::win;

#[derive(Clone, Copy)]
pub struct Opts {
    pub min: Duration,
    pub quiet: Duration,
    pub cap: Duration,
    /// the old fixed wait, used when there is no signal
    pub fallback: Duration,
}

/// After an input action (click, keys, typing ...).
pub const ACTION: Opts = Opts { min: Duration::from_millis(30), quiet: Duration::from_millis(80), cap: Duration::from_millis(700), fallback: Duration::from_millis(200) };
/// A window that was just created or restored.
pub const WINDOW: Opts = Opts { min: Duration::from_millis(30), quiet: Duration::from_millis(80), cap: Duration::from_millis(500), fallback: Duration::from_millis(300) };

pub enum Outcome {
    Settled(Duration),
    Capped,
    NoSignal,
}

/// What a screenshot of the current view would be a picture of.
pub fn view_target() -> Option<Target> {
    if let Some(h) = crate::tools::view_window() {
        return Some(Target::Window(h.0 as isize));
    }
    let mons = win::monitors();
    let v = *win::VIEW.lock().unwrap();
    let idx = if v.pinned.is_some() {
        v.index
    } else {
        match win::work_window() {
            Some(h) => {
                let r = win::window_rect(h);
                win::monitor_at(r.left + r.width / 2, r.top + r.height / 2, &mons)
            }
            None => v.index,
        }
    };
    mons.get(idx).map(|m| Target::Monitor(m.handle))
}

/// What is compared between polls: the picture (when a capture session for the target is
/// open), the foreground window and its title, the target window's rectangle.
fn signature(target: Option<Target>, pixels: bool) -> (Option<u64>, isize, u64, [i32; 4]) {
    let pic = match (target, pixels) {
        (Some(t), true) => capture::picture_hash(t),
        _ => None,
    };
    let fg = win::foreground();
    let title = win::title(fg);
    let mut h = 1469598103934665603u64;
    for b in title.bytes() {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    let rect = match target {
        Some(Target::Window(w)) => {
            let r = win::window_rect(win::hwnd_from(w));
            [r.left, r.top, r.width, r.height]
        }
        _ => [0; 4],
    };
    (pic, fg.0 as isize, h, rect)
}

/// `pixels`: also watch the picture, opening the capture session if there is none (worth it
/// when a screenshot follows, which needs the session anyway). Without it the picture is only
/// watched when a session for the target happens to be open already.
pub fn wait_for_target(target: Option<Target>, o: Opts, pixels: bool) -> Outcome {
    wait_with(target, o, pixels, || ()).0
}

/// Like wait_for_target, and `probe` (the text observation's after-snapshot) is run once as soon
/// as `min` has passed, overlapping its own cost with the quiet period. Its result is returned
/// only when nothing changed from the moment it started until the end; None means the caller
/// must probe again.
pub fn wait_with<T>(target: Option<Target>, o: Opts, pixels: bool, mut probe: impl FnMut() -> T) -> (Outcome, Option<T>) {
    let start = Instant::now();
    if target.is_none() {
        std::thread::sleep(o.fallback);
        return (Outcome::NoSignal, None);
    }
    let mut probed: Option<(Instant, T)> = None;
    let watch_pixels = match target {
        Some(t) => capture::session_for(t, pixels),
        None => false,
    };
    let mut prev = signature(target, watch_pixels);
    let mut last_change = start;
    loop {
        std::thread::sleep(Duration::from_millis(10));
        let now = Instant::now();
        let mut sig = signature(target, watch_pixels);
        if watch_pixels && sig.0.is_none() {
            sig.0 = prev.0; // no frame to look at right now: no news
        }
        if sig != prev {
            if std::env::var_os("DC_TRACE").is_some() {
                eprintln!("settle: change at {} ms (picture {} fg {} title {} rect {})", (now - start).as_millis(), prev.0 != sig.0, prev.1 != sig.1, prev.2 != sig.2, prev.3 != sig.3);
            }
            prev = sig;
            last_change = now;
        }
        let elapsed = now - start;
        if elapsed >= o.cap {
            return (Outcome::Capped, probed.map(|(_, v)| v).filter(|_| false));
        }
        if elapsed >= o.min && probed.is_none() {
            let at = Instant::now();
            probed = Some((at, probe()));
            continue; // poll again: anything that changed meanwhile invalidates the probe
        }
        if elapsed >= o.min && now - last_change >= o.quiet {
            if let (Some(t), true) = (target, watch_pixels) {
                capture::mark_stable(t);
            }
            let valid = probed.map(|(at, v)| (at, v)).filter(|(at, _)| *at >= last_change).map(|(_, v)| v);
            return (Outcome::Settled(elapsed), valid);
        }
    }
}

/// Settle on whatever the current view shows.
pub fn after_action<T>(pixels: bool, probe: impl FnMut() -> T) -> (Outcome, Option<T>) {
    wait_with(view_target(), ACTION, pixels, probe)
}
