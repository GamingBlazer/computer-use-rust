"""Safety helpers shared by the tests that drive real windows.

Rules every test follows (a stray real-keyboard alt+F4 once closed the user's own terminal):
  * only windows that the test created are ever closed, by their exact handle (WM_CLOSE) or by a PID the
    test started; never `taskkill /IM <name>`;
  * a key press is only sent after checking, in the same call, that the foreground window is the test's own;
  * finish / turn-ended is never relied on to bring the user's real terminal to the front before keyboard
    input: tests that call finish use their own fake host window or a client that lives in a test window.
"""
import ctypes
import time
from ctypes import wintypes

u = ctypes.windll.user32
k = ctypes.windll.kernel32
u.GetForegroundWindow.restype = wintypes.HWND
u.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
u.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
u.IsWindow.argtypes = [wintypes.HWND]
u.IsWindowVisible.argtypes = [wintypes.HWND]
u.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
k.OpenProcess.restype = wintypes.HANDLE


class Abort(SystemExit):
    pass


def fg_hwnd():
    return int(u.GetForegroundWindow() or 0)


def title_of(hwnd):
    b = ctypes.create_unicode_buffer(512)
    u.GetWindowTextW(hwnd, b, 512)
    return b.value


def exe_of(hwnd):
    pid = wintypes.DWORD()
    u.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    h = k.OpenProcess(0x1000, False, pid.value)
    if not h:
        return ""
    buf = ctypes.create_unicode_buffer(1024)
    n = wintypes.DWORD(1024)
    k.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(n))
    k.CloseHandle(h)
    return buf.value.rsplit("\\", 1)[-1].lower()


def fg_exe():
    return exe_of(fg_hwnd())


def is_window(hwnd):
    return bool(hwnd) and bool(u.IsWindow(hwnd))


def windows_of_exe(exe, ignore_titles=("PopupHost",)):
    """Visible titled top-level windows of an exe (lower-case name)."""
    out = []
    P = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    def cb(h, _):
        if u.IsWindowVisible(h) and exe_of(h) == exe:
            t = title_of(h)
            if t and t not in ignore_titles:
                out.append(int(h))
        return True

    u.EnumWindows(P(cb), 0)
    return out


def post_close(hwnd):
    """WM_CLOSE to one exact window (never a key press)."""
    u.PostMessageW(hwnd, 0x0010, 0, 0)


def require_fg(hwnd=None, exe=None):
    """Abort unless the foreground window is the expected one (a handle, or an exe name)."""
    cur = fg_hwnd()
    if hwnd is not None and cur != hwnd:
        raise Abort("ABORT: foreground is %r (#%d), not the test's window #%d; refusing to send keys" % (title_of(cur).encode("ascii", "replace").decode(), cur, hwnd))
    if exe is not None and exe_of(cur) != exe:
        raise Abort("ABORT: foreground is %r (%s), not %s; refusing to send keys" % (title_of(cur).encode("ascii", "replace").decode(), exe_of(cur), exe))


def press(server, keys, hwnd=None, exe=None, **kw):
    require_fg(hwnd, exe)
    args = {"keys": keys, "observe": "none"}
    args.update(kw)
    return server.call("press_keys", args)


class NotepadScope:
    """Notepad windows that exist before the test starts are the user's and are never touched."""

    def __init__(self):
        self.before = set(windows_of_exe("notepad.exe"))

    def mine(self):
        return [h for h in windows_of_exe("notepad.exe") if h not in self.before]

    def close_mine(self, server):
        """Empty and close the windows this test opened: handle-checked keys, then WM_CLOSE, then 'Don't save'."""
        for _ in range(8):
            mine = self.mine()
            if not mine:
                return True
            h = mine[0]
            server.call("focus_window", {"name": "#%d" % h, "observe": "none"})
            time.sleep(0.5)
            if fg_hwnd() == h:
                press(server, ["ctrl+a"], hwnd=h)
                press(server, ["delete"], hwnd=h)
                time.sleep(3.0)  # Notepad saves its tab state a moment after an edit; close only once the emptied state is stored
            post_close(h)
            time.sleep(1.2)
            if is_window(h) and fg_hwnd() == h:
                server.call("click_element", {"target": "Don't save", "observe": "none"})
                time.sleep(1.0)
        return not self.mine()
