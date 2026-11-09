"""The home window (bugs 2 and 6): the server returns the user to, and never reads, the window of the AI app
it was started from, in every kind of setup. Only windows and processes created by this script are used; the
user's own Windows Terminal windows are never typed into, closed or resized (their handles are listed first and
checked afterwards).

  (a) client inside a NEW Windows Terminal window (wt.exe -w new ...)   -> that window is host, finish returns to it
  (b) client inside a classic console window (conhost.exe ...)          -> that console window is host
  (c) a WinForms GUI app that spawns the server (like VS Code, Claude Desktop, the Codex app)
  (d) in every setup, the user's existing Windows Terminal windows are NOT host
  (e) no AI app in the process tree (client started through WMI): the window in front at the first action is
      the fallback: returned to, and protected like the AI app's own window (the first result says so)
  (f) homeApp in config.json overrides the detection
  and, from the clients running in (a)/(b): with the host window in front, list_elements, find, find_text and
  the text observation never list or read it.

Usage: python -I home_test.py [path\\to\\desktop-control.exe] [a b c e f]
"""
import ctypes
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import testutil  # noqa: E402
args = sys.argv[1:]
EXE = os.path.abspath(args[0]) if args and args[0].lower().endswith(".exe") else os.path.abspath(next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe")))
ONLY = "".join(a for a in args if not a.lower().endswith(".exe")) or "abcef"
PY = sys.executable
PYW = os.path.join(os.path.dirname(sys.executable), "pythonw.exe")  # no console, no window: nothing above the server to find
CLIENT = os.path.join(HERE, "home_client.py")
user32 = ctypes.windll.user32
user32.GetForegroundWindow.restype = wintypes.HWND
user32.FindWindowW.restype = wintypes.HWND
failed = 0
sys.stdout.reconfigure(encoding="utf-8", errors="replace")
TMP = tempfile.mkdtemp(prefix="dchome_")


def check(cond, what, detail=""):
    global failed
    print(("ok   " if cond else "FAIL ") + what + (f"  [{detail}]" if detail and not cond else ""), flush=True)
    failed += 0 if cond else 1


def fg():
    h = user32.GetForegroundWindow()
    b = ctypes.create_unicode_buffer(512)
    user32.GetWindowTextW(h, b, 512)
    return int(h or 0), b.value


def find(title):
    return int(user32.FindWindowW(None, title) or 0)


def classes(name_part):
    out = []
    P = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    def cb(h, _):
        b = ctypes.create_unicode_buffer(128)
        user32.GetClassNameW(h, b, 128)
        if name_part in b.value:
            out.append(int(h))
        return True

    user32.EnumWindows(P(cb), 0)
    return out


def activate(hwnd, tries=6):
    for _ in range(tries):
        if fg()[0] == hwnd:
            return True
        user32.keybd_event(0x87, 0, 0, 0)  # F24 tap lifts the foreground lock (Alt would open a window menu)
        user32.keybd_event(0x87, 0, 2, 0)
        user32.ShowWindow(hwnd, 9)
        user32.SetForegroundWindow(hwnd)
        time.sleep(0.3)
    return fg()[0] == hwnd


def wait_for(fn, timeout, step=0.3):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(step)
    return None


FORM = r"""
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form; $f.Text='%s'; $f.Width=420; $f.Height=240; $f.StartPosition='Manual'; $f.Location='%s'
[Windows.Forms.Application]::Run($f)
"""
procs = []


def start_form(title, loc):
    p = subprocess.Popen(["powershell", "-NoProfile", "-STA", "-Command", FORM % (title, loc)], creationflags=0x08000000)
    procs.append(p)
    h = wait_for(lambda: find(title), 15)
    time.sleep(1.0)
    return h


def parse_apps(text):
    """-> {hwnd: (exe, title, host)}"""
    out = {}
    for m in re.finditer(r'#(\d+) ([^:\n]+): "([^"\n]*)"([^\n]*)', text):
        out[int(m.group(1))] = (m.group(2), m.group(3), "host: never read" in m.group(4))
    return out


def read_result(path, timeout=90):
    ok = wait_for(lambda: os.path.exists(path) and os.path.getsize(path) > 2, timeout, 0.5)
    if not ok:
        return None
    time.sleep(0.3)
    return json.load(open(path, encoding="utf-8-sig"))


def common_checks(tag, res, host_hwnd, user_wt, other_hosts_ok=False, host_title=None):
    check(res is not None and "error" not in res, f"{tag}: the client ran to the end", repr(res)[:300] if res else "no result")
    if not res or "error" in res:
        return
    apps = parse_apps(res["apps"])
    hosts = [h for h, v in apps.items() if v[2]]
    check(host_hwnd in hosts, f"{tag}: list_apps marks the AI app's window as host", f"hosts={hosts} want {host_hwnd}")
    check(len(hosts) == 1 or other_hosts_ok, f"{tag}: exactly one host window", f"hosts={hosts}")
    for w in user_wt:
        check(w not in hosts, f"{tag}: the user's own Windows Terminal window #{w} is NOT host")
    fa, ff = res["fg_after_focus"], res["fg_after_finish"]
    if isinstance(ff, str):  # the PowerShell GUI host reports titles only
        fa, ff = (0, fa), (host_hwnd, ff) if ff == host_title else (0, ff)
    check("DC-TARGET" in fa[1], f"{tag}: the task moved to DC-TARGET", repr(fa))
    check(ff[0] == host_hwnd, f"{tag}: finish brought the host window to the front", f"fg {ff} want {host_hwnd}; {res['finish']}")
    check("back in" in res["finish"], f"{tag}: the finish message is neutral and truthful", res["finish"])
    check("VS Code" not in res["finish"] and "Claude" not in res["finish"], f"{tag}: no hard-coded app names in it")


def reading_checks(tag, res, title):
    """With the AI app's own window in front, nothing reads it (bug 6)."""
    if not res or "error" in res:
        return
    check(res["fg_after_finish"][1] == title, f"{tag}: the host window is in front for the read checks", repr(res["fg_after_finish"]))
    le = res["list_elements"]
    check("AI app's own window" in le and "[Button]" not in le and "TabItem" not in le, f"{tag}: list_elements refuses the host window", le[:200])
    check("Close Tab" not in res["find"] or "never read" in res["find"] or "Nothing found" in res["find"], f"{tag}: find does not read it", res["find"][:200])
    check("Close Tab" not in res["find"] or "Nothing found" in res["find"], f"{tag}: find('Close Tab') finds no terminal tab button", res["find"][:200])
    ftw = res["find_text_window"]
    check("not read" in ftw and "never read" in ftw, f"{tag}: find_text(window=host) refuses", ftw[:200])
    fts = res["find_text_screen"]
    check("No on-screen text matches" in fts or title not in fts.split("OCR read")[0], f"{tag}: monitor-wide OCR does not see the host window's text", fts[:200])
    check("nothing has keyboard focus" in res["read_text"] or "never read" in res["read_text"], f"{tag}: read_text does not read the host's focused control", res["read_text"][:200])
    d = res["diff"]
    check("Close Tab" not in d and "New Tab" not in d and "TabItem" not in d, f"{tag}: the text observation lists none of the host's controls", d[:300])
    check("SCRIPT FAILED" in res["script"] and "AI app's own window" in res["script"], f"{tag}: a script's list_elements/find do not read it either", res["script"][:200])


user_wt = classes("CASCADIA_HOSTING_WINDOW_CLASS")
print("existing Windows Terminal windows (the user's, off limits):", user_wt)
target = start_form("DC-TARGET", "200,200")
check(bool(target), "DC-TARGET window is up")

try:
    # ---------------- (a) Windows Terminal ----------------
    if "a" in ONLY:
        print("--- (a) client inside a new Windows Terminal window")
        T = "DCHOME-WT-A"
        res_path = os.path.join(TMP, "a.json")
        subprocess.Popen(["wt.exe", "-w", "new", "new-tab", "--title", T, "--suppressApplicationTitle", "--", PY, "-I", CLIENT, res_path, T, "", EXE],
                         creationflags=0x08000000)
        h = wait_for(lambda: find(T), 25)
        check(bool(h), "(a) the new Windows Terminal window is up")
        if h:
            check(h not in user_wt, "(a) it is a new window, not one of the user's")
            for _ in range(20):
                activate(h)
                if os.path.exists(res_path) or fg()[0] == h:
                    time.sleep(0.5)
                    if fg()[0] == h:
                        break
            res = read_result(res_path)
            common_checks("(a)", res, h, user_wt)
            reading_checks("(a)", res, T)
            if find(T):
                user32.PostMessageW(h, 0x10, 0, 0)  # WM_CLOSE: only the window this script created
                time.sleep(1.0)
            check(find(T) == 0, "(a) the test's Windows Terminal window is closed again")
        check(sorted(classes("CASCADIA_HOSTING_WINDOW_CLASS")) == sorted(user_wt), "(a) the user's Windows Terminal windows are all still there, none added or lost")

    # ---------------- (b) classic console ----------------
    if "b" in ONLY:
        print("--- (b) client inside a classic console window (conhost.exe)")
        T = "DCHOME-CON-B"
        res_path = os.path.join(TMP, "b.json")
        subprocess.Popen(["conhost.exe", PY, "-I", CLIENT, res_path, T, "settitle", EXE])
        h = wait_for(lambda: find(T), 25)
        check(bool(h), "(b) the classic console window is up")
        if h:
            for _ in range(20):
                activate(h)
                if fg()[0] == h:
                    time.sleep(0.5)
                    break
            res = read_result(res_path)
            common_checks("(b)", res, h, user_wt)
            reading_checks("(b)", res, T)
            time.sleep(1.5)
            if find(T):
                user32.PostMessageW(h, 0x10, 0, 0)
                time.sleep(1.0)
            check(find(T) == 0, "(b) the test's console window is closed again")

    # ---------------- (c) GUI app ----------------
    if "c" in ONLY:
        print("--- (c) a WinForms GUI app that spawns the server")
        T = "DC-GUIHOST"
        res_path = os.path.join(TMP, "c.json")
        p = subprocess.Popen(["powershell", "-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-File", os.path.join(HERE, "home_gui_host.ps1"), res_path, EXE, T],
                             creationflags=0x08000000)
        procs.append(p)
        h = wait_for(lambda: find(T), 20)
        check(bool(h), "(c) the GUI app's window is up")
        if h:
            for _ in range(6):
                activate(h)
                time.sleep(0.3)
            res = read_result(res_path)
            if res is not None and "form_handle" in res:
                h = int(res["form_handle"])
            common_checks("(c)", res, h, user_wt, host_title=T)

    # ---------------- (e) no AI app in the tree: foreground fallback ----------------
    if "e" in ONLY:
        print("--- (e) no AI app above the server: the window in front at the first action is the fallback")
        T = "DC-FALLBACK"
        fb = start_form(T, "700,120")
        res_path = os.path.join(TMP, "e.json")
        cmd = f'"{PYW}" -I "{CLIENT}" "{res_path}" {T} "{EXE}"'
        subprocess.run(["powershell", "-NoProfile", "-Command",
                        "Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{CommandLine='" + cmd.replace("'", "''") + "'} | Out-Null"],
                       capture_output=True)
        for _ in range(25):
            activate(fb)
            if os.path.exists(res_path):
                break
            time.sleep(0.4)
        res = read_result(res_path)
        check(res is not None and "error" not in res, "(e) the client ran", repr(res)[:300] if res else "no result (WMI-started processes may not reach this desktop)")
        if res and "error" not in res:
            apps = parse_apps(res["apps"])
            hosts = [h for h, v in apps.items() if v[2]]
            check(hosts == [fb], "(e) only the window in front at the first action is marked host (a guess, protected like the AI app's own)", str(hosts))
            check("could not be identified" in res["apps"], "(e) the first result of the task warns that no AI app window was identified", res["apps"][-300:])
            check(res["fg_after_finish"][0] == fb, "(e) finish returned to the window that was in front at the first action", f"{res['fg_after_finish']} want {fb}; {res['finish']}")
            check("back in" in res["finish"], "(e) message", res["finish"])

    # ---------------- (f) homeApp ----------------
    if "f" in ONLY:
        print("--- (f) homeApp in config.json")
        d = os.path.join(TMP, "cfg")
        os.makedirs(d)
        exe2 = os.path.join(d, "desktop-control.exe")
        shutil.copy2(EXE, exe2)
        json.dump({"homeApp": "notepad"}, open(os.path.join(d, "config.json"), "w"))
        np_scope = testutil.NotepadScope()  # the user's own Notepad windows are never touched
        subprocess.Popen(["notepad.exe"])
        np = wait_for(lambda: (np_scope.mine() or [0])[0], 15)
        NPT = testutil.title_of(np) if np else "Untitled - Notepad"
        check(bool(np), "(f) a Notepad window (stands in for the AI app) is up")
        T = "DC-OVERRIDE"
        res_path = os.path.join(TMP, "f.json")
        cmd = f'"{PYW}" -I "{CLIENT}" "{res_path}" "{NPT}" "{exe2}"'
        subprocess.run(["powershell", "-NoProfile", "-Command",
                        "Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{CommandLine='" + cmd.replace("'", "''") + "'} | Out-Null"],
                       capture_output=True)
        for _ in range(25):
            if np:
                activate(np)
            if os.path.exists(res_path):
                break
            time.sleep(0.4)
        res = read_result(res_path)
        if res and "error" not in res and np:
            apps = parse_apps(res["apps"])
            check(apps.get(np, ("", "", False))[2], "(f) homeApp marks the notepad.exe window as host", str(apps.get(np)))
            check(res["fg_after_finish"][0] == np, "(f) finish returned to the homeApp window", f"{res['fg_after_finish']} want {np}")
            check("DC-TARGET" in res["fg_after_focus"][1], "(f) other windows are still ordinary work windows")
        else:
            check(False, "(f) the client ran", repr(res)[:300] if res else "no result")
        if np and testutil.is_window(np):
            user32.PostMessageW(np, 0x10, 0, 0)  # WM_CLOSE to the window this test opened: an empty untitled document closes without a prompt
            time.sleep(1.0)
finally:
    for p in procs:
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(p.pid)], capture_output=True)
    shutil.rmtree(TMP, ignore_errors=True)

print("FAILED" if failed else "ALL OK", f"({failed} failures)")
sys.exit(1 if failed else 0)
