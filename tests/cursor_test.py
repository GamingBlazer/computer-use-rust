"""The real Windows cursor must stay visible whatever happens to the server.

  1. control: the detector sees a blank arrow cursor (SetSystemCursor with a blank one), then the scheme is reloaded
  2. a task runs (a click on a WinForms window of this test), mid-task the cursor is showing and its arrow is not blank
  3. the server is force-killed mid-task (taskkill /F /PID, no cursor manager exists any more): the cursor is still showing
  4. no --cursor-manager watchdog process was ever spawned
  5. a stale `desktop-control.exe --cursor-manager 1` exits at once and leaves the cursor visible
  6. (optional, saves images) with DESKTOP_CONTROL_CAPTURABLE=1 the halo shows in a monitor screenshot; without it, it does not

SPI_SETCURSORS is called before the script ends, as a precaution.
Usage: python -I cursor_test.py [path\\to\\desktop-control.exe]
"""
import base64
import ctypes
import ctypes.wintypes as wt
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from dcclient import Server, Checker, M  # noqa: E402

EXE = sys.argv[1] if len(sys.argv) > 1 else next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe"))
EVID = os.path.join(HERE, "..", "bench", "evidence")
check = Checker()
user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)

SPI_SETCURSORS = 0x0057
IDC_ARROW = 32512
CURSOR_SHOWING = 1


class CURSORINFO(ctypes.Structure):
    _fields_ = [("cbSize", wt.DWORD), ("flags", wt.DWORD), ("hCursor", wt.HANDLE), ("ptScreenPos", wt.POINT)]


class ICONINFO(ctypes.Structure):
    _fields_ = [("fIcon", wt.BOOL), ("xHotspot", wt.DWORD), ("yHotspot", wt.DWORD), ("hbmMask", wt.HBITMAP), ("hbmColor", wt.HBITMAP)]


class BITMAP(ctypes.Structure):
    _fields_ = [("bmType", wt.LONG), ("bmWidth", wt.LONG), ("bmHeight", wt.LONG), ("bmWidthBytes", wt.LONG),
                ("bmPlanes", wt.WORD), ("bmBitsPixel", wt.WORD), ("bmBits", ctypes.c_void_p)]


user32.LoadCursorW.restype = wt.HANDLE
user32.LoadCursorW.argtypes = [wt.HINSTANCE, ctypes.c_void_p]
user32.GetIconInfo.argtypes = [wt.HANDLE, ctypes.POINTER(ICONINFO)]
gdi32.GetObjectW.argtypes = [wt.HANDLE, ctypes.c_int, ctypes.c_void_p]
gdi32.GetBitmapBits.argtypes = [wt.HBITMAP, wt.LONG, ctypes.c_void_p]
gdi32.DeleteObject.argtypes = [wt.HANDLE]


def showing():
    ci = CURSORINFO()
    ci.cbSize = ctypes.sizeof(CURSORINFO)
    user32.GetCursorInfo(ctypes.byref(ci))
    return bool(ci.flags & CURSOR_SHOWING)


def bits(hbm):
    bm = BITMAP()
    gdi32.GetObjectW(hbm, ctypes.sizeof(BITMAP), ctypes.byref(bm))
    n = bm.bmWidthBytes * bm.bmHeight
    buf = (ctypes.c_ubyte * n)()
    gdi32.GetBitmapBits(hbm, n, buf)
    return bm, bytes(buf)


