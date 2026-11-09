//! Low-level keyboard and mouse hooks on their own message-pump thread (the user-interruption
//! monitor). They tell injected input (ours) from physical input (the user's),
//! and a physical Esc while a task is active stops the AI.
//!
//! Windows removes a low-level hook without a word when its callback is too slow, so a watchdog on
//! the same thread notices input that the hooks never saw and installs them again.
//!
//! The stop is the user's, and only the user can lift it: it lapses when (the turn-ended file was touched after
//! it, or a quiet period without any tool call has passed) AND a physical, non-injected key press or click was
//! seen after it, made while the AI app's own window was in front (a key or click in any other app is not the
//! user's answer to the AI), and, on the turn-ended path, made after that touch. The agent can touch the file,
//! wait, or run `turn-ended` itself, but it cannot produce input the hooks see as physical (everything it sends
//! carries the injected flag), so it cannot lift its own stop.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetAncestor, GetForegroundWindow, GA_ROOTOWNER, DispatchMessageW, GetMessageW, SetTimer, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    WM_TIMER,
};

pub static HOOKED: AtomicBool = AtomicBool::new(false);
/// The user pressed Esc; cleared only by `try_clear_stop` / `note_tool_call` / `poll_stop`.
pub static STOP: AtomicBool = AtomicBool::new(false);
pub static ACTIVE: AtomicBool = AtomicBool::new(false); // a task is running (Esc only counts then)
pub static KEYS: AtomicU64 = AtomicU64::new(0);
static USER_AT_US: AtomicU64 = AtomicU64::new(0); // micros since START of the user's last own input
static USER_KEY_AT_US: AtomicU64 = AtomicU64::new(0); // micros since START of the user's last own key press
static USER_MOUSE_AT_US: AtomicU64 = AtomicU64::new(0); // micros since START of the user's last own mouse event (move, click, wheel)
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// When the stop happened (hooks clock and unix milliseconds) and when the last tool call came in.
static STOP_AT_US: AtomicU64 = AtomicU64::new(0);
static STOP_UNIX_MS: AtomicU64 = AtomicU64::new(0);
static LAST_CALL_US: AtomicU64 = AtomicU64::new(0);
/// Unix milliseconds of the latest physical (non-injected) key press or click after the stop (after the grace period,
/// which keeps the stopping Esc and its auto-repeat from counting) made while the AI app's window was in front; 0: none.
static GO_AHEAD_UNIX_MS: AtomicU64 = AtomicU64::new(0);

/// The AI app's windows (root windows), kept up to date outside the hooks (`home::refresh_host_cache`) so that a hook
/// callback can compare the foreground window with them without a lock or a window message. `HOST_N` counts the valid
/// entries; 0 means no AI app window is known at all.
const HOST_MAX: usize = 16;
static HOST_WINDOWS: [AtomicIsize; HOST_MAX] = [const { AtomicIsize::new(0) }; HOST_MAX];
static HOST_N: AtomicUsize = AtomicUsize::new(0);
/// No AI app window is known at all (none found and no fallback window): nothing can be told apart, so any window counts.
/// Only this state fails open; a known AI app whose windows are momentarily hidden never does.
static HOST_FAIL_OPEN: AtomicBool = AtomicBool::new(true);

/// What to do with the cache when a refresh finds `new_len` windows.
#[derive(Debug, PartialEq, Eq)]
pub enum CacheAction {
    /// store the new list
    Replace,
    /// nothing is known (no AI app, no fallback window): empty the cache and fail open
    FailOpen,
    /// the AI app is known but none of its windows could be listed this time (hidden, cloaked, on another virtual desktop):
    /// keep what was cached
    Keep,
    /// as Keep, but there is nothing to keep: stay empty and fail closed
    Close,
}

/// Pure rule for a refresh of the host window cache.
pub fn cache_action(new_len: usize, nothing_known: bool, have_old: bool) -> CacheAction {
    match (new_len > 0, nothing_known, have_old) {
        (true, _, _) => CacheAction::Replace,
        (false, true, _) => CacheAction::FailOpen,
        (false, false, true) => CacheAction::Keep,
        (false, false, false) => CacheAction::Close,
    }
}

