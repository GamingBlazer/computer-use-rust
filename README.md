<div align="center">

# 🖥️ Computer Use (Rust)

### Give any AI agent fast, safe hands on your Windows desktop.

**One tiny Rust program. Every Windows app. Any AI client.**

[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
![Platform: Windows 10/11](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D6?logo=windows)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-B7410E?logo=rust)
![MCP 2026-07-28](https://img.shields.io/badge/MCP-2026--07--28-6E56CF)
![Single file](https://img.shields.io/badge/install-one%20.exe-success)
![Status: pre-release](https://img.shields.io/badge/status-pre--release-orange)
[![GitHub stars](https://img.shields.io/github/stars/GamingBlazer/computer-use-rust?style=social)](https://github.com/GamingBlazer/computer-use-rust/stargazers)

> 🚧 **Pre-release (v0.3.4).** Everything below works today unless marked *planned*. Until v1.0 the program file is still
> called `desktop-control.exe`; it becomes `computer-use.exe` at release.

<picture>
  <source srcset="docs/demo.webp" type="image/webp">
  <img src="docs/demo.gif" alt="Demo: an AI agent opens several sites in Edge tabs, looks up Rust on Wikipedia, closes the extra tabs and writes a short summary in WordPad with Computer Use (Rust)" width="900">
</picture>

<sub>Real run: three sites opened in tabs, a Wikipedia search, the extra tabs closed, then a summary typed in WordPad. Only idle waits (page loads, pauses) play at 3x speed.</sub>

**[What is it?](#-what-is-computer-use-rust)** · **[Why it's different](#-why-its-different)** · **[All features](#-all-features)** · **[Benchmarks](#-benchmarks)** · **[Quick start](#-quick-start)** · **[Safety](#%EF%B8%8F-safety-first)** · **[FAQ](#-faq)**

### ⭐ If Computer Use (Rust) saves you time, please [star the repo](https://github.com/GamingBlazer/computer-use-rust/stargazers). It helps others find it! ⭐

</div>

---

## 🤔 What is Computer Use (Rust)?

AI assistants like Claude, ChatGPT/Codex, Gemini and Copilot are great at *thinking*, but on their own they can't
**click a button, fill in a form or use the apps on your PC**.

**Computer Use (Rust)** gives them that ability. It's a small program that runs on your Windows PC and lets your AI assistant:

- 👀 **see** your screen and understand what's on it (buttons, fields, menus, text),
- 🖱️ **operate** any app with the mouse and keyboard, the way a person would,
- ✅ **check** its own work and tell you when it's done,

all while **you stay in control**: press **Esc** at any moment and it stops instantly.

It talks to your AI through **MCP (Model Context Protocol)**, the open standard that AI apps use to connect to tools.
Set it up once and your assistant can say *"Open Excel, fill in this table and save it as a PDF"* and actually do it.

> **In one sentence:** Computer Use (Rust) turns your AI chat into an assistant that can actually use your computer,
> quickly, safely and in any app.

### Who is it for?

| | |
|---|---|
| 🧑‍💻 **Developers** | Let your coding agent test the app it just built, click through the UI, read error dialogs. |
| 🎨 **Designers & creators** | Repetitive steps in design, 3D and media tools: exporting, renaming, batch edits. |
| 🧪 **QA & testers** | Describe a test in plain words and let the agent run it in the real app. |
| 🗂️ **Everyone with busywork** | Forms, data entry, file juggling, settings, the same 20 clicks every day. |

---

## 🚀 Why it's different

Most desktop agents are slow because they **screenshot after every click**, **wait fixed delays** and **re-think every
single step**. Computer Use (Rust) is built the other way round:

| | |
|---|---|
| ⚡ **Native speed** | One ~7 MB Rust binary that talks to Windows directly: UI Automation, Windows.Graphics.Capture, SendInput. No Python, Node, .NET or PowerShell starting up on every call. A window screenshot takes **~50 ms**. |
| 🧠 **Text-first, token-light** | After each action the AI gets a short **text summary of what changed** instead of a 1,400-token image. Screenshots come only when layout matters, and identical ones are never sent twice. |
| 🌍 **Works in any app** | The accessibility tree for standard apps, **on-screen text recognition (OCR)** for canvas apps, games, Qt/Java apps and remote desktops, and pixels as the last resort. One `find` **tries each method in turn**. |
| 🚀 **Many steps, one turn** | `run_actions` batches steps, and **`run_script`** runs loops, conditions and waits locally, so a 30-step chore costs **one** AI turn instead of thirty. |
| 🛡️ **Safe by design** | Physical **Esc** stops it. It backs off when *you* touch the mouse, never drives password managers, Windows Security or the lock screen, and supports an allow-list of apps. |
| 👀 **You always know what's happening** | A smooth on-screen indicator shows what the AI is doing. When it's done you get a ✓ banner and a chime, and you're **taken back to your editor automatically**. |
| 🔌 **Any MCP client** | Speaks the newest **MCP 2026-07-28** protocol *and* the classic ones, so any standard MCP client can use it (Codex, Gemini CLI, VS Code Copilot, Cursor, Windsurf, Claude Desktop, …). Tested daily with Claude Code. |
| 🏠 **100% local & open source** | No account, no cloud, no telemetry. Apache 2.0: read it, change it, ship it. |

---

## 📊 Benchmarks

Same Notepad task (open, screenshot, type, keys, read back, close), average of 3 runs on Windows 11:

| | Classic screenshot-per-action | **Computer Use (Rust)** |
|---|---:|---:|
| Wall time per run | 4.2 s | **2.9 s** (−29%) |
| Screenshots sent to the AI | 7 | **1** |
| Image tokens (est.) | ~8,800 | **~1,150** (−87%) |
| Window screenshot | 165 ms | **53 ms** |

> The biggest win isn't in this table. Fewer images and fewer turns mean **the AI itself answers faster**, and that's
> where most of an agent's time goes. Reproduce it: `desktop-control.exe bench bench\notepad.json`.

Same flow as **one `run_script`** instead of 11 single calls (0.3.0 measurement, `bench\RESULTS.md`): **2 model turns instead of 11**, about 1.9 s of server time (-18%), and 371 instead of 3,737 characters of text returned to the AI (-90%).

---

## 🧰 All features

### 👀 See the screen
- **Window screenshots** that work even when the window is **covered, on another monitor or GPU-rendered** (Windows.Graphics.Capture).
- **Monitor screenshots**: one monitor, or **all monitors at once**.
- **Model-friendly images**: JPEG, `normal` (1366 px) or `small` (1024 px), so you save tokens on the images you do send.
- **Duplicate suppression**: an unchanged screen is reported in one line instead of re-sending the image (`force=true` to override).
- **Control tree** (UI Automation): every button, field, menu and list with ids, values and states such as `[focused]`, `[checked]`, `[expanded]`.
- **Smart list budget**: long lists and tables never push the important buttons out of view (`max_items`).
- **On-screen text recognition (OCR)** with the OCR engine built into Windows: fully local, no download, works in apps with no accessibility at all.
- **Read text back** (`read_text`): the focused field's value, the selection and the document text, so typing is verified without a screenshot.
- **Change summaries** after every action: active window, focused control, and which controls appeared, disappeared or changed.

### 🖱️ Operate any app
- **Click, double-click, right-click, middle-click, drag and scroll**, with smooth, human-like cursor motion.
- **Click by name** (`click_element`), **by visible text** (`click_text`) or by coordinates.
- **Find anything** (`find`): UI Automation first, then OCR, then it asks for a screenshot. It always tells you which method found the target.
- **Type anything**: Unicode, emoji, Tamil and other scripts, multi-line text. It's pasted reliably, **your clipboard is restored afterwards** and the text is kept out of clipboard history.
- **Set field values directly** (`set_value`) and get back what the field actually contains.
- **Control actions without pixels** (`element_action`): expand, collapse, toggle, select, invoke, focus, scroll into view.
- **Keyboard shortcuts** as chords (`ctrl+shift+s`) or key lists.
- **Background input (opt-in)**: click and type into classic Win32 windows **without moving your cursor or stealing focus**.

### 🪟 Apps & windows
- **List open windows** with stable ids, and **search installed apps** (Start-menu and Microsoft Store apps, most-used first).
- **Launch apps by name** (`open_app`): waits for the window and brings it to the front, no Start-menu typing.
- **Focus and move windows**, including **to another monitor**.
- **Multi-monitor aware**: work on several monitors in one task; the indicator follows the action.
- **Per-monitor DPI aware**: pixel-exact on mixed-scaling setups.

### ⚙️ Automate
- **`run_actions`**: a batch of steps in one call, with **smart waits**: `wait_for` text (with `a|b` alternatives) and `wait_gone` (perfect for "Uploading…" or "Saving…").
- **`run_script`**: a sandboxed script with **loops, conditions and finds**, run locally, with per-step progress, a time limit and instant cancellation.
- **Event-driven settling**: waits until the screen *actually* stops changing, not a fixed delay.
- **Live progress** reported to your AI client for long batches, and **real cancellation**: stop a long wait from your client and it stops promptly.

### 👁️ Stay informed
- **On-screen indicator**: a soft glowing frame and a status pill ("clicking", "typing", "thinking…") drawn on the GPU. It's click-through, never takes focus and **never appears in screenshots**.
- **A visible cursor** with a click ripple, so you can follow every move.
- **Done = obvious**: ✓ banner, chime, and **automatic return to your editor** (VS Code, a terminal, …), even if the AI forgets to say it's finished.
- **Never yanks you around**: if you've switched to another app yourself, it flashes your editor in the taskbar instead.

### 🔌 Protocol & tooling
- **MCP 2026-07-28** (stateless `server/discover`, per-request versions, cache hints) **plus** the classic handshake (2025-11-25, 2025-06-18, 2025-03-26).
- **Structured output** (`outputSchema` and `structuredContent`) for the data tools, and **tool annotations** (read-only hints).
- **Built-in benchmark** (`desktop-control.exe bench`) to measure speed and token cost on your own machine.
- **Configurable**: accent colour, indicator text and an allowed-apps list in a simple `config.json`.

### 🧾 Tool reference

| Tool | What it does |
|---|---|
| `screenshot` | Window, monitor or all monitors; optional control list in the same call |
| `list_elements` | The control tree with ids, states and values |
| `find` · `find_text` | Locate a control or visible text (UIA, then OCR), without clicking |
| `click` · `click_element` · `click_text` | Click by coordinates, by name or by visible text |
| `drag` · `scroll` | Drag (even across monitors) and scroll |
| `type_text` · `press_keys` | Type text, press keys and shortcuts |
| `set_value` · `read_text` | Write a field directly, read values and selections back |
| `element_action` | Expand, collapse, toggle, select, invoke, focus, scroll |
| `list_apps` · `open_app` | Open windows and installed apps; launch by name |
| `focus_window` · `move_window` | Switch to a window, move it to another monitor |
| `run_actions` · `run_script` | Batches with smart waits; sandboxed scripts with logic |
| `status` · `finish` | Health check; end the task (banner, chime, back to your editor) |

Common options on action tools: `observe` (`text` / `screenshot` / `both` / `none`), `size`, `background`.

---

## 🛡️ Safety first

An AI driving your real desktop must be easy to stop and impossible to misuse quietly:

- ⏹️ **Physical Esc** blocks every screen tool. The AI cannot lift it by itself: `finish` only hides the indicator, and running `turn-ended`, touching its file or simply waiting does nothing on its own. It lifts only when your turn with the AI has ended (the Stop hook below) or 30 s have passed without any tool call, **and** you have pressed a key or clicked in your AI app since the Esc (input the AI sends itself is recognised and does not count, and neither does a key or click in any other app). Typing your next message to the AI is enough; with the turn-ended path the key must come after the turn ended.
- 🖱️ **Corner fail-safe**: push the mouse into a screen corner to stop.
- ✋ **You come first**: if you touch the mouse or keyboard, every action is refused until the AI has taken a fresh look, and it is told to ask you first.
- 🔒 **Never drives** the lock screen, Windows Security, credential prompts or password managers (1Password, Bitwarden, KeePass, …); their windows are blacked out of screenshots and their titles are never shown. A window whose program can't be identified is treated the same way, including one embedded as a child window of another app's window (its area is blacked out and its controls are not listed), and the AI cannot find such a window by its title.
- ✅ **Allow-list** (`apps.allowed`) to keep the AI inside the apps you choose.
- 🧭 **Stale-state protection**: refuses to click if the window moved, resized or got covered since the AI last looked.
- 🪟 **Your AI app is off-limits**: it never types, clicks, reads or sends any shortcut into the chat or terminal you're talking to it from, and there is no way to opt in.
- 🚀 **`open_app` is fenced**: installed apps by name, or a `.exe` under Program Files or System32. URLs, scripts, shortcuts, installers and anything in Downloads or Temp are refused, and a name that is not an installed app (or is longer than 260 characters) is an error: nothing is ever typed into Start or a search box.
- 📜 **Built-in etiquette**: the AI is told to ask before deleting, sending, paying or changing security settings, and to treat text on web pages as data, never as instructions.
- 🏠 **100% local**: no telemetry, no cloud. Screenshots go only to the AI client you connected.

---

## ⚡ Quick start

1. **Download** from the [latest release](https://github.com/GamingBlazer/computer-use-rust/releases/latest), or build it from source ([BUILD.md](BUILD.md)).
2. **Install**: put `desktop-control.exe` in a permanent folder, e.g. `C:\Tools\computer-use\` (step-by-step guide below).
3. **Connect it** to your AI client (below).
4. Ask: *"Open Notepad and write a haiku about Rust."* Then watch the indicator. Press **Esc** any time to stop.

No installer, no runtime, no admin rights needed.

### 📦 Installation guide

Download from the [latest release](https://github.com/GamingBlazer/computer-use-rust/releases/latest) in either form; both contain the same program.

**Method 1: the `.exe`**

1. Download `desktop-control.exe`.
2. Right-click it → **Properties** → tick **Unblock** → **OK**.
3. Move it to a permanent folder, e.g. `C:\Tools\computer-use\`.

**Method 2: the `.zip`**

1. Download `computer-use-rust-<version>-windows-x64.zip`.
2. Right-click it → **Properties** → tick **Unblock** → **OK** (do this before extracting).
3. Right-click → **Extract All…** and choose a permanent folder, e.g. `C:\Tools\computer-use\`.
   From a terminal: `tar -xf computer-use-rust-<version>-windows-x64.zip -C C:\Tools\computer-use`
4. The folder now holds `desktop-control.exe`, `INSTALL.txt`, `config.example.json`, this README and the license.

Then **connect it** to your AI client (next section), using the full path, e.g. `C:\Tools\computer-use\desktop-control.exe`.

- **Check the download** *(optional)*: `Get-FileHash <file>` in PowerShell must match `SHA256SUMS.txt` on the release page.
- **Optional settings**: copy `config.example.json` to `config.json` next to the exe (see Configuration below).
- **Update**: disconnect the server in your AI client, replace `desktop-control.exe`, then reconnect (in Claude Code, `/mcp`).
- **Uninstall**: remove the server from your AI client and delete the folder. Nothing else is installed.

### 🔌 Connect your AI client

Tested daily with **Claude Code**. Every other MCP client uses the same standard protocol, so it should work too; reports are welcome!

<details open>
<summary><b>Claude Code</b></summary>

```powershell
claude mcp add --scope user computer-use -- "C:\Tools\computer-use\desktop-control.exe"
```

To get the ✓ banner and return-to-editor even when the AI forgets to call `finish`, and so that your turn ending can lift an Esc stop (together with your own next key press or click), add a Stop hook to
`%USERPROFILE%\.claude\settings.json`. `turn-ended` touches a file next to the program and exits at once:

```json
{ "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "\"C:\\Tools\\computer-use\\desktop-control.exe\" turn-ended" } ] } ] } }
```
</details>

<details>
<summary><b>OpenAI Codex (CLI / app)</b></summary>

`%USERPROFILE%\.codex\config.toml`

```toml
[mcp_servers.computer-use]
command = 'C:\Tools\computer-use\desktop-control.exe'
```
</details>

<details>
<summary><b>Gemini CLI</b></summary>

`%USERPROFILE%\.gemini\settings.json`

```json
{ "mcpServers": { "computer-use": { "command": "C:\\Tools\\computer-use\\desktop-control.exe" } } }
```
</details>

<details>
<summary><b>VS Code (Copilot agent mode)</b></summary>

`.vscode/mcp.json`

```json
{ "servers": { "computer-use": { "type": "stdio", "command": "C:\\Tools\\computer-use\\desktop-control.exe" } } }
```
</details>

<details>
<summary><b>Cursor · Windsurf · Claude Desktop · others</b></summary>

Add a stdio server named `computer-use` whose command is the path to `desktop-control.exe`, e.g. in Cursor's `~/.cursor/mcp.json`:

```json
{ "mcpServers": { "computer-use": { "command": "C:\\Tools\\computer-use\\desktop-control.exe" } } }
```
</details>

---

## 🏗️ How it works

```mermaid
flowchart LR
  A[Your AI client] -- MCP over stdio --> S[desktop-control.exe]
  S --> G[Safety guards]
  G --> U[UI Automation]
  G --> C[Windows.Graphics.Capture]
  G --> O[Windows OCR]
  G --> I[Mouse and keyboard input]
  S --> V[On-screen indicator]
  U --> W[Any Windows app]
  C --> W
  O --> W
  I --> W
```

Every action passes through the safety guards first. The indicator is drawn with Windows.UI.Composition on the GPU and is excluded from capture.

---

## ⚙️ Configuration (optional)

Put a `config.json` next to the executable:

```json
{
  "accentColor": "#3B82F6",
  "strings": { "usingComputer": "AI is using your computer", "escToCancel": "Esc to cancel" },
  "apps": { "allowed": ["notepad.exe", "msedge.exe"] }
}
```

An empty `allowed` list means any app except the built-in blocked ones.

---

## ❓ FAQ

**Does it work only with Claude?** No. It's a standard MCP server: any MCP client can use it.

**Does anything leave my PC?** Only what your connected AI client receives (screenshots and text, like any other tool result). No telemetry, no accounts.

**Why Rust?** One small file, instant start-up, low memory, no garbage-collection pauses, and memory safety, which matters for software that controls your mouse and keyboard.

**Can I keep using my PC while it works?** It's best to let it work, since there's one mouse and one keyboard. For classic Win32 apps, opt-in **background input** works without touching your cursor.

**What can't it do?** The UAC secure desktop is off-limits by Windows design. DRM-protected video renders black in screenshots. Games with anti-cheat may ignore simulated input, and Computer Use (Rust) won't try to get around them.

**macOS / Linux?** Windows-only, deliberately: going deep on one OS is what makes it this fast.

---

## 🗺️ Roadmap

- [ ] Rename to `computer-use.exe`, signed releases
- [ ] Optional admin helper (UAC-approved, per task)
- [ ] One-AI-at-a-time lock when several AI apps share one PC
- [ ] Demo videos and a one-line installer
- [ ] More script-mode helpers and ready-made recipes

Have an idea? [Open an issue](https://github.com/GamingBlazer/computer-use-rust/issues). Feature requests are welcome.

## 🤝 Contributing

Pull requests are welcome! Build from source with the MSVC Rust toolchain; see [BUILD.md](BUILD.md).
Bug reports with the app name, Windows version and the tool call that failed help the most.

---

<div align="center">

## ⭐ Like it? Star it!

If Computer Use (Rust) made your AI more useful, **[give it a star](https://github.com/GamingBlazer/computer-use-rust/stargazers)**.
It takes one click, helps other people discover the project and keeps development going. Thank you! 🙏

[![Star History Chart](https://api.star-history.com/svg?repos=GamingBlazer/computer-use-rust&type=Date)](https://star-history.com/#GamingBlazer/computer-use-rust&Date)

</div>

---

## 📄 License

[Apache License 2.0](LICENSE). Free for personal and commercial use.

*Independent open-source project. Not affiliated with, endorsed by or sponsored by Anthropic, OpenAI, Google or Microsoft.
"Claude", "Codex", "Gemini", "Copilot" and "Windows" are trademarks of their respective owners.*
