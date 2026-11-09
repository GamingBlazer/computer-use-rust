"""Keyboard shortcuts inside run_script (bug 5) must behave like the same chord in a normal press_keys call:
ctrl+a (string and array form), ctrl+shift+s, ctrl+tab, alt+F4. Before the fix a chord that was released
within 20 ms could reach a busy WinUI app as the plain key ("ctrl+a" typed "a", "alt+F4" did nothing).

Windows used (all created here): Notepad (every key press first checks that Notepad is in front) and a
PowerShell WinForms window for alt+F4. Part 3 repeats the Notepad case with the CPUs loaded, with the new
key sequence and, for comparison, with DC_LEGACY_KEYS=1 (the v0.3.0 sequence; its result is informative only).

Usage: python -I keys_script_test.py [path\\to\\desktop-control.exe] [--no-load]
"""
import ctypes
import os
import re
import subprocess
import sys
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from dcclient import Server, Checker  # noqa: E402
import testutil  # noqa: E402

args = [a for a in sys.argv[1:] if not a.startswith("--")]
EXE = args[0] if args else next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe"))
LOAD = "--no-load" not in sys.argv
user32 = ctypes.windll.user32
user32.GetForegroundWindow.restype = wintypes.HWND
check = Checker()


def fg_title():
    b = ctypes.create_unicode_buffer(512)
    user32.GetWindowTextW(user32.GetForegroundWindow(), b, 512)
    return b.value


def fg_exe():
    pid = wintypes.DWORD()
    user32.GetWindowThreadProcessId(user32.GetForegroundWindow(), ctypes.byref(pid))
    k = ctypes.windll.kernel32
    h = k.OpenProcess(0x1000, False, pid.value)
    buf = ctypes.create_unicode_buffer(1024)
    n = wintypes.DWORD(1024)
    k.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(n))
    k.CloseHandle(h)
    return buf.value.rsplit("\\", 1)[-1].lower()


def need(exe_name):
    if fg_exe() != exe_name:
        raise SystemExit("ABORT: %s is not in front (%r); refusing to send keys" % (exe_name, fg_title().encode("ascii", "replace").decode()))


# a script that refuses to run when the AI's own window or a terminal is in front
GUARD = 'let w = active_window(); if w.contains("Claude") || w.contains("Terminal") || w.contains("PowerShell") { throw "wrong window: " + w; } '


def value_of(text):
    m = re.search(r'value: "([^"]*)"', text)
    return m.group(1) if m else None


def replace_case(s, script, label):
    need("notepad.exe")
    t, _, err = s.call("run_script", {"script": GUARD + script, "observe": "none"}, timeout=90)
    v = value_of(t)
    check(not err and v == "X", label, f"{t[:160]!r}")
    return v == "X"


