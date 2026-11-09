"""The indicator's colour and name follow the connected AI client (MCP clientInfo).

  1. legacy `initialize` with clientInfo, then `status`: expected accent and display name for each client name,
     and none at all (indigo, "AI")
  2. the 2026-07-28 path: clientInfo in _meta["io.modelcontextprotocol/clientInfo"] on a request
  3. config.json: clientColors > accentColor > built-in map > default (a copy of the exe in a temp folder)
  4. visual spot check (DESKTOP_CONTROL_CAPTURABLE=1): the indicator over a white window covering the monitor for claude and for an
     unknown client, saved under bench\\evidence

Usage: python -I client_style_test.py [path\\to\\desktop-control.exe]
"""
import base64
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from dcclient import Server, Checker, M, META  # noqa: E402

EXE = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.abspath(next((p for p in [os.environ.get("DESKTOP_CONTROL_EXE")] + [os.path.join(HERE, *r) for r in (("..", "..", "desktop-control.exe"), ("..", "target", "x86_64-pc-windows-msvc", "release", "desktop-control.exe"), ("..", "target", "release", "desktop-control.exe"))] if p and os.path.exists(p)), os.path.join(HERE, "..", "..", "desktop-control.exe")))
EVIDENCE = os.path.abspath(os.path.join(HERE, "..", "bench", "evidence"))
check = Checker()

CASES = [
    ("claude-code", "Claude", "#D97757"),
    ("claude-ai", "Claude", "#D97757"),
    ("codex-mcp-client", "Codex", "#10A37F"),
    ("openai-mcp", "ChatGPT", "#10A37F"),
    ("gemini-cli-mcp-client", "Gemini", "#4285F4"),
    ("Visual Studio Code", "Copilot", "#8957E5"),
    ("cursor-vscode", "Cursor", "#A1A1AA"),
    ("windsurf", "Windsurf", "#09B6A2"),
    ("mystery-agent", "AI", "#6366F1"),
]


def status_text(s, meta=None):
    params = {"name": "status", "arguments": {}}
    if meta is not None:
        params["_meta"] = meta
    m = s.wait(s.request("tools/call", params), 30)
    r = m["result"]
    return "\n".join(c["text"] for c in r["content"] if c["type"] == "text"), r.get("structuredContent")


def legacy_call(s, tool, args):
    """tools/call without _meta (the legacy session), so the client stays the one named at initialize."""
    return s.wait(s.request("tools/call", {"name": tool, "arguments": args}), 60)["result"]


def parse(text):
    m = re.search(r"client: (.*?) \(display (.*?), accent (#[0-9A-Fa-f]{6})\)", text)
    if m:
        return m.group(1), m.group(2), m.group(3).upper()
    m = re.search(r"client: not identified \(accent (#[0-9A-Fa-f]{6})\)", text)
    return (None, "AI", m.group(1).upper()) if m else (None, None, None)


def legacy_server(exe, name, env=None):
    s = Server(exe, env=env)
    params = {"protocolVersion": "2025-06-18", "capabilities": {}}
    if name is not None:
        params["clientInfo"] = {"name": name, "version": "9.9"}
    s.wait(s.request("initialize", params), 30)
    s.notify("notifications/initialized")
    return s


print("--- 1. legacy initialize + clientInfo")
for name, disp, hex_ in CASES + [(None, "AI", "#6366F1")]:
    s = legacy_server(EXE, name)
    try:
        text, data = status_text(s)
        cn, d, a = parse(text)
        check(d == disp and a == hex_.upper(), f"legacy clientInfo {name!r}: display {disp!r}, accent {hex_}", f"got {d!r} {a!r}: {text[text.find('client:'):][:120]}")
        if name:
            check(cn == f"{name} 9.9", f"legacy {name!r}: name and version reported as received", str(cn))
            check(data and data.get("client") == {"name": name, "version": "9.9"} and data.get("clientDisplay") == disp and data.get("accent", "").upper() == hex_.upper(),
                  f"legacy {name!r}: structured status has client, clientDisplay, accent", str(data and {k: data.get(k) for k in ('client', 'clientDisplay', 'accent')}))
    finally:
        s.close()