/// Refresh the cached AI app windows (called from the overlay's poll and at task start, never from a hook).
/// `nothing_known`: no AI app was identified and there is no fallback window.
pub fn set_host_windows(list: &[isize], nothing_known: bool) {
    let n = list.len().min(HOST_MAX);
    match cache_action(list.len(), nothing_known, HOST_N.load(Ordering::Relaxed) > 0) {
        CacheAction::Replace => {
            for (slot, h) in HOST_WINDOWS.iter().zip(list.iter().take(n)) {
                slot.store(*h, Ordering::Relaxed);
            }
            HOST_N.store(n, Ordering::Relaxed);
            HOST_FAIL_OPEN.store(false, Ordering::Relaxed);
        }
        CacheAction::FailOpen => {
            HOST_N.store(0, Ordering::Relaxed);
            HOST_FAIL_OPEN.store(true, Ordering::Relaxed);
        }
        CacheAction::Keep => {}
        CacheAction::Close => HOST_FAIL_OPEN.store(false, Ordering::Relaxed),
    }
}

/// Pure rule: is `fg` (a window, or its root owner `root`) one of the cached AI app windows? With no cached window,
/// `fail_open` says whether anything counts (nothing known about the AI app) or nothing does.
pub fn is_cached_host(cache: &[isize], fg: isize, root: isize, fail_open: bool) -> bool {
    if cache.is_empty() {
        return fail_open;
    }
    fg != 0 && (cache.contains(&fg) || cache.contains(&root))
}

/// Pure rule: a point is inside a window rectangle (right and bottom edges excluded, as Windows does).
pub fn point_in_rect(x: i32, y: i32, l: i32, t: i32, r: i32, b: i32) -> bool {
    x >= l && x < r && y >= t && y < b
}

/// Keys that never count as the user's answer: the modifiers and the Windows keys (they start Alt+Tab, Win+anything, menus).
pub fn is_modifier_vk(vk: u32) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

/// Debug builds only: lets a test say which window counts as the foreground for one simulated key.
#[cfg(debug_assertions)]
static DEBUG_FG: AtomicIsize = AtomicIsize::new(0);
/// Debug builds only: pretend Alt or Win is held for one simulated key.
#[cfg(debug_assertions)]
static DEBUG_MODS: AtomicBool = AtomicBool::new(false);

/// Is Alt or a Windows key held? (`GetAsyncKeyState`: lock-free, no message.) Alt+Tab, Win+R and the like are not answers.
fn alt_or_win_held() -> bool {
    #[cfg(debug_assertions)]
    if DEBUG_MODS.load(Ordering::Relaxed) {
        return true;
    }
    unsafe { [0x12, 0x5B, 0x5C].iter().any(|&vk| (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk) as u16) & 0x8000 != 0) }
}

/// Is the window in front the AI app's? Lock-free: a few atomic loads and `GetForegroundWindow` / `GetAncestor`.
/// For a click, `click` is the point of the click (`MSLLHOOKSTRUCT.pt`): it must also lie inside the window in front (or its
/// root window), because the foreground window at button-down is the one being left, not the one clicked into.
fn foreground_is_host(click: Option<(i32, i32)>) -> bool {
    let n = HOST_N.load(Ordering::Relaxed);
    if n == 0 {
        return HOST_FAIL_OPEN.load(Ordering::Relaxed);
    }
    #[cfg(debug_assertions)]
    let dbg = DEBUG_FG.load(Ordering::Relaxed);
    #[cfg(not(debug_assertions))]
    let dbg = 0isize;
    let (fg, root) = unsafe {
        let fg = if dbg != 0 { windows::Win32::Foundation::HWND(dbg as *mut _) } else { GetForegroundWindow() };
        let r = GetAncestor(fg, GA_ROOTOWNER);
        (fg.0 as isize, if r.0.is_null() { fg.0 as isize } else { r.0 as isize })
    };
    let mut cache = [0isize; HOST_MAX];
    let n = n.min(HOST_MAX);
    for (c, slot) in cache.iter_mut().zip(HOST_WINDOWS.iter()).take(n) {
        *c = slot.load(Ordering::Relaxed);
    }
    if !is_cached_host(&cache[..n], fg, root, false) {
        return false;
    }
    let Some((x, y)) = click else { return true };
    unsafe {
        [fg, root].iter().any(|&h| {
            let mut r = windows::Win32::Foundation::RECT::default();
            windows::Win32::UI::WindowsAndMessaging::GetWindowRect(windows::Win32::Foundation::HWND(h as *mut _), &mut r).is_ok()
                && point_in_rect(x, y, r.left, r.top, r.right, r.bottom)
        })
    }
}

