"""OCR, find, click_element fallback and run_script tests for desktop-control.exe (stdio, no SDK).

Windows used (all created here): a PowerShell WinForms window whose buttons are painted with GDI
text on a plain panel (UI Automation cannot see that text), Notepad, and a tiny program named
keepassxc.exe (a forbidden password-manager exe name; it only holds a text box) for the guard test.
Usage: python -I ocr_script_test.py [path\\to\\desktop-control.exe]
"""
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from dcclient import Server, Checker  # noqa: E402
import testutil  # noqa: E402

EXE = sys.argv[1] if len(sys.argv) > 1 else next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe"))
FORM = "DC OCR Test Form"
MARK = os.path.join(os.environ["TEMP"], "dc_mark.txt")
check = Checker()


def marks():
    try:
        return open(MARK).read().split()
    except OSError:
        return []


def start_ps(script, env_extra=None, args=()):
    env = dict(os.environ, **(env_extra or {}))
    return subprocess.Popen(["powershell", "-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-File", os.path.join(HERE, script), *args], env=env)


def kill_tree(p):
    """Stop a test process and its children (the compiled keepassxc.exe is a child of its PowerShell host)."""
    subprocess.run(["taskkill", "/F", "/T", "/PID", str(p.pid)], capture_output=True)


def kill_forbidden_test_program():
    """Stop only the test program (found by its exact path), never a real password manager of the same name."""
    subprocess.run(["powershell", "-NoProfile", "-Command",
                    "Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -like '*\dc_forbidden_test\keepassxc.exe' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }"],
                   capture_output=True)


