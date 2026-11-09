"""Protocol test for desktop-control.exe over stdio, no MCP SDK needed.

  (a) MCP 2026-07-28 path: server/discover, tools/list and tools/call with
      _meta.io.modelcontextprotocol/protocolVersion and no initialize
  (b) legacy path: initialize -> notifications/initialized -> tools/list -> tools/call
  (c) cancelling a long wait_for: no response for the cancelled request, the next one is answered
  (d) notifications/progress for run_actions when the request has a progressToken

Usage:  python mcp_stdio_test.py [path\\to\\desktop-control.exe]
        (default: ..\\desktop-control.exe next to this folder)
Touches the screen only through the tools it calls (status, wait_for on a window that does not
exist, wait steps, finish); nothing is typed or clicked.
"""
import json
import os
import queue
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
EXE = sys.argv[1] if len(sys.argv) > 1 else next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe"))
META = "io.modelcontextprotocol/protocolVersion"
MODERN = "2026-07-28"


class Server:
    def __init__(self):
        self.p = subprocess.Popen([EXE], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0)
        self.q = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.n = 0

    def _read(self):
        for line in self.p.stdout:
            line = line.strip()
            if line:
                self.q.put(json.loads(line))
        self.q.put(None)

    def send(self, obj):
        self.p.stdin.write((json.dumps(obj) + "\n").encode())
        self.p.stdin.flush()

    def request(self, method, params=None, id=None):
        if id is None:
            self.n += 1
            id = self.n
        msg = {"jsonrpc": "2.0", "id": id, "method": method}
        if params is not None:
            msg["params"] = params
        self.send(msg)
        return id

    def notify(self, method, params=None):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        self.send(msg)

    def recv(self, timeout=20):
        return self.q.get(timeout=timeout)

    def wait_response(self, id, timeout=20, collect=None):
        end = time.time() + timeout
        while time.time() < end:
            try:
                m = self.q.get(timeout=max(0.05, end - time.time()))
            except queue.Empty:
                break
            if m is None:
                raise AssertionError("server exited")
            if m.get("id") == id and ("result" in m or "error" in m):
                return m
            if collect is not None:
                collect.append(m)
        raise AssertionError(f"no response to id {id} within {timeout}s")

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def meta(**extra):
    m = {META: MODERN, "io.modelcontextprotocol/clientInfo": {"name": "stdio-test", "version": "1"}, "io.modelcontextprotocol/clientCapabilities": {}}
    m.update(extra)
    return m


def check(cond, what):
    print(("ok   " if cond else "FAIL ") + what)
    if not cond:
        check.failed += 1


check.failed = 0

# ---------------------------------------------------------------- (a) modern, no initialize
s = Server()
r = s.wait_response(s.request("tools/list", {}))  # no _meta, no initialize -> must be refused
check(r.get("error", {}).get("code") == -32602, "(a) tools/list without _meta and without initialize -> -32602")

r = s.wait_response(s.request("tools/list", {"_meta": {META: "1900-01-01"}}))
check(r.get("error", {}).get("code") == -32022 and r["error"]["data"]["requested"] == "1900-01-01"
      and MODERN in r["error"]["data"]["supported"], "(a) unsupported version -> -32022 with supported list")

r = s.wait_response(s.request("server/discover", {"_meta": meta()}))["result"]
check(r["resultType"] == "complete", "(a) discover resultType complete")
check(r["supportedVersions"][0] == MODERN and "2025-06-18" in r["supportedVersions"] and "2025-11-25" in r["supportedVersions"]
      and "2025-03-26" in r["supportedVersions"], f"(a) discover supportedVersions {r['supportedVersions']}")
