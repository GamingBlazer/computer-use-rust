"""Equivalent of ..\\e2e.py without the MCP Python SDK (not installed here): the same flow over raw
stdio JSON-RPC with the legacy initialize handshake, as Claude Code's older protocol does:
list_apps, open_app, screenshot(window, include_elements), list_elements ids, set_value, read_text,
element_action focus, press_keys, click, finish. Notepad is emptied and closed without saving.

Usage: python e2e_stdio.py [path\\to\\desktop-control.exe]
"""
import json
import os
import queue
import re
import subprocess
import sys
import threading
import time

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import testutil  # noqa: E402
EXE = sys.argv[1] if len(sys.argv) > 1 else next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe"))

p = subprocess.Popen([EXE], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0)
q = queue.Queue()
threading.Thread(target=lambda: [q.put(json.loads(l)) for l in p.stdout if l.strip()], daemon=True).start()
n = 0


def rpc(method, params=None):
    global n
    n += 1
    p.stdin.write((json.dumps({"jsonrpc": "2.0", "id": n, "method": method, "params": params or {}}) + "\n").encode())
    p.stdin.flush()
    while True:
        m = q.get(timeout=60)
        if m.get("id") == n:
            return m


def call(_tool, **args):
    r = rpc("tools/call", {"name": _tool, "arguments": args})["result"]
    text = "\n".join(c["text"] for c in r["content"] if c["type"] == "text")
    return r, text, any(c["type"] == "image" for c in r["content"])


scope = testutil.NotepadScope()  # Notepad windows that exist now are the user's: never touched
NP = None
rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "1"}})
p.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
tools = {t["name"] for t in rpc("tools/list")["result"]["tools"]}
need = {"screenshot", "set_value", "read_text", "list_elements", "list_apps", "element_action", "finish"}
assert need <= tools, need - tools
try:
    r, t, _ = call("list_apps", query="notepad")
    assert not r["isError"] and "Notepad" in t, t
    print("list_apps:", t.splitlines()[-1])
    r, t, _ = call("open_app", name="Notepad", screenshot_after=False)
    assert not r["isError"] and "in front" in t, t
    print("open_app:", t.splitlines()[0])
    time.sleep(0.8)
    assert scope.mine(), "no new Notepad window"
    NP = scope.mine()[0]
    r, t, img = call("screenshot", window="#%d" % NP, include_elements=True)
    assert not r["isError"] and img and "only the window" in t and "Window:" in t, t
    print("screenshot+elements:", t.splitlines()[0])
    m = re.search(r"^(\d+)\. *\[(?:Document|Edit)\]", t, re.M)
    assert m, "no editor control listed"
    editor = m.group(1)
    r, t, _ = call("set_value", target=editor, value="hello from claude")
    assert not r["isError"], t
    print("set_value:", t.splitlines()[0])
    r, t, _ = call("read_text", target=editor)
    assert not r["isError"] and "hello from claude" in t, t
    assert r["structuredContent"]["value"] == "hello from claude"
    print("read_text:", t.replace("\n", " | "))
    r, t, _ = call("element_action", target=editor, action="focus", screenshot_after=False)
    assert not r["isError"] and "focus" in t, t
    print("element_action:", t.splitlines()[0])
    testutil.require_fg(hwnd=NP)
    r, t, _ = call("press_keys", keys=["ctrl+End"], screenshot_after=False)
    assert not r["isError"], t
    print("press_keys:", t.splitlines()[0])
    r, t, img = call("click", x=420, y=420)  # inside the editor (window-view coordinates; (20,20) can hit the window's icon or a tab button); text-first result, no image by default
    assert not r["isError"] and not img and "Active window now" in t, t
    print("click:", t.splitlines()[0])
    r, t, img = call("click", x=420, y=420, observe="both")
    assert not r["isError"] and img, t
    print("click observe=both: image returned")
    call("element_action", target=editor, action="focus", observe="none")
    testutil.require_fg(hwnd=NP)
    call("press_keys", keys=["ctrl+a"], observe="none")  # leave nothing for Notepad to restore
    testutil.require_fg(hwnd=NP)
    call("press_keys", keys=["delete"], observe="none")
    time.sleep(3)  # let Notepad save the emptied tab state before it closes
    testutil.post_close(NP)  # WM_CLOSE to this test's own window only (never a real alt+F4)
    time.sleep(1.5)
    r, t, _ = call("finish", summary="e2e check")
    print("finish:", t)
finally:
    time.sleep(1.0)
    for h in scope.mine():  # still open (a failed run): close only the window this test opened
        testutil.post_close(h)
        time.sleep(1.0)
        if testutil.is_window(h) and testutil.fg_hwnd() == h:
            call("click_element", target="Don't save", observe="none")
    try:
        p.stdin.close()
        p.wait(timeout=5)
    except Exception:
        p.kill()
print("e2e ok")