/// Input within this long of the stop (micros) does not count as the user's go-ahead: the Esc that stopped the
/// agent, its release and its auto-repeat.
pub const STOP_GRACE_US: u64 = 300_000;

/// No tool call for this long after a stop (micros): the agent has really stopped, so the stop lapses.
pub const STOP_QUIET_US: u64 = 30_000_000;

// hook bookkeeping (the watchdog)
static KB_HOOK: AtomicIsize = AtomicIsize::new(0);
static MS_HOOK: AtomicIsize = AtomicIsize::new(0);
static LAST_HOOK_TICK: AtomicU32 = AtomicU32::new(0);
pub static REINSTALLS: AtomicU32 = AtomicU32::new(0);

pub fn now_us() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The user pressed a physical key within the last `ms` milliseconds (typing: never steal focus).
pub fn user_typing_within(ms: u64) -> bool {
    let at = USER_KEY_AT_US.load(Ordering::Relaxed);
    at != 0 && now_us().saturating_sub(at) < ms * 1000
}

pub fn user_mouse_at_us() -> u64 {
    USER_MOUSE_AT_US.load(Ordering::Relaxed)
}

/// The user used the mouse physically within the last `ms` milliseconds.
pub fn user_mouse_within(ms: u64) -> bool {
    let at = USER_MOUSE_AT_US.load(Ordering::Relaxed);
    at != 0 && now_us().saturating_sub(at) < ms * 1000
}

pub fn user_at_us() -> u64 {
    USER_AT_US.load(Ordering::Relaxed)
}

// ---------- the stop ----------

/// The user stopped the agent (physical Esc, or the pointer slammed into a corner).
pub fn raise_stop() {
    STOP_AT_US.store(now_us(), Ordering::Relaxed);
    STOP_UNIX_MS.store(unix_ms(), Ordering::Relaxed);
    GO_AHEAD_UNIX_MS.store(0, Ordering::Relaxed);
    STOP.store(true, Ordering::Relaxed);
}

/// Pure rule: does a physical input at `at` (hooks clock) count as the user's go-ahead for a stop raised at `stop_at`?
/// Only a key press or button press, not Escape itself (a held Esc repeats), and not within the grace period.
pub fn counts_as_go_ahead(at: u64, stop_at: u64, is_press: bool, vk: u32) -> bool {
    is_press && vk != 0x1B && !is_modifier_vk(vk) && at >= stop_at.saturating_add(STOP_GRACE_US)
}

/// Called by the hooks for every PHYSICAL (non-injected) event: remembers a go-ahead after the stop, when the AI app's
/// window is in front (the checks run in order of cost, and only while a stop holds).
fn physical_input(is_press: bool, vk: u32, click: Option<(i32, i32)>) {
    if !STOP.load(Ordering::Relaxed) || !counts_as_go_ahead(now_us(), STOP_AT_US.load(Ordering::Relaxed), is_press, vk) {
        return;
    }
    // a key pressed while Alt or Win is down (Alt+Tab, Win+R ...) is not an answer to the AI
    if click.is_none() && alt_or_win_held() {
        return;
    }
    if foreground_is_host(click) {
        GO_AHEAD_UNIX_MS.store(unix_ms().max(1), Ordering::Relaxed);
    }
}

/// Pure rule: the stop lifts when the user's go-ahead (a physical key press or click in the AI app, at `go_ahead_ms`, 0 = none)
/// has come AND the agent's turn is over: the turn-ended file was touched at `turn_ended_ms` and the go-ahead is later than
/// that touch (a key pressed before the agent's own `turn-ended` says nothing about the user's wish), or the quiet period passed.
pub fn may_lift(turn_ended_ms: Option<u64>, quiet: bool, go_ahead_ms: u64) -> bool {
    go_ahead_ms != 0 && (quiet || turn_ended_ms.map(|t| go_ahead_ms > t).unwrap_or(false))
}

/// Pure rule: a stop lapses when the agent made no tool call for the quiet period after it
/// (the quiet period starts at the stop or at the last call, whichever is later).
pub fn quiet_since_stop(now: u64, stop_at: u64, last_call: u64) -> bool {
    now.saturating_sub(stop_at.max(last_call)) >= STOP_QUIET_US
}

