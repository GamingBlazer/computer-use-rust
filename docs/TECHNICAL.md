# Technical reference: desktop-control MCP server for Windows

> Pre-release notes. The program is still `desktop-control.exe`; it will be renamed `computer-use.exe` for v1.0.
> The landing page is [../README.md](../README.md).

A native Rust MCP server that lets an AI client see and drive the Windows desktop:
- **Seeing:** window and monitor screenshots (Windows.Graphics.Capture, so covered and GPU-rendered windows work), UI Automation control trees and on-screen text recognition (Windows OCR).
- **Acting:** clicks, typing and app launching.
- **Indicator:** an on-screen status pill, edge glows and a click ripple, all GPU-animated and coloured per client.
- **Safety guards:** physical Esc stops the agent, user input is detected, the AI app's own window and password managers and the lock screen are never driven.

One file (about 7 MB), no runtime needed, 22 tools.

## Install

1. Build it (see [../BUILD.md](../BUILD.md)) or download a release, and put `desktop-control.exe` somewhere permanent.
2. Register it with your MCP client, e.g. Claude Code:

       claude mcp add --scope user computer-use -- "C:\Tools\computer-use\desktop-control.exe"

3. Restart the client and ask for something on screen, e.g. "Open Notepad and type hello".

## Tools

`screenshot` (monitor or one window, optionally with the control tree), `list_elements`,
`click_element`, `click`, `drag`, `scroll`, `type_text`, `set_value`, `read_text`,
`element_action`, `press_keys`, `list_apps`, `open_app`, `focus_window`, `move_window`,
`run_actions` (batch), `status`, `finish`, and, in the Rust build, `find_text`, `click_text`, `find`
and `run_script` (see "Any application" below).

### What the tools return (text-first)

Action tools (`click`, `drag`, `scroll`, `type_text`, `press_keys`, `focus_window`, `move_window`,
`open_app`, `click_element`, `set_value`, `element_action`, `run_actions`) take
`observe: "text" | "screenshot" | "both" | "none"`. The default is `text`: a short summary taken
after the action settles (active window, focused control, and a before/after diff of the window's
interactive controls, at most ~15 lines). `screenshot_after: true/false` still works (both/text).
Other options:

- `screenshot`: `size: "normal" | "small"` (1366 / 1024 px wide), `force` (send the image even if
  the screen is exactly as in the last screenshot; otherwise an identical screen gives a short text),
  `include_elements` + `max_items`.
- `list_elements` / `include_elements`: `max_items` (default 300 lines); interactive controls first,
  then visible text, then long lists/trees/tables, ending with a note on what was left out.