print("--- 2. 2026-07-28: clientInfo in _meta")
for name, disp, hex_ in CASES + [(None, "AI", "#6366F1")]:
    s = Server(EXE)
    try:
        meta = {META: "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}
        if name:
            meta["io.modelcontextprotocol/clientInfo"] = {"name": name, "version": "1.2"}
        text, data = status_text(s, meta)
        cn, d, a = parse(text)
        check(d == disp and a == hex_.upper(), f"2026-07-28 clientInfo {name!r}: display {disp!r}, accent {hex_}", f"got {d!r} {a!r}")
    finally:
        s.close()
# the latest client seen wins
s = Server(EXE)
try:
    for name in ("claude-code", "gemini-cli-mcp-client"):
        meta = {META: "2026-07-28", "io.modelcontextprotocol/clientInfo": {"name": name, "version": "1"}}
        text, _ = status_text(s, meta)
    check(parse(text)[1] == "Gemini", "the latest clientInfo seen wins")
finally:
    s.close()

print("--- 3. config.json precedence")
tmp = tempfile.mkdtemp(prefix="dcstyle_")
try:
    exe2 = os.path.join(tmp, "desktop-control.exe")
    shutil.copy2(EXE, exe2)

    def with_config(cfg, name):
        json.dump(cfg, open(os.path.join(tmp, "config.json"), "w"))
        s = legacy_server(exe2, name)
        try:
            return parse(status_text(s)[0])
        finally:
            s.close()

    check(with_config({"clientColors": {"CLAUDE": "#112233"}}, "claude-code")[2] == "#112233", "clientColors wins (case-insensitive substring)")
    check(with_config({"clientColors": {"claude": "#112233"}}, "codex-mcp-client")[2] == "#10A37F", "clientColors for another client leaves the built-in colour")
    check(with_config({"accentColor": "#445566"}, "mystery-agent")[2] == "#445566", "accentColor applies to an unknown client")
    check(with_config({"accentColor": "#445566"}, "claude-code")[2] == "#445566", "accentColor (global override) beats the built-in map")
    check(with_config({"accentColor": "#445566", "clientColors": {"claude": "#112233"}}, "claude-code")[2] == "#112233", "clientColors beats accentColor")
    check(with_config({}, "claude-code")[2] == "#D97757", "no config: built-in map")
finally:
    shutil.rmtree(tmp, ignore_errors=True)

print("--- 4. visual spot check (indicator over a white window covering the monitor)")
os.makedirs(EVIDENCE, exist_ok=True)
ps = r"""
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form; $f.Text='DC White Test'; $f.BackColor='White'; $f.FormBorderStyle='None'; $f.StartPosition='Manual'
$f.TopMost=$true; $f.Bounds=[Windows.Forms.Screen]::PrimaryScreen.Bounds  # covers the whole monitor: nothing private in the picture
[Windows.Forms.Application]::Run($f)
"""
form = subprocess.Popen(["powershell", "-NoProfile", "-STA", "-Command", ps], creationflags=0x08000000)
try:
    time.sleep(3.0)
    for label, name in (("claude", "claude-code"), ("unknown", "mystery-agent")):
        s = legacy_server(EXE, name, env={"DESKTOP_CONTROL_CAPTURABLE": "1"})
        try:
            legacy_call(s, "focus_window", {"name": "DC White Test", "observe": "none"})
            legacy_call(s, "screenshot", {"monitor": 1})
            time.sleep(1.8)
            r = legacy_call(s, "screenshot", {"monitor": 1, "force": True})
            imgs = [c for c in r["content"] if c["type"] == "image"]
            check(bool(imgs), f"spot check {label}: a screenshot with the indicator came back")
            if imgs:
                path = os.path.join(EVIDENCE, f"indicator_{label}.jpg")
                open(path, "wb").write(base64.b64decode(imgs[0]["data"]))
                print("     saved", path)
        finally:
            s.close()
finally:
    subprocess.run(["taskkill", "/F", "/T", "/PID", str(form.pid)], capture_output=True)

print("FAILED" if check.fails else "ALL OK", f"({check.fails} failures)")
sys.exit(1 if check.fails else 0)