fn lapse(why: &str) {
    if STOP.swap(false, Ordering::Relaxed) {
        eprintln!("stop cleared: {why}");
    }
}

/// Lifts the stop when the rule allows it; true when no stop holds any more. `prev_call` is the time of the call
/// before the current one (the quiet period is measured up to the current call), None: the latest call.
fn evaluate(prev_call: Option<u64>) -> bool {
    if !STOP.load(Ordering::Relaxed) {
        return true;
    }
    let now = now_us();
    let stop_at = STOP_AT_US.load(Ordering::Relaxed);
    let last = prev_call.unwrap_or_else(|| LAST_CALL_US.load(Ordering::Relaxed));
    let quiet = quiet_since_stop(now, stop_at, last);
    let ended_ms = crate::session::turn_ended_at_ms(STOP_UNIX_MS.load(Ordering::Relaxed));
    let go_ahead = GO_AHEAD_UNIX_MS.load(Ordering::Relaxed);
    if may_lift(ended_ms, quiet, go_ahead) {
        lapse(if ended_ms.is_some_and(|t| go_ahead > t) { "the turn ended and the user was seen in the AI app" } else { "30 s without a tool call and the user was seen in the AI app" });
        return true;
    }
    false
}

/// Every tool call passes here first (`finish` included, which therefore never clears a stop by itself).
pub fn note_tool_call() {
    let now = now_us();
    let prev = LAST_CALL_US.swap(now, Ordering::Relaxed);
    let _ = evaluate(Some(prev));
}

/// True when the stop no longer holds.
pub fn try_clear_stop() -> bool {
    evaluate(None)
}

/// Housekeeping from the overlay's poll: a stop whose conditions are met lapses even when no call arrives.
pub fn poll_stop() {
    let _ = evaluate(None);
    // keep the hooks' cache of the AI app's windows fresh while a task runs or a stop holds (at most once a second)
    static LAST_REFRESH_US: AtomicU64 = AtomicU64::new(0);
    if ACTIVE.load(Ordering::Relaxed) || STOP.load(Ordering::Relaxed) {
        let now = now_us();
        let last = LAST_REFRESH_US.load(Ordering::Relaxed);
        if last == 0 || now.saturating_sub(last) >= 1_000_000 {
            LAST_REFRESH_US.store(now, Ordering::Relaxed);
            crate::home::refresh_host_cache();
        }
    }
}

// ---------- the hooks ----------

fn tick() -> u32 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
}

unsafe extern "system" fn on_key(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 {
        LAST_HOOK_TICK.store(tick(), Ordering::Relaxed);
        let key = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let down = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
        if down {
            KEYS.fetch_add(1, Ordering::Relaxed);
        }
        if key.flags.0 & LLKHF_INJECTED.0 == 0 {
            USER_AT_US.store(now_us(), Ordering::Relaxed);
            if down {
                USER_KEY_AT_US.store(now_us(), Ordering::Relaxed);
            }
            if down && key.vkCode == 0x1B && ACTIVE.load(Ordering::Relaxed) {
                raise_stop();
            } else {
                physical_input(down, key.vkCode, None);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn on_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 {
        LAST_HOOK_TICK.store(tick(), Ordering::Relaxed);
        let m = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        if m.flags & LLMHF_INJECTED == 0 {
            USER_AT_US.store(now_us(), Ordering::Relaxed);
            USER_MOUSE_AT_US.store(now_us(), Ordering::Relaxed);
            // a click is a go-ahead; mere movement and the wheel are not
            let press = matches!(wparam.0 as u32, 0x0201 | 0x0204 | 0x0207 | 0x020B); // WM_L/R/M/XBUTTONDOWN
            physical_input(press, 0, Some((m.pt.x, m.pt.y)));
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe fn install() {
    let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), None, 0);
    let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(on_mouse), None, 0);
    KB_HOOK.store(kb.as_ref().map(|h| h.0 as isize).unwrap_or(0), Ordering::Relaxed);
    MS_HOOK.store(ms.as_ref().map(|h| h.0 as isize).unwrap_or(0), Ordering::Relaxed);
    HOOKED.store(kb.is_ok() && ms.is_ok(), Ordering::Relaxed);
    LAST_HOOK_TICK.store(tick(), Ordering::Relaxed);
}

unsafe fn uninstall() {
    for slot in [&KB_HOOK, &MS_HOOK] {
        let h = slot.swap(0, Ordering::Relaxed);
        if h != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut _));
        }
    }
}