- `click`, `type_text`, `press_keys`, `scroll`: `background: true` posts window messages to the window
  of the latest `screenshot(window=...)` without touching the real mouse, keyboard or focus.
  Best effort: classic Win32 controls (and Windows 11 Notepad's text box for typing) work; Chromium,
  Electron and most WinUI controls ignore posted input.
- Read-only data tools (`list_apps`, `list_elements`, `status`, `read_text`) also return
  `structuredContent` with an `outputSchema`; tools carry titles and read-only/destructive annotations.

## Any application: OCR, one find, scripts (Rust build, 0.3)

**On-screen text (OCR).** For apps with no or poor UI Automation (canvas apps, games, Qt/Java,
remote-desktop and VM windows). It uses the OCR engine built into Windows (Windows.Media.Ocr), in-process,
on the full-resolution capture (not the downscaled JPEG).

- `find_text(text, window?, monitor?, match?, max?, lang?)`: matches with their text, box and centre in the
  coordinates of the latest screenshot (the same convention as click x/y) and the line they are in, in reading
  order. OCR returns words; they are joined per line, so a phrase spanning several words matches.
  `match`: `contains` (default, case-insensitive), `exact` (whole words) or `regex` (case-insensitive unless the
  pattern starts with `(?-i)`). `window` / `monitor` read that instead and make it the view, as `screenshot` does.
- `click_text(text, nth?, button?, double?, offset_x?, offset_y?, match?, window?, monitor?)` plus `observe`:
  finds the text and clicks its centre through the normal guarded click path. With several matches and no
  `nth` it lists them and refuses instead of guessing. The result says whether the screen changed afterwards
  (a click is never retried automatically: a repeated click can double-submit).
- `wait_for` / `wait_gone` take `source: "auto" | "uia" | "ocr"`. `auto` (default) tries UI Automation, then OCR
  on each poll when UI Automation finds nothing (OCR no faster than every 250 ms, and only for scope `element` or
  `any`; scope `window` is the window title only).
- Language: the Windows user-profile languages, English (en-US) as the fallback, any installed OCR language with
  `lang` (BCP-47, e.g. `de-DE`). Add languages in Settings > Time & language > Language & region (the "Text
  recognition" feature). With none installed the tools say so.
- Limits: the engine accepts images up to `OcrEngine.MaxImageDimension` (10000 px here); a larger capture is
  scaled down to fit and the boxes are scaled back. Results are cached per picture (hash of the pixels), so asking
  again on an unchanged screen costs a capture and a hash, not a recognition. OCR misreads small, low-contrast or
  stylised text and cannot tell two identical labels apart: check before risky clicks. Timings on this PC
  (1920x1080 and 2560x1440 images, English): see `bench\RESULTS.md`.
- A forbidden app (password manager, Windows Security, lock screen) is refused by OCR too (reading a window of one,
  and clicking on one under the pointer), as for every other action.

**One find that tries each method.** `click_element(target)` (also `run_actions` / script `click` by target) tries,
in order, UI Automation (an exact control name, an exact automation id, a name containing the target, any element
whose name contains it, in the window being worked in), then OCR text in that window, then an error that says what
was tried and suggests a screenshot. The result states the method: "found via UI Automation" or "found via
on-screen text (OCR)". A numeric target is a `list_elements` id and only UI Automation resolves it. Several equal OCR
matches are refused (listed), not guessed. `find(target)` is the read-only version: where and how a target was found.
Retries are only for idempotent operations (focus, set_value verification); clicks are never repeated.

**Scripts.** `run_script(script, timeout_s?)` runs many steps in ONE call, locally, in a sandboxed
[Rhai](https://rhai.rs) script (the `rhai` crate: pure Rust, no file, network or process access unless the host
registers it). Only these functions are registered, and each goes through the same guarded code as the single
tools (Esc and corner fail-safe, forbidden apps, `apps.allowed`, stale-state refusals, MCP cancellation):

    click(x,y[,opts])  click_element(target[,opts])  click_text(text[,opts])  type_text(s)  press_keys("ctrl+s" | [..])
    scroll(x,y,amount[,horizontal])  drag(x,y,tx,ty)  set_value(target,v)  read_text([target])  list_elements([max])
    focus_window(name)  open_app(name)  wait_for(name[,secs|opts])->bool  wait_gone(name[,secs|opts])->bool
    sleep(ms)  active_window()  screenshot([#{window,monitor,size}])  log(msg)
    find(target) -> #{found, method, name, type, x, y, screen_x, screen_y, w, h, count, ambiguous, tried}
    find_text(text[,#{match,window,monitor,max,lang}]) -> #{found, count, matches:[#{text,x,y,w,h,cx,cy,line}], cx, cy, ..}

`wait_*` options: `timeout`, `scope` (default `any` in scripts), `source`. They return true/false instead of failing on
a timeout, so a script can branch. `click` options: `button`, `double`, `clicks`, `monitor`, `background`.
Example:

    open_app("Notepad");
    for l in ["one", "two"] { type_text(l + "\n"); }
    let r = find_text("two");
    if r.found { log(`two at ${r.cx},${r.cy}`); } else { screenshot(); }
    r.count

Limits: `timeout_s` (default 60, max 300), 2,000,000 operations, call depth 24, expression depth 48, strings
100,000 characters, arrays 1,000, maps 500, at most 3 `screenshot()` images, `sleep` at most 10 s, no `eval`, no
modules. Every action call checks Esc, the corner fail-safe, MCP cancellation and the deadline; exceeding a limit
ends the script with a clear error, the script line number and the log so far. The result is the log lines, the
returned value, the action count and time, the images, then the usual final observation (`observe`, default text).
A `notifications/progress` goes out per action when the request has a progress token. Scripts in the `bench`
(`bench\notepad_script.json`) show the saving against the same flow in single calls.

## MCP protocol

Speaks MCP revision **2026-07-28** (stateless: `server/discover`, per-request
`_meta["io.modelcontextprotocol/protocolVersion"]`, `ttlMs`/`cacheScope` on `tools/list`,
`resultType` on results) and, for older clients, the `initialize` handshake of 2025-11-25, 2025-06-18
and 2025-03-26. `notifications/cancelled` stops a running tool (no response is sent for it) and
`notifications/progress` is sent per `run_actions` step and per `run_script` action when the request has a `progressToken`.
Tasks, multi round-trip requests and the HTTP transport are not implemented (local stdio server,
short or cancellable calls, no mid-call user input, static tool list).
Protocol check: `python tests\mcp_stdio_test.py [path\to\desktop-control.exe]`.

## Benchmark

    desktop-control.exe bench bench\notepad.json --repeat 3 [--show]

runs a JSON list of `{"tool": ..., "args": {...}}` in-process through the same registry and prints,
per step, the time, number of images, image size, estimated image tokens (width x height / 750) and
text characters, then totals. `--show` also prints the text results. `bench\RESULTS.md` compares the
build before and after the speed work and the script flow against the single-call flow. OCR timing on an image: `desktop-control.exe ocr-bench image.png --repeat 5 [--find TEXT]`. Other checks: `tests\background_test.py`, `tests\completion_test.py`, `tests\ocr_script_test.py` (OCR on a window UI Automation cannot read, the click_element fallback, scripts and their limits, the forbidden-app guard inside a script; `ocr_form.ps1`, `forbidden_window.ps1` and `ocr_image.ps1` are its helpers).

## When a task ends

`finish` (or, if the turn ends without it, the Stop-hook `turn-ended` file) shows the green banner,
plays the chime, closes the capture session and brings you back to the window of the AI app the server was
started from. If Windows refuses it retries once with an attached input queue, then flashes the taskbar button;
it never moves focus while you are typing (flashes instead), nor while a new task runs. After 30 s
without a call the indicator hides; you are returned too unless you went to an unrelated window,
in which case that window's taskbar button flashes.

How the server finds that window (the "home window", marked `host: true` in `list_apps` and never read or
driven as a work window), in order: (0) `homeApp` in `config.json`; (1) the parent chain, up to the first
process that has a visible window (VS Code, Cursor, Claude Desktop, the Codex or ChatGPT apps, WezTerm,
Alacritty, ...); (2) the console owner, for command-line clients in any terminal (Claude Code, Codex CLI,
Gemini CLI, opencode in Windows Terminal, conhost, ...): a short-lived helper process attaches to the console
of the nearest ancestor that has one and walks from the console window to its root owner, which is the exact
terminal window (the right one of several Windows Terminal windows); (3) failing that, the window in front when
the first action of a task starts (returned to, and also protected as if it were the AI app's: when no AI app window could be identified the first result of a task says so). The messages name the app ("back in
Windows Terminal"), never a fixed product.

## The AI app's own window is off limits

The window of the AI app the server runs under (its terminal or chat window, see "When a task ends") is never read
as a work window, and keys, typing, clicks, scrolling, dragging, `set_value` and `element_action` are refused while
it is in front (the message says to focus the target app first): a stray Enter would submit a prompt and
alt+F4 would end the session. There is no opt-in: `focus_window` on it is refused too. Any chord that merely
contains a close or quit chord (alt+shift+F4, ctrl+alt+w ...), and ctrl+c, ctrl+d and alt+space, are refused on it
as a second line of defence. This holds for the plain tools, `run_actions` and `run_script`. The chain of processes
that makes up the AI app is re-checked (same start time, still running) instead of being trusted for the life of the
server. The same program policy (lock screen, security apps, password managers, `apps.allowed`) applies to what is
read: window screenshots, `list_elements`, `read_text`, element ids (each remembers its window) and OCR. Security
apps are blacked out of monitor screenshots, and so is a visible window whose program can't be identified (masking
fails closed like acting does); so is the rectangle of any child window of another program embedded in an ordinary window
(bounded walk: 400 children per window, 3000 per picture; a walk cut short blacks out the whole window), in monitor and window pictures and in the OCR that reads them.
`list_elements`, the text snapshots and named lookups skip the subtrees of elements whose own process is a protected one,
and the focused control is not read when it sits in one. Titles of those windows are never written into a result or an
error, and windows of protected or unidentifiable programs are not candidates when a window is chosen by name
(`focus_window`, `move_window`, `screenshot(window=...)`, waits): a name that only such a window matches gets the same
"No open window" answer as a name nothing matches. `open_app` refuses a name over 260 characters before any matching. An element's own
process (UI Automation reports one per control) is checked like its window's. Store (UWP) apps are judged by the program
inside their `ApplicationFrameHost` window (`Windows.UI.Core.CoreWindow`); an unidentifiable program is refused, not allowed.

## Stopping the AI

Press the physical **Esc** key or slam the mouse into a screen corner. Esc blocks every screen tool until it is lifted,
and it lifts only when BOTH hold: the agent's turn is over (the turn-ended file, `desktop-control.exe turn-ended`, run by a
client's Stop hook, touched after the stop, or 30 s without any tool call) AND a physical, non-injected key press or click
was seen after the stop **in your AI app**: it counts only when the foreground window, at that moment, is one of the AI
app's windows (a key or click in any other app never lifts it); a click must also lie inside that window's rectangle (the foreground window at button-down is the one being left), and a key must not be a modifier or Windows key nor pressed while Alt or a Windows key is down (Alt+Tab and Win+R are not answers), and on the turn-ended path only when it came after the
turn-ended touch. The hooks tell physical input from injected input by the `LLKHF_INJECTED` / `LLMHF_INJECTED`
flags, so nothing the agent sends counts; the Esc that stopped it, its release and its auto-repeat (the first 300 ms, and
Esc itself) never count. The AI app's windows (the exact window found for the client, the fallback window of M6 when none was
found, other windows of the host processes) are kept in a small lock-free cache that the overlay's poll refreshes about once a
second and at task start, so a hook callback only does `GetForegroundWindow`, `GetAncestor` and a few atomic loads: no lock, no
window message. When no AI app window is known at all (not even a fallback) nothing can be told apart and any window counts,
so that the stop does not hold for ever; a known AI app whose windows are momentarily hidden keeps the last cached windows. The messages the agent sees do not say how a stop lifts. `finish` hides the indicator and takes
the user home but never lifts it. After the user touches the mouse or keyboard, every input action is refused (not just
once) until the agent's own `screenshot` or `list_elements` tool call; a screenshot or listing inside `run_script`, or the
picture an action returns, does not count, and inside a script the refusal ends the run like the stop does (a script
cannot catch it). Typing checks the stop, the user and the window in front before every line and chunk, reports how far
it got and never retypes. It pastes line by line and gives the user's clipboard back after a line only once the field
visibly changed; when the field can't be read (or did not change in time) it waits until the target answers WM_NULL
promptly three times in a row (bounded to 12 s per typing) and gives the clipboard back once, at the end; the wait probes the window in front and, when the focused control belongs to another thread, that control's window too, and it ends at once on the user's stop or the client's cancellation (the clipboard is then put back and the result says the target may still have been about to paste) (password fields
and clipboard content marked private by a password manager are typed as keystrokes). The low-level hooks are
re-installed when input arrives that they never saw.

## Configuration (optional)

Copy `config.example.json` to `config.json` next to the server:

    {
      "clientColors": {"claude": "#D97757"},
      "strings": {"escToCancel": "Esc to cancel"},
      "apps": {"allowed": []},
      "homeApp": ""
    }

The indicator follows the connected AI client (MCP `clientInfo`: `initialize` params for older clients,
`_meta["io.modelcontextprotocol/clientInfo"]` for 2026-07-28): the glows, pill, ripple and name use the client's
colour and name, "Claude is using your computer", "Codex is using ...", "AI is using ..." when unknown, and the
finished banner says "✓  Claude finished" or "✓  Done". Built-in colours: claude #D97757, codex / openai / chatgpt
#10A37F, gemini #4285F4, copilot / visual studio code / vscode #8957E5, cursor #A1A1AA, windsurf / codeium #09B6A2,
anything else #6366F1. Precedence: `clientColors` (a map of name substring to hex) first, then `accentColor` (one
colour for every client), then the built-in table, then the default. `strings.usingComputer` overrides the pill text.
`status` reports `client`, `clientDisplay` and `accent`.

`apps.allowed` (exe names, e.g. `["msedge.exe", "notepad.exe"]`) restricts which apps the AI may
drive; empty means any app except the built-in blocked ones. `homeApp` (an exe name such as
`"windowsterminal.exe"` or `"Code.exe"`, optional) overrides the home-window detection: that app's
windows are the AI app's (the one in front when the task starts, else the topmost, is returned to).

## Checks

The Python tests in `tests/` (Python 3.12+, standard library only) drive the server over raw stdio.
They find the exe in this order: `DESKTOP_CONTROL_EXE`, `..\..\desktop-control.exe`, then the cargo
`target` folders. They only touch Notepad and windows they create themselves.

    python -I tests\mcp_stdio_test.py     # protocol: 2026-07-28 and legacy, cancellation, progress (no screen input)
    python -I tests\client_style_test.py  # indicator colour and name per client (no screen input)
    python -I tests\e2e_stdio.py          # end to end in Notepad
    desktop-control.exe bench bench
otepad.json   # speed and token cost

## Uninstall

    claude mcp remove --scope user computer-use

then delete the exe and, if you added one, the `turn-ended` Stop hook from your client's settings.

## Notes

- Everything runs locally; screenshots go only to the connected AI client, like any tool result.
- The indicator windows (pill, glows, ripple) are click-through and excluded from screen
  capture, so they never appear in screenshots or screen shares. The real Windows cursor is never
  hidden or replaced: older builds swapped the system cursors for a blank one during a task, which left
  them invisible if the server was killed; the server now reloads the cursor scheme once at start-up and
  `--cursor-manager` is only a harmless no-op. When stopping the server, stop the exact process by id, not
  every `desktop-control.exe` by image name.
- `src/` holds the Rust sources; [../BUILD.md](../BUILD.md) explains the toolchain.
