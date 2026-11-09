# Security review of v0.3.1 (HEAD 975a4fc), 2026-11-09

Independent static review (Opus). **Verdict: publish after fixes.** This file is the fix list for v0.3.2.

## High (all must be fixed)
- **H1. Stale `list_elements` ids act on forbidden apps.**
  - Where: `uia.rs:62/468/666-680/726-742/801-839`. `ELEMENTS` is never cleared and `resolve()` doesn't check the element's window.
  - Fix:
    - Store each element's root hwnd.
    - In `resolve`, run `guard_target` + `refuse_host` on it and require it to equal `root_hwnd()`.
    - Clear `ELEMENTS` on view change and at task end.
- **H2. Reading forbidden apps isn't guarded.**
  - Where: `screenshot(window=)` → `capture_window_view` (`tools.rs:213-260`), which also SW_RESTOREs; `list_elements`/`read_text` (`uia.rs:136-143, 288-294, 765`) only refuse the host. Monitor shots don't mask forbidden windows.
  - Fix:
    - Run `guard_target` in `capture_window_view`, `root()` and `focused()`.
    - Black out forbidden windows in monitor shots, like `mask_host_windows` (`finder.rs:322`).
- **H3. Windows Security and all UWP apps aren't resolved.**
  - Where: `guards.rs:20-23` lists `securityhealthui.exe` (the app is `SecHealthUI.exe`). UWP top-level windows belong to `ApplicationFrameHost.exe`.
  - Fix:
    - For `ApplicationFrameWindow`, resolve the child `Windows.UI.Core.CoreWindow` pid/exe.
    - Add `sechealthui.exe`.
    - Apply this to `guard_target`, `guard_point`, `background::child_at` and `apps.allowed`.
- **H4. The agent can clear the Esc stop by calling `finish`.**
  - Where: `tools.rs:707-711` → `session.rs:126`; `instructions.txt:12`.
  - Fix:
    - Clear STOP only on a user-side signal: the turn-ended file touched after the stop, or a quiet period (no tool calls) of ≥ 30 s.
    - Never clear it in `finish`.
    - Reword the instruction.
- **H5. The host-window opt-in, and close chords bypassed.**
  - Where: `focus_window` on the host calls `choose_host` (`actions.rs:256-258`), after which `refuse_host` allows everything. `is_close_chord` (`guards.rs:63-77`) matches exact sets only.
  - Fix:
    - Remove the opt-in: the host never gets input.
    - Defence in depth: refuse any chord whose key set contains a close chord, and refuse ctrl+c / ctrl+d / alt+space to the host.
- **H6. `open_app` has no guards and executes any string.**
  - Where: `actions.rs:292-315`, `apps.rs:66-72`.
  - Fix:
    - Run `check_failsafe` + the lock check first.
    - Allow installed-app ids (AppsFolder), and `.exe` under Program Files / Program Files (x86) / %LOCALAPPDATA%\Programs / Windows\System32 / WindowsApps.
    - Refuse URLs, `file:`, UNC, `.lnk/.bat/.cmd/.ps1/.vbs/.js/.hta/.msi/.reg/.scr/.com`, and anything under Downloads/Temp, with an error telling the model to ask the user.
    - Apply `apps.allowed`.
    - Start-search fallback: verify that Start/Search is in the foreground before typing, otherwise fail.
- **H7. `type_text` is uninterruptible, and partial failures retype everything.**
  - Where: `actions.rs:97-124`, `input.rs:441-571`.
  - Fix:
    - Before each line or chunk, run `check_failsafe` + cancellation + `user_touched` + "foreground == the window checked at start".
    - Restore the clipboard on every exit path.
    - Report how many lines were typed and never retype.

## Medium
- **M1. Check-then-act gaps.** Re-run `check_failsafe` + `guard_point` / the foreground check right before button-down / key-down (after the glide and after `wait_target_responsive`). Same for drag.
- **M2. `apps.allowed` gaps; checks fail open.**
  - Use `check_exe` (allow-list included) in `guard_point`, `open_app` and `focus_window`.
  - Fail closed when the exe is unknown (OpenProcess failure or long path).
  - `homeApp` mustn't exempt a window from `apps.allowed` except for host refusal.