check("tools" in r["capabilities"], "(a) discover capabilities.tools")
check(r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"] == "desktop-control", "(a) discover serverInfo in _meta")
check(len(r.get("instructions", "")) > 200, "(a) discover instructions")
check(isinstance(r["ttlMs"], int) and r["ttlMs"] >= 0 and r["cacheScope"] in ("public", "private"), "(a) discover ttlMs/cacheScope")

r = s.wait_response(s.request("tools/list", {"_meta": meta()}))["result"]
tools = {t["name"]: t for t in r["tools"]}
check(len(tools) == 22, f"(a) tools/list has {len(tools)} tools (22 expected)")
check(r["resultType"] == "complete" and r["ttlMs"] > 0 and r["cacheScope"] == "public", "(a) tools/list ttlMs/cacheScope/resultType")
check(all(t.get("title") for t in tools.values()), "(a) every tool has a title")
ro = {n for n, t in tools.items() if t["annotations"].get("readOnlyHint")}
check(ro == {"screenshot", "list_apps", "list_elements", "read_text", "status", "find", "find_text"}, f"(a) readOnlyHint tools {sorted(ro)}")
check(all(t["annotations"].get("destructiveHint") is False for n, t in tools.items() if n in ro), "(a) read-only tools destructiveHint false")
with_schema = {n for n, t in tools.items() if "outputSchema" in t}
check(with_schema == {"list_apps", "list_elements", "status", "read_text"}, f"(a) outputSchema on {sorted(with_schema)}")
for n in ("click", "type_text", "press_keys", "scroll"):
    check("background" in tools[n]["inputSchema"]["properties"], f"(a) {n} has background")
for n in ("click", "drag", "scroll", "type_text", "press_keys", "focus_window", "move_window", "open_app", "click_element",
          "set_value", "element_action", "run_actions"):
    check("observe" in tools[n]["inputSchema"]["properties"] and "screenshot_after" in tools[n]["inputSchema"]["properties"],
          f"(a) {n} has observe + screenshot_after")
check("size" in tools["screenshot"]["inputSchema"]["properties"] and "force" in tools["screenshot"]["inputSchema"]["properties"],
      "(a) screenshot has size + force")

r = s.wait_response(s.request("tools/call", {"name": "status", "arguments": {}, "_meta": meta()}))["result"]
check(r["resultType"] == "complete" and r["isError"] is False, "(a) tools/call status complete")
check(r["content"][0]["type"] == "text" and "desktop-control" in r["content"][0]["text"], "(a) status text content kept")
check(r["structuredContent"]["server"] == "desktop-control" and r["structuredContent"]["monitors"], "(a) status structuredContent")

r = s.wait_response(s.request("tools/call", {"name": "list_apps", "arguments": {}, "_meta": meta()}))["result"]
check(isinstance(r["structuredContent"]["windows"], list), "(a) list_apps structuredContent.windows")

r = s.wait_response(s.request("tools/call", {"name": "nope", "arguments": {}, "_meta": meta()}))
check(r.get("error", {}).get("code") == -32602, "(a) unknown tool -> -32602")
r = s.wait_response(s.request("tools/call", {"name": "click", "arguments": {}, "_meta": meta()}))["result"]
check(r["isError"] is True, "(a) tool failure -> isError true")

# ---------------------------------------------------------------- (c) cancel a long wait_for (same modern process)
rid = s.request("tools/call", {"name": "run_actions", "_meta": meta(progressToken="cancel-1"),
                               "arguments": {"actions": [{"action": "wait_for", "name": "zz-no-such-window-qq", "timeout": 40, "scope": "window"}]}}, id=700)
time.sleep(1.5)
t0 = time.time()
s.notify("notifications/cancelled", {"requestId": 700, "reason": "test"})
got = []
r = s.wait_response(s.request("tools/call", {"name": "status", "arguments": {}, "_meta": meta()}), timeout=10, collect=got)
took = time.time() - t0
check("result" in r, f"(c) next request answered after the cancel ({took:.1f}s)")
check(took < 5, "(c) the long wait stopped promptly (status answered in < 5 s, the wait_for had 40 s)")
check(not any(m.get("id") == 700 for m in got), "(c) no response was sent for the cancelled request")
time.sleep(0.5)
extra = []
while not s.q.empty():
    extra.append(s.q.get())
check(not any(m and m.get("id") == 700 for m in extra), "(c) still no response for the cancelled request afterwards")

# ---------------------------------------------------------------- (d) progress
got = []
steps = [{"action": "wait", "seconds": 0.2}] * 3
r = s.wait_response(s.request("tools/call", {"name": "run_actions", "_meta": meta(progressToken="p-1"),
                                             "arguments": {"actions": steps}}), timeout=20, collect=got)
prog = [m for m in got if m.get("method") == "notifications/progress"]
vals = [m["params"]["progress"] for m in prog]
check(len(prog) == 3 and vals == sorted(vals) and len(set(vals)) == 3, f"(d) 3 increasing progress notifications {vals}")
check(all(m["params"]["progressToken"] == "p-1" and m["params"].get("total") == 3 for m in prog), "(d) token and total echoed")
check("result" in r and r["result"]["isError"] is False, "(d) run_actions result after the progress")
got = []
r = s.wait_response(s.request("tools/call", {"name": "run_actions", "_meta": meta(), "arguments": {"actions": [{"action": "wait", "seconds": 0.1}]}}), collect=got)
check(not [m for m in got if m.get("method") == "notifications/progress"], "(d) no progress without a progressToken")

s.wait_response(s.request("tools/call", {"name": "finish", "arguments": {"summary": "protocol test"}, "_meta": meta()}))
s.close()

# ---------------------------------------------------------------- (b) legacy handshake
s = Server()
r = s.wait_response(s.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "legacy", "version": "1"}}))["result"]
check(r["protocolVersion"] == "2025-06-18" and r["serverInfo"]["name"] == "desktop-control" and "tools" in r["capabilities"]
      and r.get("instructions"), "(b) initialize 2025-06-18 answered")
s.notify("notifications/initialized")
r = s.wait_response(s.request("tools/list", {}))["result"]
check(len(r["tools"]) == 22 and "resultType" not in r, "(b) tools/list after initialize, no _meta needed")
r = s.wait_response(s.request("tools/call", {"name": "status", "arguments": {}}))["result"]
check(r["isError"] is False and "desktop-control" in r["content"][0]["text"], "(b) tools/call status")
check("structuredContent" in r, "(b) structuredContent also for legacy clients")
r = s.wait_response(s.request("ping"))
check(r.get("result") == {}, "(b) ping")
r = s.wait_response(s.request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "legacy", "version": "1"}}))["result"]
check(r["protocolVersion"] == "2025-11-25", "(b) initialize 2025-11-25 echoed")
r = s.wait_response(s.request("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "legacy", "version": "1"}}))["result"]
check(r["protocolVersion"] == "2025-03-26", "(b) initialize 2025-03-26 echoed")
r = s.wait_response(s.request("server/discover", {"_meta": meta()}))["result"]
check(r["supportedVersions"][0] == MODERN, "(b) a process that did initialize still answers server/discover")
s.close()

print()
print("FAILED" if check.failed else "ALL OK", f"({check.failed} failures)")
sys.exit(1 if check.failed else 0)