def arrow_visible_pixels():
    """How many pixels of the current system arrow cursor are visible (0 = a blank cursor)."""
    h = user32.LoadCursorW(None, IDC_ARROW)
    info = ICONINFO()
    if not h or not user32.GetIconInfo(h, ctypes.byref(info)):
        return 0
    try:
        mask_bm, mask = bits(info.hbmMask)
        if info.hbmColor:
            col_bm, col = bits(info.hbmColor)
            if col_bm.bmBitsPixel == 32 and any(col[3::4]):
                return sum(1 for a in col[3::4] if a)
            # no alpha: visible where the AND mask bit is 0 (opaque) or the colour is not black
            return sum(1 for i in range(0, len(col), max(1, col_bm.bmBitsPixel // 8)) if col[i:i + 3] != b"\x00\x00\x00") or sum(
                8 - bin(b).count("1") for b in mask)
        # monochrome: top half AND mask, bottom half XOR mask. Visible = AND bit 0 or XOR bit 1
        half = len(mask) // 2
        and_, xor = mask[:half], mask[half:]
        return sum(8 - bin(b).count("1") for b in and_) + sum(bin(b).count("1") for b in xor)
    finally:
        if info.hbmMask:
            gdi32.DeleteObject(info.hbmMask)
        if info.hbmColor:
            gdi32.DeleteObject(info.hbmColor)


def reload_scheme():
    user32.SystemParametersInfoW(SPI_SETCURSORS, 0, None, 0)


def blank_arrow():
    """What the old build did: replace the system arrow with a fully transparent cursor."""
    and_ = (ctypes.c_ubyte * 128)(*([0xFF] * 128))
    xor = (ctypes.c_ubyte * 128)(*([0x00] * 128))
    user32.CreateCursor.restype = wt.HANDLE
    user32.CreateCursor.argtypes = [wt.HINSTANCE, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p]
    h = user32.CreateCursor(None, 0, 0, 32, 32, and_, xor)
    user32.SetSystemCursor.argtypes = [wt.HANDLE, wt.DWORD]
    user32.SetSystemCursor(h, IDC_ARROW)


def children_of(pid):
    out = subprocess.run(
        ["powershell", "-NoProfile", "-Command", f"Get-CimInstance Win32_Process -Filter 'ParentProcessId={pid}' | ForEach-Object {{ $_.CommandLine }}"],
        capture_output=True, text=True).stdout
    return [l for l in out.splitlines() if l.strip()]


def start_form():
    env = dict(os.environ)
    return subprocess.Popen(["powershell", "-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-File", os.path.join(HERE, "ocr_form.ps1")], env=env)


def wait_form(s):
    for _ in range(50):
        t, _, _ = s.call("list_apps")
        if "DC OCR Test Form" in t:
            time.sleep(0.8)
            return True
        time.sleep(0.4)
    return False


def save_shot(s, path):
    m = s.wait(s.start_call("screenshot", {"monitor": 1, "force": True}), 30)
    for c in m["result"]["content"]:
        if c["type"] == "image":
            with open(path, "wb") as f:
                f.write(base64.b64decode(c["data"]))
            return True
    return False


def s_probe():
    sv = Server(EXE)
    servers.append(sv)
    return sv


os.makedirs(EVID, exist_ok=True)
form = None
servers = []
try:
    reload_scheme()
    user32.mouse_event(1, 1, 0, 0, 0)  # a 1 px nudge and back: Windows hides the pointer while nothing moves it
    user32.mouse_event(1, -1, 0, 0, 0)
    time.sleep(0.2)
    base = arrow_visible_pixels()
    check(showing() and base > 20, f"baseline: cursor showing, arrow has {base} visible pixels")

    blank_arrow()
    check(arrow_visible_pixels() == 0, "control: the detector sees a blank arrow cursor")
    reload_scheme()
    check(arrow_visible_pixels() == base, f"control: reloading the scheme restores it ({arrow_visible_pixels()} px)")

    form = start_form()
    s = Server(EXE)
    servers.append(s)
    check(wait_form(s), "test form is up")
    s.call("screenshot", {"window": "DC OCR Test Form"})
    t, _, err = s.call("click", {"x": 300, "y": 300, "observe": "none"})
    check(not err, "a click ran (task active, indicator shown)", t[:100])
    time.sleep(0.6)
    mid = arrow_visible_pixels()
    check(showing() and mid == base, f"mid-task: cursor showing, arrow {mid} px (baseline {base})")
    kids = children_of(s.p.pid)
    check(not any("--cursor-manager" in k for k in kids), f"no cursor-manager watchdog spawned (children: {kids})")

    # force-kill the server mid-task
    s.call("click", {"x": 310, "y": 310, "observe": "none"})
    subprocess.run(["taskkill", "/F", "/PID", str(s.p.pid)], capture_output=True)
    time.sleep(1.0)
    after = arrow_visible_pixels()
    check(showing() and after == base, f"after taskkill /F of the server mid-task: cursor showing, arrow {after} px")

    # a stale watchdog invocation of an old build must be harmless
    r = subprocess.run([EXE, "--cursor-manager", "1"], capture_output=True, timeout=10)
    check(r.returncode == 0 and showing() and arrow_visible_pixels() == base, "stale --cursor-manager call exits at once and changes nothing")

    # start-up repair: blank the arrow, start a server, the arrow is back
    blank_arrow()
    check(arrow_visible_pixels() == 0, "blanked again to simulate a system left broken by an old build")
    s2 = Server(EXE)
    servers.append(s2)
    s2.call("status")
    time.sleep(0.3)
    check(arrow_visible_pixels() == base, f"server start-up repaired it ({arrow_visible_pixels()} px)")
    s2.call("finish", {"summary": "x"})
    s2.close()

    # halo / ripple: visible when the overlay is capturable (debug switch), absent from normal screenshots.
    # A white full-screen window of this test covers everything else, so the images hold nothing private.
    form.kill()
    form = subprocess.Popen(["powershell", "-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-File", os.path.join(HERE, "cover_form.ps1")])
    for _ in range(40):
        if "DC Cover Form" in s_probe().call("list_apps")[0]:
            break
        time.sleep(0.4)
    time.sleep(1.0)
    for label, env_val in (("capturable", "1"), ("normal", None)):
        env = dict(os.environ)
        if env_val:
            env["DESKTOP_CONTROL_CAPTURABLE"] = env_val
        else:
            env.pop("DESKTOP_CONTROL_CAPTURABLE", None)
        srv = Server.__new__(Server)
        import queue, threading
        srv.p = subprocess.Popen([EXE], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0, env=env)
        srv.q = queue.Queue()
        threading.Thread(target=srv._read, daemon=True).start()
        srv.n = 0
        servers.append(srv)
        srv.call("screenshot", {"window": "DC Cover Form"})
        srv.call("click", {"x": 400, "y": 300, "observe": "none"})
        time.sleep(0.5)
        p = os.path.join(EVID, f"cursor_halo_{label}.jpg")
        ok = save_shot(srv, p)
        check(ok, f"saved monitor screenshot ({label}) -> {p}")
        srv.call("finish", {"summary": "x"})
        srv.close()
    check(showing(), "cursor still showing at the end")
finally:
    for sv in servers:
        try:
            sv.p.kill()
        except Exception:
            pass
    if form:
        form.kill()
    reload_scheme()

print()
print("ALL OK (0 failures)" if check.fails == 0 else f"FAILED ({check.fails} failures)")
sys.exit(1 if check.fails else 0)
