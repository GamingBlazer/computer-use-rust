//! Safety guards run before every input action (stale-state and policy checks):
//! the user's stop (Esc or a screen corner), the user having taken over, the target window
//! being in front, forbidden apps, the locked desktop, a window that moved since the screenshot,
//! a point now covered by another app. The same program policy also guards what is read.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::HWND;

use crate::sync::Mutex;
use crate::{hooks, input, win};

pub const STOP_MESSAGE: &str = "Computer Use was stopped by the user with the physical Escape key. Stop your work now: call \
    finish, tell the user why you stopped, and wait for the user. Do not call any screen tool again until the user tells you to \
    continue";

/// The client cancelled the request being worked on (notifications/cancelled); no response is sent for it.
pub const CANCELLED: &str = "cancelled by the client";

pub static LOCKED: &[&str] = &["lockapp.exe", "logonui.exe", "winlogon.exe"];
pub static FORBIDDEN: &[&str] = &[
    "securityhealthsystray.exe", "securityhealthui.exe", "sechealthui.exe", "credentialuibroker.exe", "1password.exe", "bitwarden.exe",
    "keepass.exe", "keepassxc.exe", "keeper.exe", "enpass.exe", "dashlane.exe", "lastpass.exe", "nordpass.exe", "roboform.exe",
];

/// A program the lock screen or a security / password-manager app runs in: never read, never controlled.
pub fn is_protected_exe(exe: &str) -> bool {
    LOCKED.contains(&exe) || FORBIDDEN.contains(&exe)
}

/// A program whose windows' titles are not shown: the protected ones and one that can't be identified (an
/// unreadable program is treated like a protected one everywhere, see `check_exe`).
pub fn hides_title(exe: &str) -> bool {
    exe.is_empty() || exe == win::FRAME_HOST || is_protected_exe(exe)
}

/// Micros (hooks clock) right after the AI's last input or screenshot; later input is the user's.
pub static BASELINE_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The user took over (used the mouse or keyboard since the agent last looked): every input action is
/// refused until the agent takes a fresh screenshot or listing.
static TAKEOVER: AtomicBool = AtomicBool::new(false);
/// Foreground window when the AI last looked or acted (the work window).
pub static SEEN_FG: Mutex<Option<isize>> = Mutex::new(None);
/// Allowed apps from config.json (lower-case exe names); empty = any.
pub static ALLOWED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Keyboard, typing, clicks, scrolling, dragging, set_value and element_action are never sent to the
/// window of the AI app the server runs under (its terminal or chat window: a stray Enter submits a
/// prompt, alt+F4 closes the session). There is no opt-in.
pub fn refuse_host(hwnd: HWND) -> Result<(), String> {
    if !hwnd.0.is_null() && win::is_host(hwnd) {
        return Err(format!(
            "the window in front is your AI app's own window ({:?}), so nothing is typed, pressed or clicked into it; bring the target app to the front first (focus_window or open_app) and retry",
            win::title(win::root_owner(hwnd)).chars().take(60).collect::<String>()
        ));
    }
    Ok(())
}