/// Input the system registered (GetLastInputInfo counts physical and injected input) that no hook
/// callback saw for more than 2 s means Windows dropped the hooks: put them back.
unsafe fn watchdog() {
    #[cfg(debug_assertions)]
    debug_triggers();
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if !GetLastInputInfo(&mut info).as_bool() {
        return;
    }
    let behind = info.dwTime.wrapping_sub(LAST_HOOK_TICK.load(Ordering::Relaxed)) as i32;
    if behind > 2000 || KB_HOOK.load(Ordering::Relaxed) == 0 || MS_HOOK.load(Ordering::Relaxed) == 0 {
        uninstall();
        install();
        if HOOKED.load(Ordering::Relaxed) {
            let n = REINSTALLS.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!("hooks: the low-level hooks had gone quiet while input arrived; installed again (#{n})");
        }
    }
}

/// Debug builds only (never compiled into the release exe): files named by environment variables
/// let a test raise the Esc stop and drop the hooks, which a test cannot do with real input.
#[cfg(debug_assertions)]
unsafe fn debug_triggers() {
    if let Some(p) = std::env::var_os("DC_TEST_STOP_FILE") {
        if std::fs::remove_file(&p).is_ok() {
            raise_stop();
        }
    }
    if let Some(p) = std::env::var_os("DC_TEST_PHYSICAL_FILE") {
        // the file's text, space separated: a window handle (decimal) that counts as the foreground window for this
        // input (none: the real one), then optionally `vk=<number>` (the key, default 0x41), `click=<x>,<y>` (a click
        // there instead of a key) and `mods=1` (Alt or Win held)
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        if std::fs::remove_file(&p).is_ok() {
            let (mut fg, mut vk, mut click, mut mods) = (0isize, 0x41u32, None, false);
            for tok in text.split_whitespace() {
                if let Some(v) = tok.strip_prefix("vk=") {
                    vk = v.parse().unwrap_or(0x41);
                } else if let Some(c) = tok.strip_prefix("click=") {
                    let mut it = c.split(',').filter_map(|n| n.parse::<i32>().ok());
                    click = it.next().zip(it.next());
                } else if tok == "mods=1" {
                    mods = true;
                } else if let Ok(h) = tok.parse::<isize>() {
                    fg = h;
                }
            }
            DEBUG_FG.store(fg, Ordering::Relaxed);
            DEBUG_MODS.store(mods, Ordering::Relaxed);
            // as if the user had pressed that key or clicked there: the same function the hooks call
            if click.is_some() { physical_input(true, 0, click) } else { physical_input(true, vk, None) }
            DEBUG_FG.store(0, Ordering::Relaxed);
            DEBUG_MODS.store(false, Ordering::Relaxed);
        }
    }
    if let Some(p) = std::env::var_os("DC_TEST_TOUCH_FILE") {
        if std::fs::remove_file(&p).is_ok() {
            USER_AT_US.store(now_us(), Ordering::Relaxed); // as if the user had used the mouse just now
        }
    }
    if let Some(p) = std::env::var_os("DC_TEST_DROP_HOOKS_FILE") {
        if std::fs::remove_file(&p).is_ok() {
            // what Windows does to a slow hook: the handles go away, nobody is told
            for slot in [&KB_HOOK, &MS_HOOK] {
                let h = slot.load(Ordering::Relaxed);
                if h != 0 {
                    let _ = UnhookWindowsHookEx(HHOOK(h as *mut _));
                }
            }
        }
    }
}

