# desktop-control 0.2.0: before and after

Machine: this PC (two monitors, Windows 11 22631), Notepad (WinUI) as the test app.
`bench\notepad.json` = open Notepad, screenshot its window, type two lines, Ctrl+End, list_elements,
read_text, clear, close (alt+F4), wait_gone, finish. 11 steps, run 3 times per build.
Image tokens are estimated as width x height / 750. Raw output: `baseline.txt` (old build),
`after.txt` (new build); the `*_defaults.txt` pair runs `notepad_defaults.json`, the same flow with
every call at its default arguments (what a model that never passes `screenshot_after` gets).

## Totals per run (average of 3 runs)

| flow | build | ms/run | ms/step | images | est. image tokens | text chars |
|---|---|---:|---:|---:|---:|---:|
| notepad.json (4 steps pass `screenshot_after:false`) | before | 2430 | 221 | 3.0 | 3698 | 2179 |
| notepad.json | after | 3047 | 277 | 1.0 | 1150 | 4464 |
| notepad_defaults.json (all defaults) | before | 4156 | 378 | 7.0 | 8795 | 2425 |
| notepad_defaults.json | after | 2948 | 268 | 1.0 | 1150 | 4464 |

Reading it honestly:

- **Tokens and images, the main goal: down by about 70 % / 87 %.** With defaults, 7 images (8.8k image
  tokens) became 1 image (the one `screenshot` the flow asks for) plus about 2.3k more characters of text
  (roughly 600 text tokens). That is about 8.8k -> 1.8k tokens for the flow.
