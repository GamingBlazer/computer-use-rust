//! Low-level keyboard and mouse hooks on their own message-pump thread (the user-interruption
//! monitor). They tell injected input (ours) from physical input (the user's),
//! and a physical Esc while a task is active stops the AI.
//!
//! Windows removes a low-level hook without a word when its callback is too slow, so a watchdog on
//! the same thread notices input that the hooks never saw and installs them again.
//!
//! The stop is the user's, and only the user can lift it: it lapses when (the turn-ended file was touched after
//! it, or a quiet period without any tool call has passed) AND a physical, non-injected key press or click was
//! seen after it. The agent can touch the file, wait, or run `turn-ended` itself, but it cannot produce input
//! the hooks see as physical (everything it sends carries the injected flag), so it cannot lift its own stop.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetTimer, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK,
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
/// A physical (non-injected) key press or click was seen after the stop (and after the grace period, which keeps
/// the stopping Esc and its auto-repeat from counting).
static PHYSICAL_AFTER_STOP: AtomicBool = AtomicBool::new(false);

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
    PHYSICAL_AFTER_STOP.store(false, Ordering::Relaxed);
    STOP.store(true, Ordering::Relaxed);
}

/// Pure rule: does a physical input at `at` (hooks clock) count as the user's go-ahead for a stop raised at `stop_at`?
/// Only a key press or button press, not Escape itself (a held Esc repeats), and not within the grace period.
pub fn counts_as_go_ahead(at: u64, stop_at: u64, is_press: bool, vk: u32) -> bool {
    is_press && vk != 0x1B && at >= stop_at.saturating_add(STOP_GRACE_US)
}

/// Called by the hooks for every PHYSICAL (non-injected) event: remembers a go-ahead after the stop.
fn physical_input(is_press: bool, vk: u32) {
    if STOP.load(Ordering::Relaxed) && counts_as_go_ahead(now_us(), STOP_AT_US.load(Ordering::Relaxed), is_press, vk) {
        PHYSICAL_AFTER_STOP.store(true, Ordering::Relaxed);
    }
}

/// Pure rule: the stop lifts when the agent's turn is over (turn-ended touched, or 30 s quiet) AND the user was seen.
pub fn may_lift(turn_over: bool, quiet: bool, user_seen: bool) -> bool {
    (turn_over || quiet) && user_seen
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
    let turn_over = crate::session::turn_ended_since_ms(STOP_UNIX_MS.load(Ordering::Relaxed));
    if may_lift(turn_over, quiet, PHYSICAL_AFTER_STOP.load(Ordering::Relaxed)) {
        lapse(if turn_over { "the turn ended and the user was seen" } else { "30 s without a tool call and the user was seen" });
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
                physical_input(down, key.vkCode);
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
            physical_input(press, 0);
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
        if std::fs::remove_file(&p).is_ok() {
            physical_input(true, 0x41); // as if the user had pressed a key: the same function the hooks call
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
        assert!(!may_lift(true, true, false)); // the agent touched the file / waited: not enough alone
        assert!(!may_lift(false, false, true)); // the user is back but the turn is still running
        assert!(may_lift(true, false, true));
        assert!(may_lift(false, true, true));
        assert!(!may_lift(false, false, false));
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
    }
}