pub fn start() {
    let _ = START.get_or_init(Instant::now);
    std::thread::Builder::new()
        .name("hooks".into())
        .spawn(|| unsafe {
            install();
            SetTimer(None, 0, 1000, None);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_TIMER {
                    watchdog();
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .expect("hook thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_lapses_only_after_a_quiet_period_that_starts_at_the_stop_or_the_last_call() {
        let s = 1_000_000_000u64;
        assert!(!quiet_since_stop(s + 29_000_000, s, 0)); // 29 s after the stop: still stopped
        assert!(quiet_since_stop(s + 30_000_000, s, 0)); // 30 s without any call: lapses
        assert!(!quiet_since_stop(s + 1_000_000, s, s - 300_000_000)); // a call long before the stop is no quiet time
        assert!(!quiet_since_stop(s + 35_000_000, s, s + 10_000_000)); // a call at +10 s restarts the clock
        assert!(quiet_since_stop(s + 40_000_000, s, s + 10_000_000));
        assert!(!quiet_since_stop(s + 100_000_000, s, s + 85_000_000)); // calling every 20 s is never quiet
    }

    #[test]
    fn a_stop_lifts_only_with_a_turn_end_or_quiet_period_and_a_physical_go_ahead() {
        assert!(!may_lift(Some(100), true, 0)); // the agent touched the file / waited: not enough alone
        assert!(!may_lift(None, false, 500)); // the user is back but the turn is still running
        assert!(may_lift(Some(100), false, 500)); // the key came after the turn-ended touch
        assert!(!may_lift(Some(500), false, 100)); // the key came before the agent's touch: it does not count
        assert!(!may_lift(Some(500), false, 500)); // not strictly later
        assert!(may_lift(None, true, 500)); // quiet period: any go-ahead after the stop
        assert!(!may_lift(None, false, 0));
    }

    #[test]
    fn only_input_while_the_ai_apps_window_is_in_front_counts() {
        let cache = [111isize, 222];
        assert!(is_cached_host(&cache, 111, 111, false)); // the AI app's window itself
        assert!(is_cached_host(&cache, 999, 222, false)); // a dialog or child whose root owner is the AI app's window
        assert!(!is_cached_host(&cache, 333, 333, false)); // any other app
        assert!(!is_cached_host(&cache, 0, 0, false)); // nothing in front
        assert!(is_cached_host(&[], 333, 333, true)); // nothing known at all: cannot tell, so it counts
        assert!(!is_cached_host(&[], 333, 333, false)); // a known AI app with no listed window: nothing counts
    }

    #[test]
    fn a_click_counts_only_inside_the_window_in_front() {
        assert!(point_in_rect(100, 100, 0, 0, 200, 200));
        assert!(point_in_rect(0, 0, 0, 0, 200, 200));
        assert!(!point_in_rect(200, 100, 0, 0, 200, 200)); // right edge is outside
        assert!(!point_in_rect(-5, 100, 0, 0, 200, 200)); // the click into another app on the left
        assert!(point_in_rect(-50, 10, -100, 0, 0, 100)); // a second monitor on the left
    }

    #[test]
    fn the_host_cache_is_kept_when_the_ai_app_is_known_but_none_of_its_windows_can_be_listed() {
        assert_eq!(cache_action(2, false, true), CacheAction::Replace);
        assert_eq!(cache_action(0, false, true), CacheAction::Keep); // hidden, cloaked, another virtual desktop
        assert_eq!(cache_action(0, false, false), CacheAction::Close); // nothing to keep: fail closed
        assert_eq!(cache_action(0, true, true), CacheAction::FailOpen); // no AI app and no fallback window at all
        assert_eq!(cache_action(0, true, false), CacheAction::FailOpen);
    }

    #[test]
    fn the_stopping_escape_and_input_right_after_it_is_not_a_go_ahead() {
        let s = 5_000_000u64;
        assert!(!counts_as_go_ahead(s + 10_000, s, false, 0x1B)); // the Esc key-up
        assert!(!counts_as_go_ahead(s + 100_000, s, true, 0x41)); // a key within the grace period
        assert!(!counts_as_go_ahead(s + 400_000, s, true, 0x1B)); // Esc auto-repeat, even later
        assert!(!counts_as_go_ahead(s + 400_000, s, false, 0x41)); // a key release is not a press
        assert!(counts_as_go_ahead(s + 300_000, s, true, 0x41)); // a key press 300 ms later
        assert!(counts_as_go_ahead(s + 2_000_000, s, true, 0)); // a click
        for vk in [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5] {
            assert!(!counts_as_go_ahead(s + 2_000_000, s, true, vk), "modifier {vk:#x}"); // Shift, Ctrl, Alt, Win
        }
        assert!(counts_as_go_ahead(s + 2_000_000, s, true, 0x09)); // Tab itself is a key (Alt+Tab is caught by the held Alt)
        assert!(counts_as_go_ahead(s + 2_000_000, s, true, 0x0D));
    }
}