- **M3. The user-takeover guard warns only once.** Keep refusing until a screenshot or list taken after the touch. The instructions should say: stop and ask the user.
- **M4. Panics.**
  - Clamp negative wait / timeout values (`actions.rs:137, 220, 240`).
  - Add `catch_unwind` in `Registry::call`.
  - Use an input guard that releases modifiers and buttons on drop.
  - Don't hold Ctrl during the paste settle.
- **M5. Clipboard.**
  - Restore after each line.
  - Use Unicode keystrokes (not paste) for password fields (UIA IsPassword) and when the clipboard holds password-manager-marked content.
  - Report when a restore was skipped because another app changed the clipboard.
- **M6. Host detection fails open.**
  - Treat the layer-3 fallback window as host.
  - Warn in results when no host was resolved.
  - Re-validate chain pids (start time / still running) instead of keeping them for the process lifetime.

## Low
- **L1. README Stop hook.** Implement a `turn-ended` CLI subcommand: touch the turn-ended file next to the exe and exit at once, with no hooks or overlay. Fix the README's exe name.
- **L2. Silently removed low-level hooks.** Detect them (e.g. physical input seen by GetLastInputInfo but no hook callback for > 2 s), re-install, and report it in `status`.
- **L3. Caps.**
  - Cap the stdin line length (e.g. 32 MB, then reply with an error).
  - Cap `type_text` length (e.g. 1 MB).
  - Cap capture size.
- **L4.** `set_clipboard` must null-check `GlobalLock`.
- **L5.** Don't write the `finish` summary to stderr.
- **L6. `instructions.txt`.**
  - Extend injection guidance to window titles, OCR/UIA text, notifications and file names.
  - Never open URLs, paths or commands seen on screen.
  - Never type into its own app.
  - Fix the "finish unblocks Esc" and "continue" wording.
  - Stay under 4096 characters.

## Runtime checks the review couldn't do statically
- Is SecHealthUI.exe under ApplicationFrameHost on this build?
- Does Alt+Shift+F4 close a window?
- Does VK_PACKET U+0003 act as Ctrl+C in Windows Terminal (test on a fake host only)?
- When does Windows remove a low-level hook?
- Dependency advisories: run `cargo audit` if it can be installed into devtools.

Confirmed fine:
- The Rhai sandbox has no escape.
- FFI sizes and handles are correct.
- No network, no file writes, and the config is read-only.
- The regex is linear-time.

---

## Status in v0.3.2

**Amended after the v0.3.2 re-check:** H4, H6, M3 and M5 were only partly closed in v0.3.2; the rest is tracked and fixed through `SECURITY-REVIEW-0.3.2.md` (H4 -> N2, H6 -> N1 and L3, M3 -> N3 and L1/L2/L4, M5 -> N4; see its "Status in v0.3.3"). The text below is what v0.3.2 claimed at the time.

All of H1-H7, M1-M6 and L1-L6 are addressed. Regression tests: `tests/security_test.py` (H1, H2, H3, H4, H6, H7, M2, M3, M4, M5, M6, L1, L2, L3, L5), `tests/host_guard_test.py` (H5) and the unit tests in `src/` (chords, `open_app` refusals, the stop rule, the bounded reader).

Runtime checks, on Windows 11 22631:
- Windows Security is a Store app: its top-level window is an `ApplicationFrameWindow` owned by `ApplicationFrameHost.exe`; the app is the `Windows.UI.Core.CoreWindow` child, whose process is `...\WindowsApps\Microsoft.SecHealthUI_...\SecHealthUI.exe` (so `sechealthui.exe`). Calculator is the same shape (`CalculatorApp.exe`).
- Alt+Shift+F4 closes a window (a WinForms test window: FormClosing fired and the window went away).
- A `VK_PACKET` U+0003 reaches a Windows Terminal console as the character U+0003, the same byte a raw-mode program receives for Ctrl+C; a program in cooked mode with a SIGINT handler got the character rather than the signal. Typing now drops control characters other than tab and line breaks, so no text can carry it.
- Windows removes a low-level hook silently when its callback exceeds the system's `LowLevelHooksTimeout`. That was not measured here; the watchdog re-installs the hooks when input arrives that no hook callback saw (tested by dropping the hooks in a debug build).
- `cargo audit`: no vulnerabilities in 83 dependencies; one warning, `smartstring` 1.0.1 unmaintained (RUSTSEC-2026-0249), pulled in by `rhai`.