/// Key names of a chord, normalised: left/right modifiers folded, sorted, no duplicates.
fn chord_names(keys: &[String]) -> Vec<String> {
    let mut names: Vec<String> = input::key_names(keys)
        .into_iter()
        .map(|n| match n.as_str() {
            "altleft" | "altright" => "alt".to_string(),
            "ctrlleft" | "ctrlright" => "ctrl".to_string(),
            "shiftleft" | "shiftright" => "shift".to_string(),
            "winleft" | "winright" => "win".to_string(),
            _ => n,
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

fn has_all(names: &[String], want: &[&str]) -> bool {
    want.iter().all(|w| names.iter().any(|n| n == w))
}

/// Chords that close or quit the window in front. A chord that merely CONTAINS one counts
/// (ctrl+shift+w, alt+shift+F4, ctrl+alt+w ...), not only the exact sets.
pub fn is_close_chord(keys: &[String]) -> bool {
    let n = chord_names(keys);
    has_all(&n, &["alt", "f4"]) || has_all(&n, &["ctrl", "w"]) || has_all(&n, &["ctrl", "q"]) || has_all(&n, &["ctrl", "f4"])
}

/// Chords that interrupt or end the program in a terminal or open a window menu (close/move/size).
pub fn is_interrupt_chord(keys: &[String]) -> bool {
    let n = chord_names(keys);
    has_all(&n, &["ctrl", "c"]) || has_all(&n, &["ctrl", "d"]) || has_all(&n, &["alt", "space"]) || has_all(&n, &["ctrl", "pause"])
}

/// Defence in depth behind `refuse_host`: a close, quit or interrupt chord is refused outright when the
/// window it would reach is the AI app's own.
pub fn refuse_close_on_host(keys: &[String], hwnd: HWND) -> Result<(), String> {
    if !hwnd.0.is_null() && win::is_host(hwnd) && (is_close_chord(keys) || is_interrupt_chord(keys)) {
        return Err(format!(
            "refusing {}: the window in front is your AI app's own window ({:?}) and a close, quit or interrupt shortcut would end the user's session; bring the target app to the front with focus_window first",
            keys.join("+"),
            win::title(win::root_owner(hwnd)).chars().take(60).collect::<String>()
        ));
    }
    Ok(())
}

/// After an action: the new baseline for "did the user touch the input". A physical touch that
/// arrived during the action itself is a takeover and is remembered.
pub fn reset_baseline() {
    if user_touched() {
        TAKEOVER.store(true, Ordering::Relaxed);
    }
    BASELINE_US.store(hooks::now_us(), Ordering::Relaxed);
}

/// The agent has looked (screenshot, listing) or a new task began: what the user did before that is
/// known to it, and a takeover ends.
pub fn acknowledge() {
    TAKEOVER.store(false, Ordering::Relaxed);
    BASELINE_US.store(hooks::now_us(), Ordering::Relaxed);
}

/// Remember the work window. The AI app's own window counts only when the AI chose it (focus_window, finish).
pub fn note_fg(chosen: bool) {
    let fg = win::foreground();
    if chosen || !win::is_host(fg) {
        *SEEN_FG.lock().unwrap() = Some(fg.0 as isize);
    }
}

pub fn user_touched() -> bool {
    hooks::HOOKED.load(Ordering::Relaxed) && hooks::user_at_us() > BASELINE_US.load(Ordering::Relaxed)
}

pub const TAKEOVER_MESSAGE: &str = "the user used the mouse or keyboard since your last screenshot or listing, so nothing is done until you look again. Stop and ask the user whether to continue; only after they say yes take a fresh screenshot (or list_elements) and carry on";

/// Refuse while the user has the controls: from the first physical touch until a screenshot or listing
/// taken after it (not just once).
pub fn check_takeover() -> Result<(), String> {
    if TAKEOVER.load(Ordering::Relaxed) || user_touched() {
        TAKEOVER.store(true, Ordering::Relaxed);
        return Err(TAKEOVER_MESSAGE.into());
    }
    Ok(())
}

/// Esc (physical, seen by the hook) or the pointer slammed into an outer screen corner.
pub fn check_failsafe() -> Result<(), String> {
    if hooks::STOP.load(Ordering::Relaxed) && !hooks::try_clear_stop() {
        return Err(STOP_MESSAGE.into());
    }
    if crate::mcp::cancelled() {
        return Err(CANCELLED.into());
    }
    let (x, y) = input::cursor_pos();
    let mons = win::monitors();
    for m in &mons {
        let r = m.rect;
        for (cx, cy, dx, dy) in [(r.left, r.top, -1, -1), (r.right(), r.top, 1, -1), (r.left, r.bottom(), -1, 1), (r.right(), r.bottom(), 1, 1)] {
            let outer = !mons.iter().any(|o| o.rect.contains(cx + dx, cy) || o.rect.contains(cx, cy + dy));
            if outer && x == cx && y == cy {
                return Err("the user moved the mouse to a screen corner to stop the AI; stop and ask before continuing".into());
            }
        }
    }
    Ok(())
}

/// The policy for one program (lower-case exe name): the lock screen and security / password-manager
/// apps are never driven, and with `apps.allowed` set only the listed ones are. It fails closed: a
/// program that can't be identified is refused, and so is a Store app whose real program can't be found.
pub fn check_exe(exe: &str, title: &str, allowed: &[String]) -> Result<(), String> {
    // the title of a window that is refused is never quoted: it can name an account, a vault or a document
    if exe.is_empty() {
        return Err("refusing to touch a window whose program can't be identified (access denied or an unreadable path), and unknown programs are not controlled".to_string());
    }
    if exe == win::FRAME_HOST {
        return Err("refusing to touch a Store app window: the app inside it can't be identified".to_string());
    }
    if LOCKED.contains(&exe) {
        return Err("the Windows desktop is locked; ask the user to unlock it".into());
    }
    if FORBIDDEN.contains(&exe) {
        return Err(format!("refusing to control {exe}: a security or password-manager app"));
    }
    if !allowed.is_empty() && !allowed.iter().any(|a| a == exe) {
        return Err(format!("{exe} ({title:?}) is not in config.json apps.allowed {allowed:?}; ask the user to allow it"));
    }
    Ok(())
}

/// The policy for a window, by the program it really belongs to (Store apps resolved through their
/// CoreWindow).
pub fn guard_target(hwnd: HWND) -> Result<(), String> {
    if hwnd.0.is_null() {
        return Ok(());
    }
    let exe = win::app_exe(hwnd);
    let allowed = ALLOWED.lock().unwrap().clone();
    check_exe(&exe, &win::shown_title(hwnd), &allowed)
}

/// An element's own process (UI Automation reports it per element: a control can belong to another process than the
/// window it sits in, as an embedded or hosted window does) must pass the same policy as its window.
pub fn guard_element_process(pid: u32, root: HWND) -> Result<(), String> {
    if pid == 0 || pid == std::process::id() || (!root.0.is_null() && pid == win::pid_of(root)) {
        return Ok(()); // the window itself was checked
    }
    let exe = win::exe_name(pid);
    if exe == win::FRAME_HOST {
        return Ok(()); // the frame of a Store app: its real program was checked through the CoreWindow
    }
    let allowed = ALLOWED.lock().unwrap().clone();
    check_exe(&exe, &win::shown_title(root), &allowed)
}

/// Before an app is launched: the user's stop and the lock screen.
pub fn guard_launch() -> Result<(), String> {
    check_failsafe()?;
    let fg = win::foreground();
    if !fg.0.is_null() && LOCKED.contains(&win::app_exe(fg).as_str()) {
        return Err("the Windows desktop is locked; ask the user to unlock it".into());
    }
    Ok(())
}

/// The window under a screen point must satisfy the same policy as the foreground window (not the AI
/// app's own, not a security app or the lock screen, and in the allow-list when there is one): a click
/// can land in a window other than the foreground one that guard_target checked.
pub fn guard_point(sx: i32, sy: i32) -> Result<(), String> {
    let hit = win::root_window_at(sx, sy);
    if hit.0.is_null() || win::pid_of(hit) == std::process::id() {
        return Ok(());
    }
    refuse_host(hit)?;
    guard_target(hit)?;
    // the deepest window may belong to another process than its root (embedded or hosted windows)
    let leaf = win::window_at(sx, sy);
    if !leaf.0.is_null() && win::pid_of(leaf) != win::pid_of(hit) && win::pid_of(leaf) != std::process::id() {
        let exe = win::exe_name(win::pid_of(leaf));
        if exe != win::FRAME_HOST {
            let allowed = ALLOWED.lock().unwrap().clone();
            check_exe(&exe, &win::shown_title(hit), &allowed)?;
        }
    }
    Ok(())
}

/// A child window (background input): its own process, when different from the top window's, must pass too.
pub fn guard_child(top: HWND, child: HWND) -> Result<(), String> {
    if child.0.is_null() || child == top {
        return Ok(());
    }
    let (cp, tp) = (win::pid_of(child), win::pid_of(top));
    if cp != tp && cp != std::process::id() {
        let exe = win::exe_name(cp);
        if exe != win::FRAME_HOST {
            let allowed = ALLOWED.lock().unwrap().clone();
            check_exe(&exe, &win::shown_title(top), &allowed)?;
        }
    }
    Ok(())
}

/// Checks before every input action: the user hasn't stopped or taken over; the target window
/// is in front (the window of a window screenshot, or the work window when the AI app jumped in
/// front); the target is not the lock screen or a security app.
pub fn keep_work_window() -> Result<(), String> {
    check_failsafe()?;
    check_takeover()?;
    let target = win::VIEW.lock().unwrap().hwnd.map(win::hwnd_from).filter(|h| win::is_window(*h));
    let fg = win::foreground();
    if let Some(t) = target {
        if fg != t && win::pid_of(fg) != win::pid_of(t) && !win::bring_to_front(t) {
            return Err(format!(
                "Windows refused to bring {:?} to the front; use focus_window, then retry (active window: {})",
                win::shown_title(t),
                win::active_title()
            ));
        }
    }
    let fg = win::foreground();
    let seen = SEEN_FG.lock().unwrap().map(win::hwnd_from);
    if let (None, Some(s)) = (target, seen) {
        if fg != s && win::is_window(s) && win::is_host(fg) && !win::is_host(s) && !win::bring_to_front(s) {
            return Err(format!("the AI app's own window came to the front and Windows refused to switch back to {:?}; use focus_window, then retry", win::shown_title(s)));
        }
    }
    let fg = win::foreground();
    refuse_host(fg)?;
    if !win::is_host(fg) {
        crate::session::note_target(fg); // input is about to go to this window
    }
    guard_target(fg)
}

/// The last look right before a button or key goes down (after the pointer glide and after waiting for
/// a busy target): the stop, the user, the window in front and, for a pointer action, the window under
/// the point, all as they are NOW and not as they were when the action began.
pub fn recheck(point: Option<(i32, i32)>) -> Result<(), String> {
    check_failsafe()?;
    check_takeover()?;
    let fg = win::foreground();
    refuse_host(fg)?;
    guard_target(fg)?;
    if let Some((x, y)) = point {
        guard_point(x, y)?;
    }
    Ok(())
}

/// In a window-scoped view: refuse a window that moved or resized since the screenshot, and a
/// point that another app's window now covers.
pub fn assert_over_view(sx: i32, sy: i32, x: f64, y: f64) -> Result<(), String> {
    let v = *win::VIEW.lock().unwrap();
    let Some(target) = v.hwnd.map(win::hwnd_from) else { return Ok(()) };
    if let Some(frame) = v.frame {
        if win::is_window(target) && win::frame_rect(target) != frame {
            return Err("the window moved or resized since the screenshot; take a fresh screenshot before retrying".into());
        }
    }
    let hit = win::root_window_at(sx, sy);
    if !hit.0.is_null() && hit != target {
        let hp = win::pid_of(hit);
        if hp != win::pid_of(target) && hp != unsafe { windows::Win32::System::Threading::GetCurrentProcessId() } {
            return Err(format!(
                "({x}, {y}) is over {:?} (another app), not the window in the screenshot; take a fresh screenshot or use focus_window before retrying",
                win::shown_title(hit)
            ));
        }
    }
    Ok(())
}

/// Checks before a background (posted-message) action: the user's stop and the fail-safe corners,
/// the lock screen, forbidden and not-allowed apps, and a window that moved or resized since the
/// screenshot. It does NOT refuse because the user used the mouse or keyboard (nothing here
/// touches the real input), does not bring the window to the front, and does not mind other
/// windows covering the point (the message goes to the target window itself).
pub fn background_target(target: HWND) -> Result<(), String> {
    check_failsafe()?;
    if !win::is_window(target) {
        return Err("the window of the screenshot is gone; take a fresh screenshot".into());
    }
    guard_target(target)?;
    refuse_host(target)?;
    let v = *win::VIEW.lock().unwrap();
    if let Some(frame) = v.frame {
        if win::frame_rect(target) != frame {
            return Err("the window moved or resized since the screenshot; take a fresh screenshot before retrying".into());
        }
    }
    Ok(())
}

/// Coordinates refer to the latest screenshot; without one there is nothing to refer to.
pub fn require_view() -> Result<(), String> {
    if win::VIEW.lock().unwrap().rect.width <= 1 {
        return Err("no screenshot has been taken yet, so x/y have nothing to refer to; call screenshot first".into());
    }
    Ok(())
}

/// Most child windows looked at under one top-level window, and in all for one picture: a bound, so that a window with
/// thousands of children cannot make a screenshot slow.
const CHILD_WALK_PER_WINDOW: usize = 400;
const CHILD_WALK_TOTAL: usize = 3000;

/// Screen rectangles of the visible child windows of `top` whose process is protected (the lock screen, a security or
/// password-manager app) or can't be identified, when that process is not the top-level window's own: a window of
/// another program embedded in an ordinary one (`SetParent`, a hosted control). `budget` is what is left of the total
/// number of children that may be looked at; Store-app frames are judged through their CoreWindow elsewhere and left alone.
///
/// The bool is true when the walk was cut short by the limit: what is left unseen is unknown, so the caller must fail closed
/// and black out the whole top-level window.
fn embedded_protected_rects(top: HWND, top_pid: u32, by_pid: &mut std::collections::HashMap<u32, String>, budget: &mut usize) -> (Vec<win::Rect>, bool) {
    use windows::core::BOOL;
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetWindow, IsWindowVisible, GW_CHILD};
    struct Walk<'a> {
        top_pid: u32,
        me: u32,
        by_pid: &'a mut std::collections::HashMap<u32, String>,
        left: usize,
        cut: bool,
        out: Vec<win::Rect>,
    }
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let w = &mut *(lp.0 as *mut Walk);
        if w.left == 0 {
            w.cut = true; // another child is there that the limit does not allow to look at
            return BOOL(0);
        }
        w.left -= 1;
        if !IsWindowVisible(h).as_bool() {
            return BOOL(1);
        }
        let pid = win::pid_of(h);
        if pid == 0 || pid == w.top_pid || pid == w.me {
            return BOOL(1);
        }
        let exe = w.by_pid.entry(pid).or_insert_with(|| win::exe_name(pid)).clone();
        if exe != win::FRAME_HOST && (exe.is_empty() || is_protected_exe(&exe)) {
            let r = win::window_rect(h);
            if r.width > 0 && r.height > 0 {
                w.out.push(r);
            }
        }
        BOOL(1)
    }
    let take = child_walk_limit().min(*budget);
    if take == 0 {
        // the total is used up: a window that has children at all cannot be looked at
        let has_children = unsafe { GetWindow(top, GW_CHILD) }.map(|c| !c.0.is_null()).unwrap_or(false);
        return (Vec::new(), has_children);
    }
    let mut walk = Walk { top_pid, me: std::process::id(), by_pid, left: take, cut: false, out: Vec::new() };
    unsafe {
        let _ = EnumChildWindows(Some(top), Some(cb), LPARAM(&mut walk as *mut _ as isize));
    }
    *budget -= take - walk.left;
    (walk.out, walk.cut)
}

