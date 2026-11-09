# Building desktop-control.exe from source

The released `desktop-control.exe` (on the Releases page) is this Rust project compiled for Windows.
You only need to build it if you want to change the server.

## MSVC route (used for the shipped build)

With the Visual Studio Build Tools (the C++ workload) and the Rust MSVC toolchain installed
(`rustup default stable-x86_64-pc-windows-msvc`), build with the project's `.cargo\config.toml` target overridden:

    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    cargo build --release --locked --target x86_64-pc-windows-msvc
    # -> target\x86_64-pc-windows-msvc\release\desktop-control.exe

If your toolchain is not in the default `%USERPROFILE%\.rustup` and `%USERPROFILE%\.cargo`, point `RUSTUP_HOME`
and `CARGO_HOME` at it first, for example `$env:RUSTUP_HOME = 'D:\dev\rust\rustup'` and `$env:CARGO_HOME = 'D:\dev\rust\cargo'`.

Crates beyond `windows`, `serde`, `serde_json`, `base64` and `image`: `rhai` 1.x (the `run_script` language; pure Rust) and `regex` 1.x (`find_text` with `match: regex`). About 3 minutes cold, under 60 s incremental (the exe is about 7 MB). To install a build, stop the running `desktop-control.exe` process (your AI client starts it), copy the new file over the one your client
launches, and delete the `target` folder if you no longer need it.
Tests (all in `tests\`, run with the exe as argument): `mcp_stdio_test.py`, `e2e_stdio.py`, `completion_test.py`, `background_test.py`, `ocr_script_test.py`, `cursor_test.py`, `match_test.py` (window names), `home_test.py` (home window in Windows Terminal, conhost, a GUI app, fallback, homeApp; opens its own windows), `pointer_test.py` (hidden pointer), `keys_script_test.py` (shortcuts in scripts). `host_guard_test.py` (the AI app's own window gets no input, in a Windows Terminal and a console window the test opens), `security_test.py` (the v0.3.2 and v0.3.3 security fixes, see `docs\SECURITY-REVIEW-0.3.1.md` and `docs\SECURITY-REVIEW-0.3.2.md`: element ids, reading policy, Store apps, `open_app` limits, typing and the clipboard, the Esc stop, takeover, scripts, titles and masking, hooks; it builds tiny dummy windows, some under the exe names of forbidden apps, with `dummy_window.ps1`). The sections of `security_test.py` that need a raised Esc stop, a simulated user touch, dropped hooks or "no AI app found" run only against a debug build, whose hooks are compiled out of the release exe: `cargo build --locked --target x86_64-pc-windows-msvc --target-dir <temp dir>` and `DESKTOP_CONTROL_DEBUG_EXE=<that exe>`. `SEC_ONLY=n1,n4,...` runs only the named sections (see the top of the file). Tests that can touch the clipboard (typing pastes through it) save the clipboard first and put it back; `clipguard.py save|check|restore <file>` does the same around a whole run of suites (keep that file private and delete it afterwards).
Smoke tests: `desktop-control.exe bench bench\notepad.json`, `python tests\mcp_stdio_test.py <exe>` and `python tests\ocr_script_test.py <exe>`; unit tests (`cargo test --release --target x86_64-pc-windows-msvc`) cover the OCR matcher, the forbidden-app policy, close-chord matching, the `open_app` refusals, the stop's quiet-period rule and the bounded stdin reader. `cargo install cargo-audit --locked` and `cargo audit --db <dir>` check the dependencies against the RustSec advisories.

## gnullvm route (no Visual Studio required)

Toolchain:

    winget install Rustlang.Rustup            # or download rustup-init.exe
    rustup-init.exe --default-host x86_64-pc-windows-gnu --default-toolchain stable --profile minimal -y
    rustup target add x86_64-pc-windows-gnullvm
    winget install MartinStorsjo.LLVM-MinGW.UCRT

Then in `%USERPROFILE%\.cargo\config.toml` point the gnullvm target at LLVM-MinGW
(adjust the path to where winget put it):

    [target.x86_64-pc-windows-gnullvm]
    linker = "C:/Users/<you>/AppData/Local/Microsoft/WinGet/Packages/MartinStorsjo.LLVM-MinGW.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe/llvm-mingw-20260908-ucrt-x86_64/bin/x86_64-w64-mingw32-clang.exe"
    rustflags = ["-C", "dlltool=C:/Users/<you>/AppData/Local/Microsoft/WinGet/Packages/MartinStorsjo.LLVM-MinGW.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe/llvm-mingw-20260908-ucrt-x86_64/bin/llvm-dlltool.exe", "-C", "target-feature=+crt-static"]

Build (the project's own `.cargo/config.toml` already selects the gnullvm target):

    cargo build --release
    # -> target\x86_64-pc-windows-gnullvm\release\desktop-control.exe

Why gnullvm: with the plain `x86_64-pc-windows-gnu` target the `windows` crate produces an
executable whose import table is not bound by the loader (crash at start-up); the LLVM linker
plus LLVM's dlltool produce a correct one, and `+crt-static` removes the libunwind.dll dependency.

Layout: `mcp.rs` (stdio JSON-RPC: MCP 2026-07-28 and legacy, cancellation, progress), `tools.rs` (the 22 tools), `ocr.rs` (Windows.Media.Ocr engine, per-picture cache, word-to-phrase matching, `ocr-bench`), `finder.rs` (UI Automation then OCR find, `find_text` / `click_text`), `script.rs` (`run_script`, the Rhai sandbox), `observe.rs` (text/screenshot results, unchanged-screenshot suppression), `settle.rs` (event-driven settling), `background.rs` (posted-message input), `bench.rs` (`bench` mode), `capture.rs` (Windows.Graphics.Capture),
`input.rs` (SendInput, eased pointer glide, keys, clipboard paste), `hooks.rs` (low-level hooks),
`guards.rs` (the policy: stop, takeover, forbidden and allowed programs, the last look before input, masking), `sync.rs` (a mutex that survives a caught panic), `home.rs` (finds the AI app's window: parent chain, console owner via a short-lived `--console-owner` helper process, foreground fallback, `homeApp`), `client.rs` (the connected client: indicator colour and name), `session.rs`, `apps.rs` (AppsFolder + UserAssist), `uia.rs` (UI Automation),
`overlay.rs` (Windows.UI.Composition indicator, pill, glows and ripple; it never touches the system cursors), `win.rs` (window helpers).