def wait_window(s, title, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        t, _, _ = s.call("list_apps")
        if title in t:
            time.sleep(0.8)
            return True
        time.sleep(0.4)
    return False


if os.path.exists(MARK):
    os.remove(MARK)
form = start_ps("ocr_form.ps1", {"DC_MARK": MARK})
s = Server(EXE)
extra = []  # other processes to clean up
try:
    check(wait_window(s, FORM), "test form is up")
    t, imgs, err = s.call("screenshot", {"window": FORM})
    check(not err and imgs == 1, "screenshot of the form")

    print("--- 1. OCR on a window UI Automation cannot read")
    t, _, err = s.call("list_elements")
    check("Launch Rocket" not in t and "Real Button" in t, "UI Automation sees the real button but not the painted text", t[:200])
    t, _, err = s.call("find_text", {"text": "Launch Rocket"})
    print(t)
    check(not err and "1 match" in t and '"Launch Rocket"' in t, "find_text finds the painted 'Launch Rocket'")
    t2, _, _ = s.call("find_text", {"text": "launch rocket"})
    check("1 match" in t2 and "cached" in t2, "case-insensitive; same picture is served from the cache")
    t, _, _ = s.call("find_text", {"text": "Status: idle"})
    check("1 match" in t, "a phrase spanning two words matches", t)
    t, _, _ = s.call("find_text", {"text": r"Abort\s+Mission", "match": "regex"})
    check("1 match" in t, "regex match", t)
    t, _, _ = s.call("find_text", {"text": "Confirm", "match": "exact"})
    check("2 match" in t, "exact whole-word match finds both Confirm labels", t)
    t, _, _ = s.call("find_text", {"text": "no such words anywhere"})
    check("No on-screen text matches" in t and "OCR read:" in t, "no match says what OCR did read", t)
    t, _, err = s.call("find", {"target": "Launch Rocket"})
    check("found via on-screen text (OCR)" in t, "find -> OCR", t)
    t, _, err = s.call("find", {"target": "Real Button"})
    check("found via UI Automation" in t, "find -> UI Automation", t)
    t, _, err = s.call("find", {"target": "zzz no such thing"})
    check("Nothing found" in t and "Tried:" in t and "screenshot" in t, "find says what was tried", t)

    print("--- 2. click_text / click_element fallback")
    t, _, err = s.call("click_text", {"text": "Launch Rocket", "window": FORM})
    print(t)
    check(not err and "rocket" in marks() and "found via on-screen text (OCR)" in t, "click_text clicked the painted button", f"{marks()} {t[:200]}")
    check("screen changed" in t.lower(), "the result says the screen changed", t)
    t, _, _ = s.call("find_text", {"text": "rocket clicked"})
    check("1 match" in t, "the app's own status text confirms it", t)
    n = len(marks())
    t, _, err = s.call("click_element", {"target": "Abort Mission"})
    print(t)
    check(not err and "abort" in marks() and "found via on-screen text (OCR)" in t, "click_element fell back from UI Automation to OCR", f"{marks()} {t[:200]}")
    t, _, err = s.call("click_element", {"target": "Real Button"})
    check(not err and "real" in marks() and "found via UI Automation" in t, "click_element used UI Automation for a real control", f"{marks()} {t[:200]}")
    n = len(marks())
    t, _, err = s.call("click_text", {"text": "Confirm"})
    check(err and "2 places match" in t and len(marks()) == n, "click_text with two matches lists them and refuses (nothing clicked)", t)
    t, _, err = s.call("click_element", {"target": "Confirm"})
    check(err and "2 times" in t and len(marks()) == n, "click_element with two OCR matches refuses", t)
    t, _, err = s.call("click_text", {"text": "Confirm", "nth": 2})
    check(not err and marks()[-1] == "confirm2", "click_text nth=2 clicks the second", f"{marks()} {t[:200]}")
    t, _, err = s.call("click_element", {"target": "zzz no such thing"})
    check(err and "Tried:" in t and "UI Automation" in t and "OCR" in t, "click_element error says what was tried", t)
    t, _, err = s.call("run_actions", {"actions": [{"action": "click_element", "name": "Launch Rocket"}], "observe": "none"})
    check("found via on-screen text (OCR)" in t and marks()[-1] == "rocket", "run_actions click_element also falls back to OCR", f"{marks()} {t[:200]}")

    print("--- 3. wait_for / wait_gone with source")
    t, _, err = s.call("run_actions", {"actions": [{"action": "wait_for", "name": "rocket clicked", "scope": "element", "source": "ocr", "timeout": 5}], "observe": "none"})
    check("found" in t and "OCR" in t, "wait_for source=ocr", t)
    t, _, err = s.call("run_actions", {"actions": [{"action": "wait_for", "name": "rocket clicked", "scope": "any", "timeout": 5}], "observe": "none"})
    check("found" in t and "OCR" in t, "wait_for auto falls back to OCR when UI Automation finds nothing", t)
    t, _, err = s.call("run_actions", {"actions": [{"action": "wait_for", "name": "rocket clicked", "scope": "element", "source": "uia", "timeout": 1}], "observe": "none"})
    check("FAILED" in t, "wait_for source=uia does not use OCR", t)
    t, _, err = s.call("run_actions", {"actions": [{"action": "wait_gone", "name": "text that is not there", "scope": "element", "source": "ocr", "timeout": 3}], "observe": "none"})
    check("is gone" in t, "wait_gone source=ocr", t)

    print("--- 4. run_script")
    script = '''
screenshot(#{window: "DC OCR Test Form"});
let clicked = 0;
for label in ["Launch Rocket", "Abort Mission", "Nonexistent Label"] {
    let r = find_text(label);
    if r.found {
        click(r.cx, r.cy);
        clicked += 1;
        log(`${label} at ${r.cx},${r.cy}`);
    } else {
        log(`${label}: not found`);
    }
}
let f = find("Real Button");
log(`real button via ${f.method}`);
clicked
'''
    before = len(marks())
    t, imgs, err = s.call("run_script", {"script": script})
    print(t)
    check(not err and "Script finished" in t and "Returned: 2" in t, "loop + branch on find_text ran, returned 2", t[:300])
    check(imgs == 1, "screenshot() added one image", str(imgs))
    check(marks()[before:] == ["rocket", "abort"], "the two found labels were clicked, the missing one skipped", str(marks()[before:]))
    check("Nonexistent Label: not found" in t and "real button via uia" in t, "log lines present")
    check("Active window now" in t, "final observation follows")

    t, _, err = s.call("run_script", {"script": "let a = 1;\nlet b = 2;\nclick_element(\"zzz nothing here\");\nlet c = 3;"})
    check("SCRIPT FAILED at line 3" in t and "Nothing found" in t, "error includes the script line number", t[:300])
    t, _, err = s.call("run_script", {"script": "let a = 1;\nlet b = a + ;\n"})
    check("SCRIPT FAILED at line 2" in t and "syntax" in t.lower(), "syntax error with line", t[:200])
    t, _, err = s.call("run_script", {"script": "log(\"before\");\nundefined_function(1);"})
    check("SCRIPT FAILED at line 2" in t and "before" in t, "unknown function: line + log so far", t[:300])
    t, _, err = s.call("run_script", {"script": "try { click_element(\"zzz nothing here\"); } catch (e) { log(\"caught\"); }\n\"done\""})
    check("Script finished" in t and "caught" in t and "Returned: done" in t, "try/catch works", t[:300])

    t0 = time.time()
    t, _, err = s.call("run_script", {"script": "loop { sleep(200); }", "timeout_s": 2, "observe": "none"})
    dt = time.time() - t0
    check("SCRIPT FAILED" in t and "timed out" in t and dt < 6, f"timeout ends an endless script ({dt:.1f}s)", t[:200])
    t0 = time.time()
    t, _, err = s.call("run_script", {"script": "let i = 0;\nwhile true { i += 1; }", "observe": "none"})
    dt = time.time() - t0
    check("SCRIPT FAILED at line 2" in t and "operation limit" in t, f"operation limit ({dt:.1f}s)", t[:200])
    t, _, err = s.call("run_script", {"script": "fn f(n) { f(n + 1) }\nf(0)", "observe": "none"})
    check("SCRIPT FAILED" in t and ("call depth" in t or "depth" in t), "runaway recursion is stopped, server survives", t[:200])
    t, _, err = s.call("run_script", {"script": "let s = \"x\";\nfor i in 0..40 { s += s; }\ns.len", "observe": "none"})
    check("SCRIPT FAILED" in t and "too large" in t, "string size limit", t[:200])
    t, imgs, err = s.call("run_script", {"script": "screenshot(); screenshot(); screenshot(); screenshot();", "observe": "none"})
    check("SCRIPT FAILED" in t and "at most 3" in t and imgs == 3, "at most 3 screenshots", t[:200])
    t, _, err = s.call("run_script", {"script": "eval(\"1+1\")", "observe": "none"})
    check("SCRIPT FAILED" in t, "eval is disabled", t[:200])

    # progress notifications, one per action
    coll = []
    rid = s.start_call("run_script", {"script": "sleep(10); sleep(10); sleep(10); active_window();", "observe": "none"}, progress="p1")
    s.wait(rid, 30, coll)
    prog = [m["params"]["progress"] for m in coll if m.get("method") == "notifications/progress"]
    check(prog == [1.0, 2.0, 3.0, 4.0], f"progress notification per action {prog}")

    # cancellation mid-script: no response, the server stays responsive
    rid = s.start_call("run_script", {"script": "loop { sleep(100); }", "timeout_s": 60, "observe": "none"})
    time.sleep(1.5)
    t0 = time.time()
    s.notify("notifications/cancelled", {"requestId": rid})
    rid2 = s.request("tools/call", {"name": "status", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}})
    got = []
    r2 = s.wait(rid2, 10, got)
    dt = time.time() - t0
    check("result" in r2 and dt < 3, f"cancelled script stops promptly, next request answered ({dt:.2f}s)")
    time.sleep(0.5)
    late = []
    try:
        s.wait(rid, 1.0, late)
        check(False, "no response for the cancelled script")
    except AssertionError:
        check(not any(m.get("id") == rid for m in late + got), "no response for the cancelled script")

    # the corner fail-safe inside a script: the pointer is put in the top-left corner of the primary monitor
    import ctypes
    user32 = ctypes.windll.user32
    rid = s.start_call("run_script", {"script": "loop { sleep(100); }", "timeout_s": 60, "observe": "none"})
    time.sleep(1.2)
    user32.SetCursorPos(0, 0)
    m = s.wait(rid, 10)
    r = m["result"]
    txt = " ".join(c["text"] for c in r["content"] if c["type"] == "text")
    check(r.get("isError") and "screen corner" in txt, "corner fail-safe stops a running script", txt[:200])
    user32.SetCursorPos(400, 400)
    time.sleep(0.5)
    s.call("screenshot", {"window": FORM})  # a stopped task needs a fresh look before the next one

    print("--- 5. forbidden app inside a script")
    forb = start_ps("forbidden_window.ps1")
    extra.append(forb)
    check(wait_window(s, "keepassxc.exe", 40), "forbidden-named test window is up (list_apps shows its program but hides the title)")
    t, _, err = s.call("run_script", {"script": 'screenshot(#{window: "DC Forbidden Test Window"});\ntype_text("hello");', "observe": "none"})
    check("SCRIPT FAILED" in t and "refusing to control keepassxc.exe" in t, "script: typing into a forbidden app is refused", t[:300])
    t, _, err = s.call("run_script", {"script": 'screenshot(#{window: "DC Forbidden Test Window"});\nclick(50, 50);', "observe": "none"})
    check("SCRIPT FAILED" in t and "refusing to control keepassxc.exe" in t, "script: click in a forbidden app is refused (its screenshot is refused already, at line 1)", t[:300])
    t, _, err = s.call("find_text", {"text": "secret", "window": "DC Forbidden Test Window"})
    check(err and "refusing to control keepassxc.exe" in t, "find_text on a forbidden app is refused", t[:300])
    t, _, err = s.call("run_script", {"script": 'let r = find_text("secret", #{window: "DC Forbidden Test Window"});', "observe": "none"})
    check("SCRIPT FAILED" in t and "refusing to control keepassxc.exe" in t, "script: find_text on a forbidden app is refused", t[:300])
    kill_tree(forb)
    kill_forbidden_test_program()
    s.call("focus_window", {"name": FORM, "observe": "none"})

    print("--- 6. OCR in Notepad")
    np_scope = testutil.NotepadScope()  # the user's own Notepad windows are never touched
    t, _, err = s.call("open_app", {"name": "Notepad", "observe": "none"})
    check(not err, "Notepad opened", t[:200])
    time.sleep(0.8)
    NP = (np_scope.mine() or [0])[0]
    NPW = "#%d" % NP
    testutil.require_fg(hwnd=NP)
    s.call("screenshot", {"window": NPW})
    testutil.require_fg(hwnd=NP)
    s.call("type_text", {"text": "OCR reads this sentence in Notepad", "observe": "none"})
    time.sleep(0.5)
    t, _, err = s.call("find_text", {"text": "this sentence in Notepad", "window": NPW})
    print(t)
    check("1 match" in t, "find_text finds text in Notepad", t[:300])
    t, _, err = s.call("find", {"target": "this sentence in Notepad"})
    check("found via" in t, "find in Notepad", t[:200])
    t, _, err = s.call("click_element", {"target": "sentence in Notepad", "observe": "none"})
    check(not err and "found via" in t, "click_element in Notepad", t[:200])
    s.call("screenshot", {"window": NPW})
    s.call("click", {"x": 420, "y": 500, "observe": "none"})  # into the editor: the OCR click above landed on the tab header
    testutil.press(s, ["ctrl+a"], hwnd=NP)
    testutil.press(s, ["delete"], hwnd=NP)
    for _ in range(15):  # wait until the editor really reads empty
        rt, _, _ = s.call("read_text", {"target": "Text editor"})
        if 'value: ""' in rt:
            break
        time.sleep(0.3)
    check('value: ""' in rt, "Notepad's text was emptied before closing", ascii(rt)[:300])
    time.sleep(3.0)  # let Notepad store the emptied tab state before it closes
    testutil.post_close(NP)  # WM_CLOSE to this test's own window (an emptied document closes without a prompt)
    t, _, err = s.call("run_actions", {"actions": [{"action": "wait_gone", "name": NPW, "timeout": 5, "scope": "window"}], "observe": "none"})
    check(not testutil.is_window(NP), "Notepad closed again", t[:200])

    print("--- 7. timings (this machine)")
    s.call("focus_window", {"name": FORM, "observe": "none"})
    for label, args in (("window (form, 739x553)", {"window": FORM}), ("monitor 1", {"monitor": 1})):
        for k in range(2):
            t0 = time.time()
            t, _, _ = s.call("find_text", {"text": "Launch Rocket", **args})
            dt = (time.time() - t0) * 1000
            print(f"  find_text {label}, call {k + 1}: {dt:.0f} ms round trip; {t.splitlines()[0]}")
finally:
    if "np_scope" in globals() and np_scope.mine() and s.p.poll() is None:
        np_scope.close_mine(s)  # a failed run: close only the window this test opened
    kill_tree(form)
    for p in extra:
        kill_tree(p)
    kill_forbidden_test_program()
    s.close()
    import shutil
    shutil.rmtree(os.path.join(os.environ["TEMP"], "dc_forbidden_test"), ignore_errors=True)

print()
print("ALL OK (0 failures)" if check.fails == 0 else f"FAILED ({check.fails} failures)")
sys.exit(1 if check.fails else 0)