- **Time with defaults: 29 % faster** (4156 -> 2948 ms per run). Each action no longer captures, converts,
  encodes and base64s a 1366 px JPEG, and the fixed 200 ms wait became a 90-180 ms event-driven one.
  A window screenshot itself went 165 -> 53 ms (the capture session is reused and the wait for a "newer
  frame" is skipped after a settle).
- **Time when a caller passes `screenshot_after:false`: slower** (2430 -> 3047 ms per run). The old
  `false` meant "return nothing but the active window", about 25 ms. By the specification `false` now
  means observe=text, which waits for the window to settle and reads its controls twice (before/after,
  the "before" is reused from the previous action when possible): about 180 ms per action on Notepad
  (UI Automation costs about 2 ms per control). Use `observe:"none"` for the old behaviour and speed.
- list_elements: 158 -> 116 ms (cached state properties instead of per-element pattern calls), and it now
  also lists visible text, which is why its output is about 300 characters longer.
- open_app (1.5-2.4 s) is dominated by Notepad starting; not a server cost.

Per step (ms, average of 3; notepad.json, before -> after): open_app 1561 -> 1736 (startup noise),
screenshot 165 -> 53, type_text 91 -> 251, press_keys(ctrl+End, was with screenshot) 280 -> 189,
list_elements 158 -> 116, read_text 4 -> 5, press_keys (false) 24 -> 182, run_actions(wait_gone) 8 -> 96
(the final observation), finish 82 -> 47.

## Fixed waits that were changed

| where | was | now |
|---|---|---|
| `tools.rs` after-action wait (`SETTLE`) | fixed 200 ms before every after-action screenshot | `settle::after_action`: min 30 ms, done after 80 ms without change, cap 700 ms. Signals polled every 10 ms: hash of the newest Windows.Graphics.Capture frame (when a capture session exists for the target, or one is opened because a screenshot follows), foreground window + title, target window rectangle. The text observation's after-snapshot runs inside this wait and is kept if nothing changed meanwhile. Falls back to the fixed 200 ms if there is no target to watch. |
| `tools.rs` `capture_window_view`, restoring a minimized window | fixed 300 ms | settle, min 30 / quiet 80 / cap 500 ms, picture watched |
| `actions.rs` `open_app`, after the new window is found | fixed 200 ms | the same settle (cap 500 ms) on the new window |
| `capture.rs` `grab`, waiting for a frame newer than the last | 60 ms | 0 ms when settle saw the target quiet within the last 300 ms, else 60 ms |
| `actions.rs` `wait_for` poll interval | 150 ms | 80 ms |
| `actions.rs` `wait_gone` poll interval | 250 ms | 120 ms |

Not changed on purpose (correctness): the 200 ms fallback pause in `type_text` when the field text can't be
read (paste timing, the clipboard is restored afterwards), the 100 ms press/move waits in `drag`, 15 ms /
80 ms in clicks, 20 ms in key chords, 8 ms glide steps, `bring_to_front`'s 20 ms polls (250 ms cap) and the
Start-menu-search sleeps (150 / 800 ms) in `open_app`.
Discovery during the work: the capture frame counter can't be the "screen changed" signal: Windows delivers
frames continuously (about 300 per second here) whether or not the pixels change, so settle hashes the frame.
UI Automation structure-changed events were not used (subscribing makes browsers turn their accessibility
tree on for the whole process).

## What was verified (all on the built exe, this machine)

| item | result |
|---|---|
| observe text / screenshot / both / none, `screenshot_after` mapping | verified in the bench with `--show` (text diffs read correctly: typed text shows as a value change, window changes as "Window changed") |
| unchanged-screenshot suppression, `force=true`, reset after `finish` | verified: identical screen -> "Screen unchanged ..." text; force sends the image; `size` change or new view sends the image |
| `max_items` budget and ordering | verified on Notepad (max_items=5 listed controls only, note says what was left out); long-list ordering is in the code but untested on a real 1000-item list |
| `size:"small"` | verified: monitor screenshot 1366x768 -> 1024x576; a window narrower than 1024 px is unchanged by design |
| background input | `tests\background_test.py`: on a WinForms window (EDIT, BUTTON) click, type (EM_REPLACESEL), keys and wheel all worked with the real cursor and foreground window unchanged; on Windows 11 Notepad's text box (RichEditD2DPT) `type_text` worked too. Not tried: Chromium, Electron, other WinUI controls (expected to ignore it), Ctrl/Alt shortcuts (expected not to work) |
| MCP: server/discover, per-request `_meta` version, legacy initialize 2025-03-26/06-18/11-25, -32022 / -32602 errors, annotations, titles, outputSchema + structuredContent, ttlMs/cacheScope | `tests\mcp_stdio_test.py`: all checks ok |
| cancellation | long `wait_for` (40 s) cancelled after 1.5 s: no response for it, the next request answered within 0.1 s |
| progress | 3 increasing `notifications/progress` for a 3-step `run_actions`; none without a token |
| old end-to-end flow | `e2e.py` needs the `mcp` SDK (no venv here); `tests\e2e_stdio.py` repeats the same flow over raw stdio: ok |

## Step E: end of task (completion)

`tests\completion_test.py`; a tkinter window of the test process stands in for VS Code (the server treats
windows of the processes above it as its home); other windows are two WinForms forms.

| case | result |
|---|---|
| (a) task, then the `turn-ended` file touched, no `finish` | back in the home window in about 0.2 s; server log "task completed (TurnEnded): back in ..." |
| (b) idle 30 s, mouse moved over the task window | back in the home window (the old code refused because the mouse had moved) |
| (c) idle 30 s, an unrelated window focused before | not moved; the home window's taskbar button flashed ("you are in another window of your own") |
| (d) `finish` | back in the home window; message states what happened |
| typing guard (a physical key in the last 1.5 s -> flash instead), AttachThreadInput retry after a refused foreground lock, 4 s timeout when the home window does not answer | implemented; the timeout path fired once by accident in a test whose window was not pumping messages (finish returned with "did not answer within 4 s ... flashed"); the typing guard and the attach retry could not be triggered from a script (injected input is not "physical") and are untested |
| green banner and chime | cannot be observed from a script; implemented in the shared `session::complete` used by `finish` and the turn-ended path (banner, `MessageBeep`), verified only through the server log |

## Risks and behaviour changes

- Defaults changed: action tools return text, not a screenshot. A model that relied on the automatic
  picture must pass `observe:"screenshot"` / `"both"` (instructions.txt says so).
- `screenshot_after:false` is slower than before (see above); `observe:"none"` is the fast path.
- `structuredContent` duplicates the text of the four data tools; if a client forwards both to the model
  that costs tokens (not measurable here).
- Coordinate clicks before any screenshot are now refused ("take a screenshot first") instead of landing
  on the top-left corner.
- Unknown tool names now return the JSON-RPC error -32602 (spec) instead of an `isError` result.
- The text diff only sees interactive controls of the work window (Claude Code's own window and
  forbidden apps are never read); changes in page content, canvases or images need a screenshot.

## Found on the way: the server instructions were being cut off

`claude --debug mcp get Computer-Use` logs "Server instructions truncated ... to 4096 chars": Claude Code keeps
only the first 4096 characters. The old `instructions.txt` (5150 chars) lost its last paragraph (the
confirm-first rules and the Esc/fail-safe note); a first draft of the new one (6556) would have lost more.
It is now rewritten to 3994 characters with every rule kept and the new options added, and the debug log no
longer reports a truncation. The same log shows `protocolEra: modern`, `negotiatedProtocolVersion: 2026-07-28`
after the deploy (Claude Code 2.1.319).

## Follow-up fix (director, after review)
`screenshot_after: false` was slower than before (about 180 ms per action) because it mapped to the full text observation. It now maps to an internal `brief` mode: message, active window and focused control, with no change list and no settle wait, like the old behaviour. One bench run of `notepad.json` with the fixed build: steps 7–10 took 25 / 24 / 79 / 9 ms (baseline 24 / 24 / 33 / 8 ms; before the fix 182 / 182 / 191 / 96 ms). Wall time per run was 2847 ms, dominated by the cold Notepad launch (open_app 2024 ms). `tests\mcp_stdio_test.py` was re-run on the deployed exe: all OK.


---

# desktop-control 0.3.0: OCR, one find, script mode

Same machine and the same Notepad flow. Raw output: `bench\before_v03.txt` (the deployed 0.2.0 build, run at the start
of this work), `bench\after_v03.txt` (0.3.0, single calls), `bench\script_v03.txt` (0.3.0, `notepad_script.json`).
3 runs each; image tokens estimated as width x height / 750; "text tokens" = text characters / 4.

## Script mode against single calls (the same Notepad work)

`notepad.json` = 11 tool calls (open, screenshot, type, keys, list, read, clear, close, wait, finish).
`notepad_script.json` = 1 `run_script` doing all of it (open_app, screenshot, type_text, press_keys, list_elements,
read_text, clear, close, wait_gone) + `finish`.

| flow | build | model turns (tool calls) | server ms/run | images | est. image tokens | text chars (about tokens) |
|---|---|---:|---:|---:|---:|---:|
| notepad.json | 0.2.0 | 11 | 2407 | 1 | 1150 | 3558 (890) |
| notepad.json | 0.3.0 | 11 | 2362 | 1 | 1150 | 3737 (930) |
| notepad_script.json | 0.3.0 | 2 | 1937 (run 1 2390 with the cold Notepad start, runs 2-3 about 1660) | 1 | 1150 | 371 (90) |

- The server time is what the exe spends; it excludes the model's own time per turn, which is the bigger saving
  (11 turns became 2, each turn re-reading the conversation). Server time alone drops about 18 % (about 30 % without
  the cold start) because the per-step text observations (a before/after UI Automation snapshot and a settle wait per
  action) are not taken inside a script, only once at the end.
- Text returned to the model: 3737 -> 371 characters, since `list_elements` and `read_text` results stay inside the
  script unless it returns or logs them. The script text itself costs about 480 characters (about 120 tokens) to
  write, against 11 JSON calls of about 40-110 characters each plus their per-call overhead.
- The 0.3.0 single-call flow is within noise of 0.2.0 (2362 vs 2407 ms): the new code does not slow the old paths.
  (`click_element` now looks at automation ids too, and the crawl for it also caches them; `wait_for` / `wait_gone`
  scope window are unchanged.)

## OCR timings (Windows.Media.Ocr, en-US, this PC)

| what | size | text | OCR time |
|---|---|---|---:|
| window: WinForms test form (GDI-painted text) | 739x553 | 8 lines | 28-37 ms |
| window: Notepad | 847x1018 | 11 lines | 33-37 ms |
| monitor 1, live screen (find_text monitor=1) | 1920x1080 | 84-88 lines | 200-270 ms (round trip incl. capture 340-650 ms) |
| image, dense UI-size text (`ocr-bench`, warm) | 1920x1080 | 115 lines, 692 words | 377 ms |
| image, dense UI-size text (`ocr-bench`, warm) | 2560x1440 | 209 lines, 1256 words | 682 ms (first run, with engine creation, 706 ms) |

- This PC has one 1920x1080 monitor now, so a real 2560x1440 monitor could not be captured: the 2560x1440 figure is the
  engine on a 2560x1440 image filled with dense 11 pt text (a harder case than most screens), through the same
  recognition path (`desktop-control.exe ocr-bench image.png --repeat 5`). The capture and pixel-hash of such a monitor
  add on top; not measured. `OcrEngine.MaxImageDimension` is 10000 here, so no scaling or tiling was needed; the
  scale-down path for larger images exists but could not be exercised (no image over 10000 px was tried).
- A repeat `find_text` on an unchanged picture is a cache hit: about 10 ms round trip on a window (capture + hash,
  no recognition), shown as "cached, same picture" in the result.

## What was verified (on the built exe, this machine)

`tests\ocr_script_test.py` (all checks ok) runs against a PowerShell WinForms window whose labels are drawn with GDI
`DrawString` on a plain panel, so UI Automation cannot see them (`list_elements` shows only the real button and the
title bar):

| item | result |
|---|---|
| OCR finds and clicks text UI Automation cannot see | `find_text` found "Launch Rocket" (box and centre in view coordinates), `click_text` clicked it (the app recorded the click, its painted status text changed, the result said "The screen changed after the click") |
| OCR in Notepad | `find_text` found "this sentence in Notepad" in a Notepad window; `find` and `click_element` used OCR there too |
| phrases, case, exact, regex | "Status: idle" (two words), "launch rocket", `exact` finds both "Confirm" labels, regex `Abort\s+Mission`; a miss lists what OCR did read |
| click_element UI Automation -> OCR | "Real Button" -> "found via UI Automation"; "Abort Mission" -> "found via on-screen text (OCR)" (clicked, recorded by the app); unknown text -> error "Nothing found ... Tried: UI Automation ...; on-screen text (OCR) ... Take a screenshot"; also through `run_actions` |
| ambiguity | two identical "Confirm" labels: `click_text` and `click_element` list both and click nothing; `nth=2` clicks the second |
| `find` | read-only, states the method and the position, or what was tried |
| `wait_for` / `wait_gone` `source` | `ocr` and `auto` (UI Automation first, then OCR) find the painted "rocket clicked"; `uia` does not; `wait_gone` `ocr` |
| `run_script` | loop + branch on `find_text` (clicked the two found labels, skipped the missing one, `find` said "uia" for the real button, returned 2, one screenshot image, final observation); try/catch; error with line number (`click_element` failure at line 3, syntax error at line 2, unknown function at line 2 with the log before it); timeout (`loop { sleep(200) }` with `timeout_s` 2 ended after 2.0 s); operation limit (`while true { i += 1 }` ended after 0.3 s at line 2); runaway recursion stopped and the server kept answering; string size limit; 4th `screenshot()` refused; `eval` disabled |
| progress and cancel | one `notifications/progress` per script action (1, 2, 3, 4); a `notifications/cancelled` for `loop { sleep(100) }` ended it at once, no response was sent for it and the next request was answered in 0.03 s |
| corner fail-safe inside a script | the pointer put in the top-left corner while `loop { sleep(100) }` ran: the script stopped with the "moved the mouse to a screen corner" error |
| forbidden apps inside scripts | a test program named `keepassxc.exe` (a forbidden exe name, containing only a text box) with a window "DC Forbidden Test Window": `screenshot` of it then `type_text` in a script -> "refusing to control keepassxc.exe"; the same with `click`; `find_text` on it (tool and script) refused too. Unit tests (`cargo test`) cover the policy function: password managers, Windows Security, lock screen, `apps.allowed`, and that the forbidden list beats the allowed list |
| earlier tests | `mcp_stdio_test.py` ALL OK (tool count 18 -> 22 and the read-only set updated in the test), `e2e_stdio.py` ok, `completion_test.py` ALL OK, `background_test.py` ALL OK, `bench\notepad.json` ran without errors |
| cursor (separate commit "Never swap system cursors") | `tests\cursor_test.py` ALL OK: the detector sees a blank arrow (0 visible pixels) and a scheme reload restores it (144 px); during a task the arrow is intact (144 px) and no `--cursor-manager` process exists; the server force-killed mid-task with `taskkill /F /PID` leaves the cursor showing (144 px); a stale `--cursor-manager 1` call exits at once; start-up repairs a blanked arrow; with the debug switch `DESKTOP_CONTROL_CAPTURABLE=1` a monitor screenshot shows the halo, pill and glows, without it none of them (`bench\evidence\cursor_halo_*.jpg`, on a white full-screen window of the test) |

Not verified: the physical Esc kill switch inside a script (the hook ignores injected keys, so a script test cannot press it;
the script calls the same `guards::check_failsafe` as every other action, which the corner test exercises);
`apps.allowed` end to end (needs a `config.json` next to the deployed exe; covered by the unit test of the policy function only);
OCR languages other than en-US; OCR of an image above `MaxImageDimension`; a real 2560x1440 monitor.

## Risks and behaviour changes

- New dependencies: `rhai` 1.27 (script language, pure Rust) and `regex` 1 (`match: regex`). The exe grew from about 1.9 MB to 7.2 MB.
- `click_element` can now click on-screen text that UI Automation does not know. It says so ("found via on-screen text (OCR)")
  and refuses ambiguous matches, but OCR can misread: a similar-looking text may be matched where a control name was meant.
  `click_text` / OCR clicks never retry; after one the result reports whether the screen changed (it waits up to 350 ms for a repaint).
- Every click now also checks the window under the pointer against the forbidden list (a click in a monitor view could land
  on a password manager's window that is not the foreground one); this applies to `click`, `click_element`, `click_text`.
- `wait_for` / `wait_gone` with scope element or any now fall back to OCR (default `auto`); `wait_gone` therefore costs
  one OCR pass (about 30-250 ms) after UI Automation reports the text gone. Use `source: "uia"` for the old behaviour.
  OCR polls are at least 250 ms apart. Scope `window` (the default of `wait_for`) never uses OCR.
- A numeric `click_element` target is still only a `list_elements` id (no OCR for digits).
- `run_script`: `wait_for` / `wait_gone` return true/false in scripts (a timeout is not an error) and default to scope `any`.
  Scripts run on the server's main thread with conservative recursion limits (a 1 MB stack): call depth 24, expression depth 48.
- `instructions.txt` is 4013 characters (limit 4096).
- The test count of `mcp_stdio_test.py` and its read-only tool set were updated for the four new tools.

# desktop-control 0.3.2: security fixes

`bench\notepad.json`, 3 runs, raw output in `bench\after_v032.txt`. Per run (sum of step times) 2605 ms, against about
2830 ms measured for 0.3.1 and 2362 ms for 0.3.0: the new guards (a last look before every key and button, the program
policy on every read, per-line typing checks, the user's clipboard given back after every line) cost nothing
measurable on this flow. Steps of the last run: open_app 1355, screenshot 52, type_text 351 (was about 250: one
paste per line now waits for the line break and for the text, and checks the password-field flag), press_keys 231,
list_elements 105, read_text 5, the three clean-up key steps 73, 41 and 107, run_actions 61, finish 7. Run 1 was 3040 ms
wall because of the cold Notepad start (open_app 2016 ms).

# desktop-control 0.3.3: the 0.3.2 re-check fixes

`bench
otepad.json`, 3 runs, raw output in `benchfter_v033.txt`. Per run (sum of step times) 2632 ms against 2605 ms
for 0.3.2 (+1 %, within the run-to-run noise of about 5 %). Steps of the last run: open_app 1288, screenshot 55,
type_text 358, press_keys 233, list_elements 108, read_text 4, the three clean-up key steps 73, 42 and 118, run_actions
61, finish 10. Notepad's text box is readable, so every paste is confirmed by the field changing and the new idle wait
(only used when a field can't be read or does not change in time) never ran. The open_app text is longer in this run
(817 characters against 578) only because the window in front before it was another app, which makes the change list
longer; the same flow with the same window in front is unchanged.
