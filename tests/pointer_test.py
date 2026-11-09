"""Hidden pointer during a task (bug 4). Windows' "Hide pointer while typing" hides the pointer after key
presses (GetCursorInfo flags = 0); during a task the server wakes it (a 3 px injected move, then SetCursorPos
back to the exact saved point). Only Notepad is used; every key press first checks that Notepad is in front.

  1. wake enabled : letter keys with no mouse movement -> pointer visible (flags 1), position unchanged
  2. negative control (DC_NO_POINTER_WAKE=1) -> pointer hidden (flags 0), which shows the test can see the bug
  3. scripts: press_keys / type_text steps inside run_script -> visible and unchanged too
  4. the user moving the mouse themselves is not interfered with is not testable without a physical mouse;
     the guard (no wake within 500 ms of physical mouse input) is covered by the code path only.

Usage: python -I pointer_test.py [path\\to\\desktop-control.exe]
"""
import ctypes
import os
import re
import sys
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from dcclient import Server, Checker  # noqa: E402
import testutil  # noqa: E402

EXE = sys.argv[1] if len(sys.argv) > 1 else next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe"))
user32 = ctypes.windll.user32
user32.GetForegroundWindow.restype = wintypes.HWND
check = Checker()


class CURSORINFO(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.DWORD), ("flags", wintypes.DWORD), ("hCursor", wintypes.HANDLE), ("pt", wintypes.POINT)]


def cursor():
    ci = CURSORINFO()
    ci.cbSize = ctypes.sizeof(ci)
    user32.GetCursorInfo(ctypes.byref(ci))
    return ci.flags & 1, (ci.pt.x, ci.pt.y)


def fg_title():
    b = ctypes.create_unicode_buffer(512)
    user32.GetWindowTextW(user32.GetForegroundWindow(), b, 512)
    return b.value


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [("dx", wintypes.LONG), ("dy", wintypes.LONG), ("mouseData", wintypes.DWORD), ("dwFlags", wintypes.DWORD),
                ("time", wintypes.DWORD), ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong))]


class INPUT(ctypes.Structure):
    class _U(ctypes.Union):
        _fields_ = [("mi", MOUSEINPUT)]
    _anonymous_ = ("u",)
    _fields_ = [("type", wintypes.DWORD), ("u", _U)]


def nudge_visible():
    """Put the pointer back to visible with a relative move, as a physical mouse would."""
    for _ in range(3):
        i = INPUT(type=0)
        i.mi = MOUSEINPUT(3, 0, 0, 0x0001, 0, None)
        user32.SendInput(1, ctypes.byref(i), ctypes.sizeof(INPUT))
        time.sleep(0.05)


def park(x, y):
    nudge_visible()
    user32.SetCursorPos(x, y)
    time.sleep(0.1)
    return cursor()


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


def notepad_in_front():
    return fg_exe() == "notepad.exe"


NP = None  # the Notepad window this test opened (handle)


def need_notepad():
    testutil.require_fg(hwnd=NP)


vanish = ctypes.c_int(0)
user32.SystemParametersInfoW(0x1020, 0, ctypes.byref(vanish), 0)  # SPI_GETMOUSEVANISH
print("Hide pointer while typing (SPI_GETMOUSEVANISH):", bool(vanish.value))

PARK = (700, 520)
scope = testutil.NotepadScope()
s = Server(EXE)
t, _, err = s.call("open_app", {"name": "Notepad"}, timeout=60)
time.sleep(1.0)
NP = (scope.mine() or [0])[0]
need_notepad()
s.close()  # no finish here: the real terminal must not be brought to the front before keyboard input


def run_phase(label, env, expect_visible):
    """Letter keys through the tools, then through a script; returns the readings."""
    srv = Server(EXE, env=env)
    readings = []
    try:
        for k in "qwe":
            srv.call("focus_window", {"name": "#%d" % NP, "observe": "none"})
            time.sleep(0.3)
            need_notepad()
            pos = park(*PARK)
            before = pos
            srv.call("press_keys", {"keys": [k], "observe": "none"})
            fl, p = cursor()
            readings.append(("press_keys " + k, fl, before[1], p))
        pos = park(*PARK)
        srv.call("type_text", {"text": "ab", "observe": "none"})
        fl, p = cursor()
        readings.append(("type_text ab", fl, pos[1], p))
        pos = park(*PARK)
        sc = 'if active_window().contains("Claude") || active_window().contains("Terminal") { throw "not notepad"; } press_keys("r"); press_keys("t"); type_text("yu");'
        srv.call("run_script", {"script": sc, "observe": "none"})
        fl, p = cursor()
        readings.append(("script keys", fl, pos[1], p))
        pos = park(*PARK)
        sc = 'if active_window().contains("Claude") || active_window().contains("Terminal") { throw "not notepad"; } press_keys("ctrl+a");'
        srv.call("run_script", {"script": sc, "observe": "none"})
        fl, p = cursor()
        readings.append(("script ctrl+a", fl, pos[1], p))
        fl_task = cursor()[0]
        status, _, _ = srv.call("status")
        readings.append(("task active", 1 if "task active" in status else 0, None, None))
        # clear the text with a normal call
        need_notepad()
        srv.call("press_keys", {"keys": ["ctrl+a"], "observe": "none"})
        need_notepad()
        srv.call("press_keys", {"keys": ["delete"], "observe": "none"})
    finally:
        srv.close()
    for what, fl, p0, p1 in readings:
        if what == "task active":
            check(fl == 1, f"{label}: the task was active while the pointer was read")
            continue
        if expect_visible:
            check(fl == 1 and p0 == p1, f"{label}: {what}: pointer visible and unmoved", f"flags={fl} before={p0} after={p1}")
        elif what == "script ctrl+a":
            continue  # a shortcut types no character, so Windows does not hide the pointer for it
        else:
            check(fl == 0, f"{label}: {what}: pointer hidden (the bug, with the wake disabled)", f"flags={fl}")
    return readings


try:
    if not vanish.value:
        print("SKIP: 'Hide pointer while typing' is off on this PC, so there is nothing to wake")
    else:
        run_phase("wake on", None, True)
        run_phase("negative control (wake off)", {"DC_NO_POINTER_WAKE": "1"}, False)
finally:
    # leave Notepad empty and closed: only the window this test opened, by handle
    s = Server(EXE)
    try:
        scope.close_mine(s)
    finally:
        s.close()

print("FAILED" if check.fails else "ALL OK", f"({check.fails} failures)")
sys.exit(1 if check.fails else 0)
