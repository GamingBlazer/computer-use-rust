"""End-of-task behaviour: the user is brought back to the home window (a tkinter window of this
script stands in for VS Code: the server treats the windows of the processes above it as its
host), or its taskbar button is flashed when switching would be rude.

  (a) task, then the turn-ended file is touched, no finish  -> banner + chime + back home
  (b) idle 30 s, the mouse moved over the task window       -> back home
  (c) idle 30 s, the user went to an unrelated window       -> not yanked, home flashed
  (d) the finish tool                                       -> back home, truthful message

The banner and the chime cannot be observed from here; the server logs what it did on stderr
("task completed (...)") and that is what is checked, together with the real foreground window.
Only windows created by this script are used. Takes about 90 s.

Usage: python completion_test.py [path\\to\\desktop-control.exe]
"""
import ctypes
import json
import os
import queue
import subprocess
import sys
import threading
import time
import tkinter
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
EXE = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe")))
META = {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}
user32 = ctypes.windll.user32
user32.GetForegroundWindow.restype = wintypes.HWND
user32.FindWindowW.restype = wintypes.HWND

FORM = r"""
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form; $f.Text='%s'; $f.Width=420; $f.Height=240; $f.StartPosition='Manual'; $f.Location='%s'
[Windows.Forms.Application]::Run($f)
"""

failed = 0


sys.stdout.reconfigure(encoding="utf-8", errors="replace")


def check(cond, what):
    global failed
    print(("ok   " if cond else "FAIL ") + what, flush=True)
    failed += 0 if cond else 1


def fg_title():
    buf = ctypes.create_unicode_buffer(256)
    user32.GetWindowTextW(user32.GetForegroundWindow(), buf, 256)
    return buf.value


def activate(title):
    """Make a window the foreground one (this script owns the desktop for the test)."""
    h = user32.FindWindowW(None, title)
    if fg_title() == title:
        return True
    for _ in range(5):
        user32.keybd_event(0x87, 0, 0, 0)  # a lone F24 tap lifts the foreground lock (Alt would open a window menu)
        user32.keybd_event(0x87, 0, 2, 0)
        user32.SetForegroundWindow(h)
        time.sleep(0.3)
        if fg_title() == title:
            return True
    return False


fg_log = []
_last_fg = [None]


def note_fg():
    t = fg_title()
    if t != _last_fg[0]:
        _last_fg[0] = t
        fg_log.append(f"{time.time() - T0:6.1f}s foreground -> {t!r}")


T0 = time.time()


def wait_fg(title, seconds):
    end = time.time() + seconds
    while time.time() < end:
        root.update()
        note_fg()
        if fg_title() == title:
            return True
        time.sleep(0.1)
    return fg_title() == title


root = tkinter.Tk()
root.title("DC-HOME")
root.geometry("300x120+1100+80")
root.update()

procs = []
flags = 0x08000000
for title, loc in (("DC-TARGET", "200,200"), ("DC-OTHER", "700,500")):
    procs.append(subprocess.Popen(["powershell", "-NoProfile", "-STA", "-Command", FORM % (title, loc)], creationflags=flags))
t_end = time.time() + 12
while time.time() < t_end and not (user32.FindWindowW(None, "DC-TARGET") and user32.FindWindowW(None, "DC-OTHER")):
    root.update()
    time.sleep(0.2)
time.sleep(1.5)

srv = subprocess.Popen([EXE], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0, env=dict(os.environ, **({"DC_TRACE": "1"} if os.environ.get("DC_TEST_VERBOSE") else {})))
out_q, err_lines = queue.Queue(), []
threading.Thread(target=lambda: [out_q.put(json.loads(l)) for l in srv.stdout if l.strip()], daemon=True).start()
def _read_err():
    for l in srv.stderr:
        line = l.decode("utf8", "replace").strip()
        err_lines.append(line)
        if os.environ.get("DC_TEST_VERBOSE"):
            print("     [server]", line, flush=True)


threading.Thread(target=_read_err, daemon=True).start()
n = 0


def call(tool, **args):
    global n
    n += 1
    msg = {"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": tool, "arguments": args, "_meta": META}}
    srv.stdin.write((json.dumps(msg) + "\n").encode())
    srv.stdin.flush()
    end = time.time() + 40
    while time.time() < end:
        root.update()  # the home window must keep pumping messages while the tool runs, like VS Code does
        try:
            m = out_q.get_nowait()
        except queue.Empty:
            time.sleep(0.02)
            continue
        if m.get("id") == n:
            return "\n".join(c["text"] for c in m["result"]["content"] if c["type"] == "text")
    raise TimeoutError(tool)


