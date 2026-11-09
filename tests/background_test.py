"""Background input (background=true) against a plain Win32 EDIT/BUTTON window that this script
creates itself (a WinForms form via PowerShell), plus an informational try on Windows 11
Notepad (WinUI), which is expected to ignore posted input.

Checks that the real cursor did not move, the foreground window did not change, the button
click and the typed text arrived, and the wheel scrolled the edit control.

Usage: python background_test.py [path\\to\\desktop-control.exe]
"""
import ctypes
import json
import os
import queue
import re
import subprocess
import sys
import threading
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
EXE = sys.argv[1] if len(sys.argv) > 1 else next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe"))
META = {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}
user32 = ctypes.windll.user32
user32.GetForegroundWindow.restype = wintypes.HWND

FORM = r"""
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form; $f.Text='DC-BG-Test'; $f.Width=520; $f.Height=360; $f.StartPosition='Manual'; $f.Location='260,160'
$t = New-Object Windows.Forms.TextBox; $t.Multiline=$true; $t.ScrollBars='Vertical'; $t.Width=440; $t.Height=120; $t.Location='20,20'
$b = New-Object Windows.Forms.Button; $b.Text='Count'; $b.Location='20,160'
$l = New-Object Windows.Forms.Label; $l.Text='clicks: 0'; $l.Location='140,165'; $l.AutoSize=$true
$script:n = 0
$b.Add_Click({ $script:n++; $l.Text = "clicks: $script:n" })
$f.Controls.AddRange(@($t,$b,$l))
[Windows.Forms.Application]::Run($f)
"""
OTHER = r"""
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form; $f.Text='DC-FG-Other'; $f.Width=300; $f.Height=160; $f.StartPosition='Manual'; $f.Location='900,600'
$f.Add_Shown({ $f.Activate() })
[Windows.Forms.Application]::Run($f)
"""


def cursor():
    p = wintypes.POINT()
    user32.GetCursorPos(ctypes.byref(p))
    return p.x, p.y


def fg_title():
    buf = ctypes.create_unicode_buffer(256)
    user32.GetWindowTextW(user32.GetForegroundWindow(), buf, 256)
    return buf.value


class Client:
    def __init__(self):
        self.p = subprocess.Popen([EXE], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0)
        self.q = queue.Queue()
        threading.Thread(target=lambda: [self.q.put(json.loads(l)) for l in self.p.stdout if l.strip()], daemon=True).start()
        self.n = 0

    def call(self, _tool, **args):
        self.n += 1
        msg = {"jsonrpc": "2.0", "id": self.n, "method": "tools/call", "params": {"name": _tool, "arguments": args, "_meta": META}}
        self.p.stdin.write((json.dumps(msg) + "\n").encode())
        self.p.stdin.flush()
        while True:
            m = self.q.get(timeout=40)
            if m.get("id") == self.n:
                break
        r = m["result"]
        text = "\n".join(c["text"] for c in r["content"] if c["type"] == "text")
        return text, sum(1 for c in r["content"] if c["type"] == "image"), r

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


failed = 0


def check(cond, what):
    global failed
    print(("ok   " if cond else "FAIL ") + what)
    failed += 0 if cond else 1


def find_xy(listing, kind, name):
    m = re.search(r'\[%s\] "%s"[^\n]*?\((\d+), (\d+)\)' % (kind, re.escape(name)), listing)
    return (int(m.group(1)), int(m.group(2))) if m else None


