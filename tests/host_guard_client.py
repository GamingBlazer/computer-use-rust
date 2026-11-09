"""Client run INSIDE a fake host (a Windows Terminal or console window that host_guard_test.py opens) to try to
send input to the host window. Everything must be refused and the window must survive. Not a test by itself.

Usage: python -I host_guard_client.py <result.json> <host title> [settitle] <exe>
"""
import ctypes
import json
import os
import re
import sys
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from dcclient import Server  # noqa: E402

user32 = ctypes.windll.user32
user32.GetForegroundWindow.restype = wintypes.HWND
out_path, host_title = sys.argv[1], sys.argv[2]
rest = sys.argv[3:]
if "settitle" in rest:
    ctypes.windll.kernel32.SetConsoleTitleW(host_title)
exe = next((a for a in rest if a.lower().endswith(".exe")), None)


def fg():
    h = user32.GetForegroundWindow()
    b = ctypes.create_unicode_buffer(512)
    user32.GetWindowTextW(h, b, 512)
    return int(h or 0), b.value


res = {}
try:
    end = time.time() + 25
    while time.time() < end and fg()[1] != host_title:
        time.sleep(0.2)
    res["fg_at_start"] = fg()
    s = Server(exe, creationflags=0x08000000)
    apps = s.call("list_apps")[0]
    m = re.search(r'#(\d+) [^\n]*host: never read', apps)
    host = int(m.group(1)) if m else 0
    res["host"] = host
    res["fg_is_host"] = fg()[0] == host
    attempts = {
        "alt+F4 key": ("press_keys", {"keys": ["alt+F4"], "observe": "none"}),
        "alt+shift+F4": ("press_keys", {"keys": ["alt+shift+F4"], "observe": "none"}),
        "ctrl+alt+w": ("press_keys", {"keys": ["ctrl+alt+w"], "observe": "none"}),
        "ctrl+c": ("press_keys", {"keys": ["ctrl+c"], "observe": "none"}),
        "ctrl+d": ("press_keys", {"keys": ["ctrl+d"], "observe": "none"}),
        "alt+space": ("press_keys", {"keys": ["alt+space"], "observe": "none"}),
        "type U+0003": ("type_text", {"text": "\u0003\u0003", "observe": "none"}),
        "type U+0004 and text": ("type_text", {"text": "ab\u0004cd", "observe": "none"}),
        "screenshot of the host window": ("screenshot", {"window": "#%d" % host}),
        "read_text": ("read_text", {"observe": "none"}),
        "list_elements": ("list_elements", {}),
        "ctrl+w": ("press_keys", {"keys": ["ctrl+w"], "observe": "none"}),
        "letter key": ("press_keys", {"keys": ["q"], "observe": "none"}),
        "type_text": ("type_text", {"text": "zzz", "observe": "none"}),
        "click": ("click", {"x": 100, "y": 100, "observe": "none"}),
        "scroll": ("scroll", {"x": 100, "y": 100, "amount": 1, "observe": "none"}),
        "drag": ("drag", {"x": 100, "y": 100, "to_x": 150, "to_y": 150, "observe": "none"}),
        "set_value": ("set_value", {"target": "Text editor", "value": "x", "observe": "none"}),
        "element_action": ("element_action", {"target": "Close Tab", "action": "invoke", "observe": "none"}),
        "click_element": ("click_element", {"target": "Close Tab", "observe": "none"}),
        "run_actions alt+F4": ("run_actions", {"actions": [{"action": "keys", "keys": ["alt+F4"]}], "observe": "none"}),
        "run_script alt+F4": ("run_script", {"script": 'press_keys("alt+F4");', "observe": "none"}),
        "run_script type": ("run_script", {"script": 'type_text("zzz");', "observe": "none"}),
    }
    s.call("screenshot", {"monitor": 1})  # so that x/y have a view; the host is still the window in front
    res["fg_after_shot"] = fg()
    res["refused"] = {}
    for label, (tool, a) in attempts.items():
        if fg()[0] != host:
            res["refused"][label] = "SKIPPED: the host window is no longer in front: %r" % (fg(),)
            continue
        t, _, err = s.call(tool, a)
        res["refused"][label] = t[:400]
    res["alive_after_refusals"] = bool(user32.IsWindow(host))
    # there is no opt-in any more: focusing the host is refused, and typing / keys stay refused after trying it
    t, _, err = s.call("focus_window", {"name": "#%d" % host, "observe": "none"})
    res["focus_host"] = t[:300]
    res["focus_host_error"] = err
    res["fg_after_focus"] = fg()[0] == host
    res["chosen_type"] = s.call("type_text", {"text": "x", "observe": "none"})[0][:300]
    for label, keys in (("alt+F4", ["alt+F4"]), ("ctrl+w", ["ctrl+w"]), ("ctrl+shift+w", ["ctrl+shift+w"]), ("ctrl+q", ["ctrl+q"]), ("ctrl+F4", ["ctrl+F4"]), ("alt+F4 array", ["alt", "F4"]), ("alt+shift+F4", ["alt+shift+F4"])):
        res.setdefault("chosen_close", {})[label] = s.call("press_keys", {"keys": keys, "observe": "none"})[0][:300]
    res["chosen_script_close"] = s.call("run_script", {"script": 'press_keys("alt+F4");', "observe": "none"})[0][:400]
    res["chosen_script_type"] = s.call("run_script", {"script": 'type_text("zzz");', "observe": "none"})[0][:400]
    res["alive_at_end"] = bool(user32.IsWindow(host))
    s.close()
except BaseException as e:  # noqa: BLE001
    res["error"] = repr(e)
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(res, f)
