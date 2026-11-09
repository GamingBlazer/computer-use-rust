# Bug list — v0.3.0 test round (ALL FIXED in v0.3.1, commit 0550095, 2026-11-09; see the end for open items)

1. **focus_window can't find Edge by name.** Edge titles contain a zero-width space ("Microsoft​ Edge"), so `focus_window("Microsoft Edge")` fails.
   - Fix: normalise titles before matching (strip zero-width and format characters, collapse whitespace, case-insensitive).
   - Also match the app/exe name (e.g. "edge", "msedge", "chrome").
   - Found 2026-11-09 while cycling through Edge tabs.

2. **finish doesn't return to Claude Code running in Windows Terminal.** `list_apps` shows the Claude Code window (windowsterminal.exe) as `host:false`, and finish says "the VS Code window wasn't found".
   - Cause: the parent chain is desktop-control.exe <- claude.exe <- pwsh.exe <- explorer.exe. Windows Terminal hosts the console through the default-terminal hand-off, so it is never a parent.
   - Fix: resolve the console window that claude.exe or its shell uses (AttachConsole/GetConsoleWindow, then the owner/root-owner of the pseudo-console window, which is the Windows Terminal window). Fall back to the foreground window at task start.
   - Mark that window `host:true`.
   - Make all "VS Code" wording neutral ("your app").
   - Found 2026-11-09.
   - **User requirement:** return to WHATEVER AI app the user uses, on any platform, not just Claude Code. Universal home-window design, each layer tried in order:
     1. Parent chain up to the first process with a visible top-level window. This covers GUI clients that spawn the server: VS Code, Cursor, Windsurf, Claude Desktop, Codex app, ChatGPT desktop. Skip helper processes (node, Electron utilities).
     2. Console owner, for CLI clients in any terminal (Claude Code, Codex CLI, Gemini CLI, opencode). Use AttachConsole(ancestor pid), then GetConsoleWindow, then GetAncestor(GA_ROOTOWNER) or GW_OWNER to reach the real terminal window (Windows Terminal hand-off, conhost, OpenConsole). Third-party terminals (WezTerm, Alacritty) are already in the parent chain.
     3. Fallback: the foreground window when the first action of a task starts.
     4. Override: `homeApp` in config.json.
   - Pick the exact window, not just the app (several VS Code or Terminal windows).
   - Neutral messages, e.g. "Back in Windows Terminal".
   - Test with: Claude Code in Windows Terminal, in VS Code, in classic conhost, and a GUI test app that spawns the server. Report other clients as "should work".

4. **Pointer invisible during a task, only the orange halo shows.** Reported by the user and reproduced 2026-11-09.
   - Cause: Windows "Hide pointer while typing" (SPI_GETMOUSEVANISH, on by default). After the tool presses keys, GetCursorInfo reports flags=0 (hidden) while the task runs. The pointer returns only on a real mouse movement.
   - Measured fixes:
     - A 1 px injected relative move does NOT unhide it (below the acceleration threshold).
     - A 3 px relative move DOES unhide it (flags=1). Relative moves aren't exact on return because of acceleration, so return with SetCursorPos to the saved point, which does not re-hide.
   - Fix: after keyboard actions (press_keys, type_text, script key steps), and at the start of the halo/glide and at finish, if the task is active and GetCursorInfo flags==0, nudge 3 px and SetCursorPos back. Verify flags==1. Never do it while the user is physically moving the mouse.
   - Test: press keys in Notepad, check flags==1 during the task, and check the pointer position is unchanged.

5. **`press_keys("ctrl+a")` inside run_script types a plain "a".** Reproduced 2026-11-09 in Notepad; `alt+F4` in the same script also did nothing. The same chord as a normal `press_keys` tool call works. `ctrl+tab` in a script worked in the Edge test.
   - Find the difference between the script binding and the tool path: chord string parsing, modifier timing, or the string-vs-array overload. Add tests for ctrl+a, alt+F4, ctrl+shift+s and ctrl+tab inside scripts.

6. **The text diff reads Claude Code's own terminal window** (WT tabs and buttons listed after alt+F4 returned focus to it). This happens because the host isn't recognised (bug 2). Re-check after fixing bug 2 that host windows are never listed or read.

7. **A test closed the user's Claude Code terminal (2026-11-09) and killed the session.**
   - Cause: once bug 2 is fixed, finish brings the host (WT) to the front, and a blind real-keyboard alt+F4 in the tests then closed it.
   - Fixes:
     - A product guard refuses input to the host window unless it was explicitly focused, and always refuses close chords on the host.
     - Tests close only their own HWNDs/PIDs and never use `taskkill /im`.

8. **User request: remove the halo ring around the cursor** ("doesn't look good"). Keep the click ripple, edge glow and pill.

9. **User request: the indicator colour and name follow the connected AI client.**
   - Colours: Claude #D97757 orange, OpenAI/Codex/ChatGPT #10A37F, Gemini #4285F4, Copilot/VS Code #8957E5, Cursor #A1A1AA, Windsurf #09B6A2, unknown = default #6366F1 indigo.
   - The pill reads "<Name> is using your computer" (or "AI …"). `status` reports client, clientDisplay and accent.
   - Config precedence: clientColors, then accentColor, then the built-in map, then the default.

3. **Leftover branding in tool text.** `status` says "cursor sprite" (gone) and "stopped Claude with Esc". Neutral wording is due in the release clean-up anyway.

## Status after v0.3.1 (0550095)
All items 1–9 fixed and covered by tests in `tests\`. Still open:
- **Flaky back-to-back runs:** background_test and ocr_script_test each failed once when every suite ran back to back. They pass alone and with 8 s gaps. Not diagnosed yet.
- **Not tested yet:** home window in VS Code, Cursor, Claude Desktop, Codex, Gemini CLI and opencode. These should work through the parent chain or console owner.
- **Bench:** notepad.json is about 5% slower than 0.3.0 in the same session (2830 vs 2701 ms), from the longer chord hold and pointer polling.

## Found in the v0.3.1 live self-test (2026-11-09): small usability items, not bugs
- **`read_text()` returns a labelled description** (`[Document] "Text editor"\nvalue: ""`), not the plain text. Scripts can't use `.len()` or compare it directly. Suggest `read_text(target, #{value_only: true})`, or a `read_value(target)` that returns only the value string.
- **Rhai's `trim()` works in place and returns `()`**, so `x.trim() == ""` is always false, and so is `${x.trim()}` printed as empty. Mention it in the run_script description, or register a `trimmed()` helper that returns a new string.
- Verified live: focus_window("explorer") by app name; Ctrl+A, Delete and Alt+F4 inside scripts; the WT window marked host:true; finish says "back in Windows Terminal" and returns there.
