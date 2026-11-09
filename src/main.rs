//! Desktop-control MCP server for Windows, in Rust.
//!
//! Same tool names and arguments as the Python `server.py`, so existing MCP clients and the
//! user's instructions need no changes.

mod actions;
mod apps;
mod background;
mod bench;
mod capture;
mod client;
mod finder;
mod guards;
mod home;
mod hooks;
mod input;
mod mcp;
mod observe;
mod ocr;
mod overlay;
mod script;
mod session;
mod sync;
mod settle;
mod tools;
mod uia;
mod win;

/// config.json next to the exe: accentColor, clientColors, strings, apps.allowed, homeApp.
pub fn config() -> &'static serde_json::Value {
    static C: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("config.json")))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::json!({}))
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("turn-ended") {
        // For a client's Stop hook: touch the turn-ended file next to this exe and leave at once. No hooks,
        // no indicator, no cursor scheme: just the file.
        std::process::exit(session::touch_turn_ended());
    }
    if args.get(1).map(String::as_str) == Some("--cursor-manager") {
        // the watchdog of older builds: a no-op that only reloads the cursor scheme
        overlay::cursor_manager(args.get(2).and_then(|p| p.parse().ok()).unwrap_or(0));
        return;
    }
    if args.get(1).map(String::as_str) == Some("--console-owner") {
        // helper of home::resolve: runs without a console so that AttachConsole can attach
        home::console_owner_main(args.get(2).map(String::as_str).unwrap_or(""));
        return;
    }
    unsafe {
        // Physical pixels everywhere (screenshots, UI Automation rectangles, clicks), before any
        // DPI-dependent call is made.
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
        // WinRT (capture) and COM (UI Automation, shell) on this thread.
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        let _ = windows::Win32::Media::timeBeginPeriod(1); // even 8 ms glide steps
    }
    home::init();
    if let Some(list) = config().pointer("/apps/allowed").and_then(|v| v.as_array()) {
        *guards::ALLOWED.lock().unwrap() = list.iter().filter_map(|v| v.as_str()).map(|s| s.to_lowercase()).collect();
    }
    // Older builds swapped the system cursors for a blank one while a task ran; if one of them died
    // mid-task the cursors stayed invisible. Nothing here changes them any more; reload the scheme once.
    overlay::restore_cursors();
    hooks::start();
    overlay::start();
    guards::reset_baseline();
    if args.get(1).map(String::as_str) == Some("ocr-bench") {
        let Some(path) = args.get(2) else {
            eprintln!("usage: desktop-control.exe ocr-bench <image.png> [--repeat N] [--find TEXT]");
            std::process::exit(2);
        };
        let repeat = args.iter().position(|a| a == "--repeat").and_then(|i| args.get(i + 1)).and_then(|n| n.parse().ok()).unwrap_or(3usize);
        let find = args.iter().position(|a| a == "--find").and_then(|i| args.get(i + 1)).map(String::as_str);
        std::process::exit(ocr::bench_file(path, repeat, find));
    }
    let registry = tools::registry();
    if args.get(1).map(String::as_str) == Some("bench") {
        let Some(path) = args.get(2) else {
            eprintln!("usage: desktop-control.exe bench <calls.json> [--repeat N] [--show]");
            std::process::exit(2);
        };
        let repeat = args.iter().position(|a| a == "--repeat").and_then(|i| args.get(i + 1)).and_then(|n| n.parse().ok()).unwrap_or(1usize).max(1);
        let code = bench::run(path, repeat, &registry, args.iter().any(|a| a == "--show"));
        std::process::exit(code);
    }
    mcp::serve(registry);
}
