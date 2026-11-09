"""Client run INSIDE a terminal (or any process tree) by home_test.py: it starts the server, runs a small
task and writes what it saw to a result file. Not a test by itself.

Usage: python -I home_client.py <result.json> <terminal title> [settitle] [exe]
  settitle: set the console title to <terminal title> first (classic console windows)
"""
import ctypes
import json
import os
import sys
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from dcclient import Server  # noqa: E402

user32 = ctypes.windll.user32
user32.GetForegroundWindow.restype = wintypes.HWND

out_path, host_title = sys.argv[1], sys.argv[2]
rest = sys.argv[3:]  # empty arguments get lost on some command lines: pick the options by what they are
settitle = "settitle" in rest
exe = next((a for a in rest if a.lower().endswith(".exe")), None)
if settitle:
    ctypes.windll.kernel32.SetConsoleTitleW(host_title)


def fg():
    h = user32.GetForegroundWindow()
    b = ctypes.create_unicode_buffer(512)
    user32.GetWindowTextW(h, b, 512)
    return int(h or 0), b.value


res = {"client_pid": os.getpid()}
try:
    # the test activates this terminal window; wait until it is in front
    end = time.time() + 25
    while time.time() < end and fg()[1] != host_title:
        time.sleep(0.2)
    res["fg_at_start"] = fg()
    s = Server(exe, creationflags=0x08000000)  # no console window of its own for the server
    res["apps"] = s.call("list_apps")[0]
    res["status"] = s.call("status")[0]
    res["focus"] = s.call("focus_window", {"name": "DC-TARGET", "observe": "none"})[0]
    res["fg_after_focus"] = fg()
    res["finish"] = s.call("finish", {"summary": "home test"})[0]
    time.sleep(1.0)
    res["fg_after_finish"] = fg()
    # with the AI app's own window in front, nothing may read it (bug 6)
    res["list_elements"] = s.call("list_elements")[0]
    res["find"] = s.call("find", {"target": "Close Tab"})[0]
    res["find_text_window"] = s.call("find_text", {"text": host_title, "window": host_title})[0]
    res["find_text_screen"] = s.call("find_text", {"text": host_title})[0]
    res["read_text"] = s.call("read_text")[0]
    res["diff"] = s.call("run_actions", {"actions": [{"action": "wait", "seconds": 0.2}]})[0]
    res["script"] = s.call("run_script", {"script": 'let e = list_elements(); let f = find("Close Tab"); #{n: e.len(), found: f.found}', "observe": "none"})[0]
    res["fg_end"] = fg()
    s.call("finish", {"summary": "home test done"})
    s.close()
except BaseException as e:  # noqa: BLE001
    res["error"] = repr(e)
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(res, f)