/// Children looked at per top-level window (a debug build can lower it with `DC_TEST_CHILD_LIMIT`).
fn child_walk_limit() -> usize {
    #[cfg(debug_assertions)]
    if let Some(n) = std::env::var("DC_TEST_CHILD_LIMIT").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n;
    }
    CHILD_WALK_PER_WINDOW
}

/// Black out, in the picture of one window (`origin` is the screen rectangle the picture shows), the windows of protected
/// or unidentifiable programs embedded in it as child windows.
pub fn mask_embedded(img: &mut image::RgbaImage, hwnd: HWND, origin: win::Rect) {
    let mut by_pid: std::collections::HashMap<u32, String> = Default::default();
    let mut budget = CHILD_WALK_TOTAL;
    let (rects, cut) = embedded_protected_rects(hwnd, win::pid_of(hwnd), &mut by_pid, &mut budget);
    if cut {
        // fail closed: part of the window could not be checked, so none of it is shown
        black_out(img, origin, win::Rect { left: origin.left, top: origin.top, width: img.width() as i32, height: img.height() as i32 });
        return;
    }
    for r in rects {
        black_out(img, origin, r);
    }
}

fn black_out(img: &mut image::RgbaImage, origin: win::Rect, r: win::Rect) {
    let x0 = (r.left - origin.left).clamp(0, img.width() as i32) as u32;
    let y0 = (r.top - origin.top).clamp(0, img.height() as i32) as u32;
    let x1 = (r.left + r.width - origin.left).clamp(0, img.width() as i32) as u32;
    let y1 = (r.top + r.height - origin.top).clamp(0, img.height() as i32) as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            img.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
        }
    }
}