def start_task():
    """Home in front, then the task starts (it remembers the home window) and goes to DC-TARGET."""
    check(activate("DC-HOME"), "DC-HOME is in front when the task starts")
    root.update()
    if os.environ.get("DC_TEST_VERBOSE"):
        print("     [test] calling focus_window", flush=True)
    call("focus_window", name="DC-TARGET", observe="none")
    check(fg_title() == "DC-TARGET", "the task moved to DC-TARGET")


def completed(reason):
    return [l for l in err_lines if l.startswith(f"task completed ({reason})")]


ONLY = os.environ.get("DC_ONLY", "abcd")  # e.g. DC_ONLY=d to run just the finish case
turn_file = os.path.join(os.path.dirname(EXE), "turn-ended")
try:
    # (a) turn ended, no finish
    if 'a' in ONLY:
        start_task()
        n_before = len(completed("TurnEnded"))
        with open(turn_file, "w") as f:
            f.write("")
        os.utime(turn_file, None)
        ok = wait_fg("DC-HOME", 6)
        for _ in range(40):
            root.update()
            if len(completed("TurnEnded")) > n_before:
                break
            time.sleep(0.1)
        check(ok, f"(a) turn-ended without finish: back in DC-HOME (foreground {fg_title()!r})")
        check(len(completed("TurnEnded")) > n_before and "back in" in completed("TurnEnded")[-1], f"(a) logged: {completed('TurnEnded')[-1:] }")
        print("     (the green banner and the chime cannot be observed from outside; see the report)")
        os.remove(turn_file)
        time.sleep(1.0)

    # (b) idle, mouse moved over the task window
    if 'b' in ONLY:
        start_task()
        r = wintypes.RECT()
        user32.GetWindowRect(user32.FindWindowW(None, "DC-TARGET"), ctypes.byref(r))
        user32.SetCursorPos((r.left + r.right) // 2, (r.top + r.bottom) // 2)
        time.sleep(0.5)
        user32.SetCursorPos((r.left + r.right) // 2 + 20, (r.top + r.bottom) // 2 + 10)
        print("     waiting for the 30 s idle end ...", flush=True)
        ok = wait_fg("DC-HOME", 40)
        check(ok, f"(b) idle after moving the mouse over the task window: back in DC-HOME (foreground {fg_title()!r})")
        for _ in range(40):
            root.update()
            if any("(Idle)" in l and "back in" in l for l in err_lines):
                break
            time.sleep(0.1)
        check(any("(Idle)" in l and "back in" in l for l in err_lines), "(b) logged an idle completion that went back")
        time.sleep(1.0)

    # (c) idle, the user went to an unrelated window
    if 'c' in ONLY:
        n_idle = len(completed("Idle"))
        start_task()
        check(activate("DC-OTHER"), "(c) the user focused an unrelated window (DC-OTHER)")
        print("     waiting for the 30 s idle end ...", flush=True)
        end = time.time() + 40
        while time.time() < end and len(completed("Idle")) == n_idle:
            root.update()
            note_fg()
            time.sleep(0.2)
        time.sleep(1.0)
        check(len(completed("Idle")) > n_idle, "(c) the idle end happened")
        check(fg_title() == "DC-OTHER", f"(c) not yanked: foreground still DC-OTHER ({fg_title()!r})")
        last = completed("Idle")[-1] if completed("Idle") else ""
        check("flashed the taskbar button" in last and "another window of your own" in last, f"(c) home was flashed: {last[:150]!r}")

    # (d) finish
    if 'd' in ONLY:
        start_task()
        text = call("finish", summary="completion test")
        check(wait_fg("DC-HOME", 3) and "back in" in text, f"(d) finish went back home: {text!r}")
        check(any(l.startswith("task completed (Finish)") for l in err_lines), "(d) logged a Finish completion")

finally:
    try:
        srv.stdin.close()
        srv.wait(timeout=5)
    except Exception:
        srv.kill()
    for p in procs:
        p.kill()
    if os.path.exists(turn_file):
        os.remove(turn_file)
    root.destroy()

print()
print("server log lines about completion:")
for l in err_lines:
    if l.startswith("task completed"):
        print("  ", l)
print("foreground changes seen while waiting:")
for l in fg_log:
    print("  ", l)
print("FAILED" if failed else "ALL OK", f"({failed} failures)")
sys.exit(1 if failed else 0)