scope = testutil.NotepadScope()  # the user's own Notepad windows are never touched
s = Server(EXE)
form = None
try:
    print("--- 1. ctrl+a, ctrl+shift+s, ctrl+tab in Notepad, inside run_script")
    s.call("open_app", {"name": "Notepad"}, timeout=60)
    time.sleep(1.0)
    need("notepad.exe")
    # start from an empty tab
    s.call("press_keys", {"keys": ["ctrl+a"], "observe": "none"})
    s.call("press_keys", {"keys": ["delete"], "observe": "none"})
    replace_case(s, 'type_text("hello world"); press_keys("ctrl+a"); type_text("X"); read_text()', 'script press_keys("ctrl+a") selects all (type then replaces)')
    replace_case(s, 'type_text("hello world"); press_keys(["ctrl", "a"]); type_text("X"); read_text()', 'script press_keys(["ctrl","a"]) (array form) selects all')
    replace_case(s, 'type_text("some more text"); press_keys("CTRL+A"); type_text("X"); read_text()', 'chord names are case-insensitive in scripts')
    # root cause of the v0.3.0 failure: focus_window tapped Alt before switching, which left the new window in
    # menu / access-key mode, so the next shortcut in the same script arrived as a plain letter
    other = subprocess.Popen(["powershell", "-NoProfile", "-STA", "-Command",
                              "Add-Type -AssemblyName System.Windows.Forms; $f = New-Object Windows.Forms.Form; $f.Text='DC Other Form'; "
                              "$f.Width=360; $f.Height=200; $f.StartPosition='Manual'; $f.Location='900,300'; [Windows.Forms.Application]::Run($f)"],
                             creationflags=0x08000000)
    try:
        end = time.time() + 20
        while time.time() < end and "DC Other Form" not in s.call("list_apps")[0]:
            time.sleep(0.4)
        bad = 0
        for i in range(10):
            s.call("focus_window", {"name": "Notepad", "observe": "none"})
            need("notepad.exe")
            t, _, err = s.call("run_script", {"script": GUARD + 'focus_window("DC Other Form"); focus_window("Notepad"); if !active_window().contains("Notepad") { throw "not notepad"; } type_text("hello world"); press_keys("ctrl+a"); type_text("X"); read_text()', "observe": "none"}, timeout=60)
            bad += value_of(t) != "X"
            need("notepad.exe")
            s.call("press_keys", {"keys": ["ctrl+a"], "observe": "none"})
            s.call("press_keys", {"keys": ["delete"], "observe": "none"})
        check(bad == 0, "focus_window then ctrl+a in the same script works every time (10 runs; v0.3.0 failed about 3 in 10)", f"{bad} of 10 failed")
    finally:
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(other.pid)], capture_output=True)
    # ctrl+shift+s: Save as dialog
    need("notepad.exe")
    t, _, err = s.call("run_script", {"script": GUARD + 'press_keys("ctrl+shift+s"); sleep(1200); active_window()', "observe": "none"}, timeout=60)
    check(not err and "Save" in t.split("Returned:")[-1], 'script press_keys("ctrl+shift+s") opens the Save as dialog', t[-160:])
    if "Save" in fg_title():
        s.call("press_keys", {"keys": ["escape"], "observe": "none"})
        time.sleep(0.6)
    # ctrl+tab: two tabs, switch with the chord
    need("notepad.exe")
    s.call("press_keys", {"keys": ["ctrl+a"], "observe": "none"})
    s.call("type_text", {"text": "tab-one", "observe": "none"})
    s.call("press_keys", {"keys": ["ctrl+n"], "observe": "none"})
    time.sleep(0.8)
    s.call("type_text", {"text": "tab-two", "observe": "none"})
    t, _, err = s.call("run_script", {"script": GUARD + 'press_keys("ctrl+tab"); sleep(700); let a = read_text(); a', "observe": "none"}, timeout=60)
    first = value_of(t)
    t, _, err = s.call("run_script", {"script": GUARD + 'press_keys(["ctrl", "tab"]); sleep(700); let a = read_text(); a', "observe": "none"}, timeout=60)
    second = value_of(t)
    check(first != second and {first, second} == {"tab-one", "tab-two"}, 'script press_keys("ctrl+tab") and the array form switch tabs', f"{first!r} then {second!r}")
    # tidy: both tabs emptied, then closed below
    for _ in range(2):
        need("notepad.exe")
        s.call("press_keys", {"keys": ["ctrl+a"], "observe": "none"})
        s.call("press_keys", {"keys": ["delete"], "observe": "none"})
        s.call("press_keys", {"keys": ["ctrl+w"], "observe": "none"})
        time.sleep(0.6)
    time.sleep(0.5)

    print("--- 2. alt+F4 on a window created here")
    ps = r"""
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form; $f.Text='DC Altf4 Target'; $f.Width=360; $f.Height=200; $f.StartPosition='Manual'; $f.Location='420,300'
[Windows.Forms.Application]::Run($f)
"""
    for variant, script in (("string", 'focus_window("DC Altf4 Target"); if !active_window().contains("DC Altf4 Target") { throw "not the test window"; } press_keys("alt+F4");'), ("array", 'focus_window("DC Altf4 Target"); if !active_window().contains("DC Altf4 Target") { throw "not the test window"; } press_keys(["alt", "F4"]);')):
        form = subprocess.Popen(["powershell", "-NoProfile", "-STA", "-Command", ps], creationflags=0x08000000)
        end = time.time() + 20
        while time.time() < end and "DC Altf4 Target" not in s.call("list_apps")[0]:
            time.sleep(0.4)
        time.sleep(0.8)
        t, _, err = s.call("run_script", {"script": script + ' wait_gone("DC Altf4 Target", #{scope: "window", timeout: 4})', "observe": "none"}, timeout=60)
        gone = "DC Altf4 Target" not in s.call("list_apps")[0]
        check(gone, f'script press_keys alt+F4 ({variant} form) closes the window', t[:160])
        if not gone:
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(form.pid)], capture_output=True)
        form = None

    if LOAD:
        print("--- 3. the same ctrl+a case with every CPU busy (new keys, then the v0.3.0 sequence for comparison)")
        s.close()
        s = None
        # separate busy-loop processes (not multiprocessing: its children would re-import and re-run this script)
        procs = [subprocess.Popen([sys.executable, "-I", "-c", "while True: pass"], creationflags=0x08000000) for _ in range(max(2, (os.cpu_count() or 4)))]
        try:
            results = {}
            for label, env in (("new keys", None), ("legacy keys (v0.3.0)", {"DC_LEGACY_KEYS": "1"})):
                srv = Server(EXE, env=env)
                ok = 0
                N = 12
                try:
                    srv.call("open_app", {"name": "Notepad"}, timeout=90)
                    time.sleep(1.0)
                    for i in range(N):
                        need("notepad.exe")
                        t, _, err = srv.call("run_script", {"script": GUARD + 'type_text("hello world"); press_keys("ctrl+a"); type_text("X"); read_text()', "observe": "none"}, timeout=90)
                        ok += value_of(t) == "X"
                        need("notepad.exe")
                        srv.call("press_keys", {"keys": ["ctrl+a"], "observe": "none"})
                        srv.call("press_keys", {"keys": ["delete"], "observe": "none"})
                finally:
                    srv.close()
                results[label] = (ok, N)
                print(f"     {label}: {ok}/{N} correct under load")
            check(results["new keys"][0] == results["new keys"][1], "new key sequence: ctrl+a in a script works every time under CPU load", str(results))
        finally:
            for p in procs:
                p.kill()
        s = Server(EXE)
finally:
    try:
        if s is None:
            s = Server(EXE)
        scope.close_mine(s)
    finally:
        s.close()
        if form is not None:
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(form.pid)], capture_output=True)

print("FAILED" if check.fails else "ALL OK", f"({check.fails} failures)")
sys.exit(1 if check.fails else 0)
