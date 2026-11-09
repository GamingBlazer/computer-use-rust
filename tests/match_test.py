"""Window matching by name (bug 1): titles are normalised before they are compared (invisible Unicode
format characters such as U+200B removed, whitespace collapsed, case ignored) and a name also matches
the app's exe name. The windows are created here (PowerShell WinForms); nothing else is touched.

Usage: python -I match_test.py [path\\to\\desktop-control.exe]
"""
import ctypes
import os
import subprocess
import sys
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from dcclient import Server, Checker  # noqa: E402

EXE = sys.argv[1] if len(sys.argv) > 1 else next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe"))
user32 = ctypes.windll.user32
user32.GetForegroundWindow.restype = wintypes.HWND
check = Checker()

# the title has a zero-width space inside "Micro|soft", a no-break space and a double space; "edge" only
# appears as part of the first title
TITLE_PS = "'Micro' + [char]0x200B + 'soft' + [char]0x00A0 + ' Edge Test Zq'"
FORM = r"""
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form; $f.Text = %s; $f.Width=420; $f.Height=240; $f.StartPosition='Manual'; $f.Location='300,250'
$g = New-Object Windows.Forms.Form; $g.Text = 'DC Plain Second Window'; $g.Width=300; $g.Height=200; $g.StartPosition='Manual'; $g.Location='800,500'
$g.Show()
[Windows.Forms.Application]::Run($f)
"""


def fg_title():
    b = ctypes.create_unicode_buffer(512)
    user32.GetWindowTextW(user32.GetForegroundWindow(), b, 512)
    return b.value


p = subprocess.Popen(["powershell", "-NoProfile", "-STA", "-Command", FORM % TITLE_PS], creationflags=0x08000000)
s = Server(EXE)
try:
    end = time.time() + 20
    while time.time() < end:
        t, _, _ = s.call("list_apps")
        if "Edge Test Zq" in t and "DC Plain Second Window" in t:
            break
        time.sleep(0.4)
    check("Edge Test Zq" in t, "test windows are up (title contains U+200B and a no-break space)")
    time.sleep(0.8)

    def focus(name):
        s.call("focus_window", {"name": "DC Plain Second Window", "observe": "none"})
        time.sleep(0.4)
        text, _, err = s.call("focus_window", {"name": name, "observe": "none"})
        return text, err

    text, err = focus("Microsoft Edge Test Zq")
    check(not err and "Edge Test Zq" in fg_title(), "focus_window('Microsoft Edge Test Zq') finds the title that has U+200B", text)
    text, err = focus("MICROSOFT   edge test ZQ")
    check(not err and "Edge Test Zq" in fg_title(), "case and extra whitespace do not matter", text)
    text, err = focus("soft edge")
    check(not err and "Edge Test Zq" in fg_title(), "a partial name works across the invisible characters", text)
    text, err = focus("powershell")
    check(not err and ("Edge Test Zq" in fg_title() or "DC Plain" in fg_title()), "focus_window('powershell') matches by exe name", text + " | " + fg_title())
    text, err = focus("powershell.exe")
    check(not err and ("Edge Test Zq" in fg_title() or "DC Plain" in fg_title()), "the exe name with .exe matches too", text + " | " + fg_title())

    t, imgs, err = s.call("screenshot", {"window": "Microsoft Edge Test Zq"})
    check(not err and imgs == 1, "screenshot(window=) uses the same matching", t[:150])
    t, _, err = s.call("run_actions", {"actions": [{"action": "wait_for", "name": "microsoft edge test zq", "timeout": 3, "scope": "window"}], "observe": "none"})
    check(not err and "found window" in t, "wait_for scope window uses the same matching", t[:200])
    t, _, err = s.call("run_script", {"script": 'focus_window("Microsoft Edge Test Zq"); active_window()', "observe": "none"})
    check(not err and "Edge Test Zq" in t, "the script's focus_window uses the same matching", t[:200])
    t, _, err = s.call("run_script", {"script": 'let a = wait_for("microsoft  edge test zq", #{scope: "window", timeout: 3}); a', "observe": "none"})
    check(not err and "Returned: true" in t, "the script's wait_for(scope window) too", t[:200])
    t, _, err = s.call("focus_window", {"name": "No Such Window Qxz", "observe": "none"})
    check("No open window" in t, "a name that matches nothing is still refused", t[:100])
    s.call("finish", {"summary": "match test"})
finally:
    s.close()
    subprocess.run(["taskkill", "/F", "/T", "/PID", str(p.pid)], capture_output=True)

print("FAILED" if check.fails else "ALL OK", f"({check.fails} failures)")
sys.exit(1 if check.fails else 0)
