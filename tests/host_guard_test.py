"""Host-window guard: keyboard (close, quit and interrupt chords included, and chords that merely contain one),
typing (control characters like U+0003 too), clicks, scrolling, dragging, set_value, element_action, reading and
run_actions / run_script steps are refused while the window in front is the AI app's own window. There is no opt-in:
focus_window on the host is refused, and nothing is allowed afterwards either. The "AI app" is a Windows Terminal window (and a classic console window) that THIS test opens and
runs its client in; the user's own terminal windows are never involved. The test proves the fake host survives.

Usage: python -I host_guard_test.py [path\\to\\desktop-control.exe]
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import testutil  # noqa: E402
from testutil import u as user32  # noqa: E402

EXE = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.abspath(next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe")))
PY = sys.executable
CLIENT = os.path.join(HERE, "host_guard_client.py")
TMP = tempfile.mkdtemp(prefix="dcguard_")
failed = 0
sys.stdout.reconfigure(encoding="utf-8", errors="replace")


def check(cond, what, detail=""):
    global failed
    print(("ok   " if cond else "FAIL ") + what + (f"  [{detail}]" if detail and not cond else ""), flush=True)
    failed += 0 if cond else 1


def find(title):
    return int(user32.FindWindowW(None, title) or 0)


def wait_for(fn, timeout, step=0.3):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(step)
    return None


def activate(hwnd):
    for _ in range(6):
        if testutil.fg_hwnd() == hwnd:
            return True
        user32.keybd_event(0x87, 0, 0, 0)
        user32.keybd_event(0x87, 0, 2, 0)
        user32.ShowWindow(hwnd, 9)
        user32.SetForegroundWindow(hwnd)
        time.sleep(0.3)
    return testutil.fg_hwnd() == hwnd


def run_case(tag, launch, title):
    print(f"--- {tag}")
    res_path = os.path.join(TMP, tag.split()[0] + ".json")
    launch(res_path)
    h = wait_for(lambda: find(title), 25)
    check(bool(h), f"{tag}: the fake host window is up")
    if not h:
        return
    for _ in range(25):
        activate(h)
        if os.path.exists(res_path):
            break
        if testutil.fg_hwnd() == h:
            time.sleep(0.5)
            break
    ok = wait_for(lambda: os.path.exists(res_path) and os.path.getsize(res_path) > 2, 120, 0.5)
    check(bool(ok), f"{tag}: the client finished", "no result file")
    if not ok:
        return
    time.sleep(0.3)
    r = json.load(open(res_path, encoding="utf-8-sig"))
    check("error" not in r, f"{tag}: the client ran without errors", r.get("error", ""))
    check(r.get("host") == h, f"{tag}: the fake host is the window marked host", f"{r.get('host')} vs {h}")
    check(r.get("fg_is_host"), f"{tag}: it was in front when the attempts started")
    for label, text in r.get("refused", {}).items():
        check(("AI app's own" in text and ("bring the target app to the front first" in text or "never read" in text)) or (label == "read_text" and "nothing has keyboard focus" in text), f"{tag}: {label} is refused with the host message", text[:160])
    check(r.get("alive_after_refusals"), f"{tag}: the host window is still open after every refused attempt")
    check(r.get("focus_host_error") and "AI app's own window" in r.get("focus_host", "") and "never focused" in r.get("focus_host", ""), f"{tag}: focus_window on the host is refused (no opt-in)", r.get("focus_host", ""))
    check("AI app's own window" in r.get("chosen_type", ""), f"{tag}: after trying to choose it, typing is still refused", r.get("chosen_type", ""))
    for label, text in r.get("chosen_close", {}).items():
        check("AI app's own window" in text, f"{tag}: {label} is still refused after the attempt to choose the host", text[:160])
    st = r.get("chosen_script_close", "")
    check("AI app's own window" in st, f"{tag}: a script's press_keys('alt+F4') is refused", st[:160])
    st = r.get("chosen_script_type", "")
    check("AI app's own window" in st, f"{tag}: a script's type_text is refused", st[:160])
    check(r.get("alive_at_end"), f"{tag}: the host window survived everything")
    if testutil.is_window(h):
        testutil.post_close(h)  # WM_CLOSE to the window this test opened
        time.sleep(1.2)
    check(not testutil.is_window(h), f"{tag}: the test closed its own fake host again")


user_wt = [h for h in __import__("testutil").windows_of_exe("windowsterminal.exe")]
try:
    T1 = "DCGUARD-WT"
    run_case("wt Windows Terminal window",
             lambda rp: subprocess.Popen(["wt.exe", "-w", "new", "new-tab", "--title", T1, "--suppressApplicationTitle", "--", PY, "-I", CLIENT, rp, T1, EXE], creationflags=0x08000000),
             T1)
    T2 = "DCGUARD-CON"
    run_case("con classic console window",
             lambda rp: subprocess.Popen(["conhost.exe", PY, "-I", CLIENT, rp, T2, "settitle", EXE]),
             T2)
finally:
    shutil.rmtree(TMP, ignore_errors=True)
now = testutil.windows_of_exe("windowsterminal.exe")
check(sorted(now) == sorted(user_wt), "the user's own Windows Terminal windows are all still there", f"{user_wt} -> {now}")
print("FAILED" if failed else "ALL OK", f"({failed} failures)")
sys.exit(1 if failed else 0)
