"""Tiny stdio MCP client for the desktop-control tests (no SDK needed)."""
import json
import os
import queue
import subprocess
import threading
import time
import sys

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_EXE = next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe"))
META = "io.modelcontextprotocol/protocolVersion"
MODERN = "2026-07-28"
M = {META: MODERN, "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"}, "io.modelcontextprotocol/clientCapabilities": {}}


class Server:
    def __init__(self, exe=None, stderr=subprocess.DEVNULL, env=None, creationflags=0):
        self.p = subprocess.Popen([exe or DEFAULT_EXE], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, bufsize=0,
                                  env=dict(os.environ, **env) if env else None, creationflags=creationflags)
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

    def request(self, method, params=None):
        self.n += 1
        msg = {"jsonrpc": "2.0", "id": self.n, "method": method}
        if params is not None:
            msg["params"] = params
        self.send(msg)
        return self.n

    def notify(self, method, params=None):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        self.send(msg)

    def wait(self, id, timeout=60, collect=None):
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

    def start_call(self, name, args=None, progress=None):
        params = {"name": name, "arguments": args or {}, "_meta": dict(M)}
        if progress is not None:
            params["_meta"]["progressToken"] = progress
        return self.request("tools/call", params)

    def call(self, name, args=None, timeout=60):
        """-> (text, images, is_error)"""
        m = self.wait(self.start_call(name, args), timeout)
        if "error" in m:
            return json.dumps(m["error"]), 0, True
        r = m["result"]
        text = "\n".join(c["text"] for c in r["content"] if c["type"] == "text")
        imgs = sum(1 for c in r["content"] if c["type"] == "image")
        return text, imgs, bool(r.get("isError"))

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


class Checker:
    def __init__(self):
        self.fails = 0

    def __call__(self, ok, label, detail=""):
        print(("ok   " if ok else "FAIL ") + label + (f"  [{detail}]" if detail and not ok else ""))
        if not ok:
            self.fails += 1