/// Black out, in a picture of a monitor (`monitor` is its screen rectangle), the windows that must
/// never be read: security and password-manager apps, the lock screen and, when asked, the AI app's own.
/// The stacking order is respected: an ordinary window in front of a hidden one stays readable, so
/// the agent can still see what it is working on when it sits on top of the AI app's terminal.
pub fn mask_protected(img: &mut image::RgbaImage, monitor: win::Rect, include_host: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GetWindowTextLengthW, IsIconic, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT};
    struct W {
        r: win::Rect,
        protected: bool,
    }
    let mut by_pid: std::collections::HashMap<u32, String> = Default::default();
    let mut child_budget = CHILD_WALK_TOTAL;
    let me = std::process::id();
    let mut list: Vec<W> = Vec::new(); // front to back
    for h in win::windows_z() {
        if !unsafe { IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() } {
            continue;
        }
        let r = win::frame_rect(h);
        if r.width <= 0 || r.height <= 0 || r.left >= monitor.left + monitor.width || r.top >= monitor.top + monitor.height || r.left + r.width <= monitor.left || r.top + r.height <= monitor.top {
            continue;
        }
        let pid = win::pid_of(h);
        let exe = if win::class_name(h) == "ApplicationFrameWindow" {
            win::app_exe(h)
        } else {
            by_pid.entry(pid).or_insert_with(|| win::exe_name(pid)).clone()
        };
        // an ordinary window that can cover a hidden one: titled, not a tool window, not click-through, not ours
        let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
        let covers = pid != me && unsafe { GetWindowTextLengthW(h) } > 0 && ex & WS_EX_TOOLWINDOW.0 == 0 && ex & WS_EX_TRANSPARENT.0 == 0 && !win::is_cloaked(h);
        // masking fails closed like acting does: a visible window whose program can't be identified, and a Store-app frame whose
        // app (CoreWindow) can't be found, are blacked out too (invisible helper windows are left alone)
        let unknown = covers && (exe.is_empty() || exe == win::FRAME_HOST);
        let protected = is_protected_exe(&exe) || unknown || (include_host && win::is_host(h));
        if protected {
            list.push(W { r, protected: true });
            continue;
        }
        // windows of protected or unidentifiable programs embedded in this one as child windows: blacked out too. They sit
        // in front of this window's own pixels, so they come before its entry in the front-to-back list.
        if pid != me {
            let (kids, cut) = embedded_protected_rects(h, pid, &mut by_pid, &mut child_budget);
            if cut {
                // the walk hit its limit: what is left unseen is unknown, so the whole window is blacked out (fail closed)
                list.push(W { r, protected: true });
                continue;
            }
            for k in kids {
                let (l, t) = (k.left.max(r.left), k.top.max(r.top));
                let (rr, bb) = ((k.left + k.width).min(r.left + r.width), (k.top + k.height).min(r.top + r.height));
                if rr > l && bb > t {
                    list.push(W { r: win::Rect { left: l, top: t, width: rr - l, height: bb - t }, protected: true });
                }
            }
        }
        if covers {
            list.push(W { r, protected: false });
        }
    }
    if !list.iter().any(|w| w.protected) {
        return;
    }
    let orig = img.clone();
    for w in list.iter().rev() {
        // back to front: a hidden window is blacked out, an ordinary one above it gives its pixels back
        let x0 = (w.r.left - monitor.left).clamp(0, img.width() as i32) as u32;
        let y0 = (w.r.top - monitor.top).clamp(0, img.height() as i32) as u32;
        let x1 = (w.r.left + w.r.width - monitor.left).clamp(0, img.width() as i32) as u32;
        let y1 = (w.r.top + w.r.height - monitor.top).clamp(0, img.height() as i32) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let px = if w.protected { image::Rgba([0, 0, 0, 255]) } else { *orig.get_pixel(x, y) };
                img.put_pixel(x, y, px);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn forbidden_locked_and_allowed_policy() {
        for exe in ["keepass.exe", "bitwarden.exe", "1password.exe", "securityhealthui.exe", "sechealthui.exe", "keepassxc.exe"] {
            let e = check_exe(exe, "Vault", &[]).unwrap_err();
            assert!(e.contains("refusing to control"), "{exe}: {e}");
        }
        assert!(check_exe("lockapp.exe", "", &[]).unwrap_err().contains("locked"));
        assert!(check_exe("notepad.exe", "Untitled", &[]).is_ok());
        let allowed = vec!["msedge.exe".to_string()];
        assert!(check_exe("notepad.exe", "Untitled", &allowed).unwrap_err().contains("apps.allowed"));
        assert!(check_exe("msedge.exe", "x", &allowed).is_ok());
        // the forbidden list wins over the allowed list
        assert!(check_exe("keepass.exe", "x", &["keepass.exe".to_string()]).is_err());
    }

    #[test]
    fn unknown_programs_fail_closed() {
        assert!(check_exe("", "mystery", &[]).unwrap_err().contains("can't be identified"));
        assert!(check_exe(win::FRAME_HOST, "Calculator", &[]).unwrap_err().contains("Store app"));
    }

    #[test]
    fn close_chords_are_matched_by_containment() {
        for c in [&["alt+F4"][..], &["alt", "F4"], &["ctrl+w"], &["ctrl+shift+w"], &["ctrl+q"], &["ctrl+F4"], &["alt+shift+F4"], &["ctrl+alt+w"], &["altleft", "f4"], &["ALT+SHIFT+f4"]] {
            assert!(is_close_chord(&k(c)), "{c:?}");
        }
        for c in [&["ctrl+a"][..], &["alt+tab"], &["f4"], &["ctrl+s"], &["w"], &["alt"]] {
            assert!(!is_close_chord(&k(c)), "{c:?}");
        }
        for c in [&["ctrl+c"][..], &["ctrl+d"], &["alt+space"], &["ctrl+shift+c"], &["ctrl+pause"]] {
            assert!(is_interrupt_chord(&k(c)), "{c:?}");
        }
        assert!(!is_interrupt_chord(&k(&["ctrl+v"])));
    }
}