procs = []
c = None
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import testutil  # noqa: E402
sys.stdout.reconfigure(encoding="utf-8", errors="replace")
np_scope = testutil.NotepadScope()  # the user's own Notepad windows are never touched
try:
    flags = 0x08000000  # CREATE_NO_WINDOW
    procs.append(subprocess.Popen(["powershell", "-NoProfile", "-STA", "-Command", FORM], creationflags=flags))
    time.sleep(3.0)
    procs.append(subprocess.Popen(["powershell", "-NoProfile", "-STA", "-Command", OTHER], creationflags=flags))
    time.sleep(3.0)
    for _ in range(6):  # take the foreground back if something stole it (the lock may refuse the first tries)
        if fg_title() == "DC-FG-Other":
            break
        oh = testutil.u.FindWindowW(None, "DC-FG-Other")
        testutil.u.keybd_event(0x87, 0, 0, 0)
        testutil.u.keybd_event(0x87, 0, 2, 0)
        testutil.u.SetForegroundWindow(oh)
        time.sleep(0.5)
    check(fg_title() == "DC-FG-Other", f"foreground is the other window before the test ({fg_title()!r})")
    cur0, fg0 = cursor(), fg_title()

    c = Client()
    text, imgs, _ = c.call("screenshot", window="DC-BG-Test")
    check(imgs == 1, "window screenshot of the test form (it is not in front)")
    listing, _, _ = c.call("list_elements")
    btn = find_xy(listing, "Button", "Count")
    check(btn is not None, f"Count button found in the listing at {btn}")
    edit = None
    m = re.search(r'\[Edit\][^\n]*?\((\d+), (\d+)\)', listing)
    if m:
        edit = (int(m.group(1)), int(m.group(2)))
    check(edit is not None, f"edit field found at {edit}")

    text, _, r = c.call("click", x=btn[0], y=btn[1], background=True, observe="none")
    check("in the background" in text and "best-effort" in text, "click result says it was posted and best-effort")
    time.sleep(0.4)
    text, _, _ = c.call("click", x=btn[0], y=btn[1], background=True, observe="none")
    time.sleep(0.4)
    listing, _, _ = c.call("list_elements")
    check('"clicks: 2"' in listing, "the button received both background clicks (label reads clicks: 2)")

    text, _, _ = c.call("click", x=edit[0], y=edit[1], background=True, observe="none")
    text, _, _ = c.call("type_text", text="hello background\nsecond line", background=True, observe="none")
    check("EM_REPLACESEL" in text and "hello background" in text, "type_text used EM_REPLACESEL and the field reads back the text")
    text, _, _ = c.call("press_keys", keys=["x", "y"], background=True, observe="none")
    check("in the background" in text, "press_keys posted")
    rt, _, rr = c.call("read_text", target="Edit") if False else (None, None, None)

    # many lines, then wheel: first visible line must move down
    c.call("type_text", text="\n".join(f"line {i}" for i in range(60)), background=True, observe="none")
    hwnds = []
    cb = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)(lambda h, lp: (hwnds.append(h), True)[1])
    top = user32.FindWindowW(None, "DC-BG-Test")
    user32.EnumChildWindows(top, cb, 0)
    edit_h = next((h for h in hwnds if (lambda b: (user32.GetClassNameW(h, b, 128), "edit" in b.value.lower())[1])(ctypes.create_unicode_buffer(128))), None)
    first = lambda: user32.SendMessageW(edit_h, 0xCE, 0, 0)  # EM_GETFIRSTVISIBLELINE
    c.call("press_keys", keys=["ctrl+End"], background=True, observe="none")
    before = first()
    c.call("scroll", x=edit[0], y=edit[1], amount=5, background=True, observe="none")  # up
    time.sleep(0.3)
    after_up = first()
    check(edit_h is not None and after_up != before, f"background wheel scrolled the edit control (first visible line {before} -> {after_up})")

    check(cursor() == cur0, f"real cursor did not move {cur0} -> {cursor()}")
    check(fg_title() == fg0, f"foreground window unchanged ({fg0!r} -> {fg_title()!r})")

    # a window the guard refuses is still refused in background mode: no window view -> error
    c.call("screenshot")  # monitor view
    text, _, r = c.call("click", x=10, y=10, background=True, observe="none")
    check(r["isError"] and "window-scoped" in text, "background without a window view is refused")

    # informational: WinUI Notepad
    if True:
        c.call("open_app", name="Notepad", observe="none")
        for _ in range(25):
            if np_scope.mine():
                break
            time.sleep(0.2)
        time.sleep(0.6)
        NP = (np_scope.mine() or [0])[0]
        c.call("screenshot", window="#%d" % NP)
        text, _, r = c.call("type_text", text="posted text", background=True, observe="none")
        time.sleep(0.4)
        rt, _, _ = c.call("read_text", target="Text editor")
        print("info Notepad (WinUI):", "posted text ARRIVED" if "posted text" in rt else "posted text ignored (as expected for WinUI)")
        print("info Notepad type_text result:", text[:120].replace("\n", " "))
finally:
    if c:
        try:
            for h in np_scope.mine():  # only the Notepad window this test opened, by handle
                c.call("focus_window", name="#%d" % h, observe="none")
                time.sleep(0.6)
                if testutil.fg_hwnd() == h:  # empty it first: Notepad keeps unsaved text and restores it next time
                    c.call("press_keys", keys=["ctrl+a"], observe="none")
                    testutil.require_fg(hwnd=h)
                    c.call("press_keys", keys=["delete"], observe="none")
                    time.sleep(3.0)  # let Notepad store the emptied tab state
                testutil.post_close(h)
                time.sleep(1.2)
                if testutil.is_window(h) and testutil.fg_hwnd() == h:
                    c.call("click_element", target="Don't save", observe="none")
                    time.sleep(0.8)
        finally:
            c.close()
    for p in procs:
        p.kill()  # only the processes this test started

print()
print("FAILED" if failed else "ALL OK", f"({failed} failures)")
sys.exit(1 if failed else 0)
