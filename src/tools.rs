//! Tool registry: name, description, JSON schema and handler for every MCP tool.

use std::cell::RefCell;
use std::time::Instant;

use serde::Serialize;
use serde_json::{json, Value};

use crate::observe::{self, Mode, Obs, Shot, ViewKey};
use crate::{actions, apps, background, capture, guards, hooks, overlay, session, settle, win};

#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum Content {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, #[serde(rename = "mimeType")] mime_type: String },
}

impl Content {
    pub fn text(s: impl Into<String>) -> Content {
        Content::Text { text: s.into() }
    }
    pub fn jpeg(bytes: &[u8]) -> Content {
        use base64::Engine;
        Content::Image { data: base64::engine::general_purpose::STANDARD.encode(bytes), mime_type: "image/jpeg".into() }
    }
}

pub type Handler = fn(Value) -> Result<Vec<Content>, String>;

pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub schema: Value,
    /// JSON schema of `structuredContent`, for the read-only data tools
    pub output_schema: Option<Value>,
    pub annotations: Value,
    pub handler: Handler,
}

pub struct Registry {
    tools: Vec<Tool>,
}

/// What a tool call produced: the content, plus structured data for tools with an outputSchema.
pub struct ToolOutput {
    pub content: Vec<Content>,
    pub structured: Option<Value>,
}

thread_local! {
    static STRUCTURED: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// Called by a handler to attach structuredContent to its result.
fn set_structured(v: Value) {
    STRUCTURED.with(|s| *s.borrow_mut() = Some(v));
}

impl Registry {
    /// The tool list, in a fixed order (the list is static, so clients may cache it).
    pub fn list(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|t| {
                let mut v = json!({"name": t.name, "title": t.title, "description": t.description, "inputSchema": t.schema, "annotations": t.annotations});
                if let Some(o) = &t.output_schema {
                    v["outputSchema"] = o.clone();
                }
                v
            })
            .collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name == name)
    }

    pub fn call(&self, name: &str, args: Value) -> Result<ToolOutput, String> {
        STRUCTURED.with(|s| *s.borrow_mut() = None);
        let t = self.tools.iter().find(|t| t.name == name).ok_or_else(|| format!("unknown tool {name:?}"))?;
        // every call, finish included, is noted here first (the user's stop can lapse only with the user's own input)
        hooks::note_tool_call();
        // a panic in a handler must not take the server (and the user's session with it) down; the
        // input it was holding is released by the guards that unwind with it
        let handler = t.handler;
        let content = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(args))) {
            Ok(r) => r?,
            Err(p) => {
                let what = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown".into());
                session::recover_after_panic();
                return Err(format!("internal error in {name} ({what}); the server is still running and every key and mouse button it held was released. Take a fresh screenshot before going on, and tell the user if it happens again"));
            }
        };
        let structured = STRUCTURED.with(|s| s.borrow_mut().take()).filter(|_| t.output_schema.is_some());
        let mut content = content;
        if name != "finish" && name != "status" {
            if let Some(w) = session::take_host_warning() {
                content.push(Content::text(w));
            }
        }
        Ok(ToolOutput { content, structured })
    }
}

/// JSON schema helper: `obj(&[("x", "integer", "description", required)])`.
fn obj(props: &[(&str, &str, &str, bool)]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, ty, desc, req) in props {
        let mut p = json!({"description": desc});
        if let Some(t) = ty.strip_prefix("array:") {
            p["type"] = json!("array");
            p["items"] = json!({"type": t});
        } else {
            p["type"] = json!(ty);
        }
        properties.insert((*name).to_string(), p);
        if *req {
            required.push(json!(name));
        }
    }
    json!({"type": "object", "properties": properties, "required": required})
}

/// Schema of an action tool: its own properties plus the shared `observe`, `screenshot_after`
/// and `size` options (and `background` where the tool can post window messages).
fn act_schema(props: &[(&str, &str, &str, bool)], background: bool) -> Value {
    let mut s = obj(props);
    let p = s["properties"].as_object_mut().unwrap();
    p.insert("observe".into(), json!({
        "type": "string", "enum": ["text", "screenshot", "both", "none"],
        "description": "what comes back after the action: text (default: active window, focused control, compact before/after change list), screenshot, both, or none"
    }));
    p.insert("screenshot_after".into(), json!({
        "type": "boolean",
        "description": "legacy: true = observe both, false = brief text (active window and focused control, no change list); an explicit observe wins"
    }));
    p.insert("size".into(), json!({
        "type": "string", "enum": ["normal", "small"],
        "description": "image width for screenshots in the result: normal = 1366 px, small = 1024 px (default: the size of the latest screenshot)"
    }));
    if background {
        p.insert("background".into(), json!({
            "type": "boolean",
            "description": "post window messages to the window of the latest screenshot(window=...) instead of using the real mouse/keyboard: no cursor move, no focus change (best-effort; default false)"
        }));
    }
    s
}

fn ann(read_only: bool, destructive: bool, idempotent: bool, open_world: bool) -> Value {
    json!({"readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": idempotent, "openWorldHint": open_world})
}

// ---------- argument helpers ----------

pub fn s<'a>(a: &'a Value, key: &str) -> Option<&'a str> {
    a.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}
pub fn s_or(a: &Value, key: &str, default: &str) -> String {
    s(a, key).unwrap_or(default).to_string()
}
pub fn f(a: &Value, key: &str) -> Result<f64, String> {
    a.get(key).and_then(Value::as_f64).ok_or_else(|| format!("{key} (number) is required"))
}
pub fn f_or(a: &Value, key: &str, default: f64) -> f64 {
    a.get(key).and_then(Value::as_f64).unwrap_or(default)
}
pub fn i_or(a: &Value, key: &str, default: i64) -> i64 {
    a.get(key).and_then(Value::as_i64).unwrap_or(default)
}
pub fn b_or(a: &Value, key: &str, default: bool) -> bool {
    a.get(key).and_then(Value::as_bool).unwrap_or(default)
}
pub fn u(a: &Value, key: &str) -> Option<u64> {
    a.get(key).and_then(Value::as_u64)
}
pub fn req_s(a: &Value, key: &str) -> Result<String, String> {
    s(a, key).map(str::to_string).ok_or_else(|| format!("{key} is required"))
}
pub fn strings(a: &Value, key: &str) -> Result<Vec<String>, String> {
    match a.get(key) {
        Some(Value::Array(v)) => Ok(v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()),
        Some(Value::String(x)) => Ok(vec![x.clone()]),
        _ => Err(format!("{key} (list of strings) is required")),
    }
}

// ---------- screenshots ----------

pub fn monitor_note(detailed: bool) -> String {
    let v = *win::VIEW.lock().unwrap();
    if let Some(h) = v.hwnd {
        let t = win::shown_title(win::hwnd_from(h));
        return format!(
            "Screenshot shows only the window {:?} ({}x{} px); coordinates refer to it. screenshot() shows the whole monitor again.\n",
            t.chars().take(60).collect::<String>(),
            v.rect.width,
            v.rect.height
        );
    }
    let mons = win::monitors();
    if mons.len() == 1 {
        return String::new();
    }
    let mut note = format!("Screenshot shows monitor {} of {}.", v.index + 1, mons.len());
    if detailed {
        let mut front: std::collections::BTreeMap<usize, String> = Default::default();
        for h in win::windows_z() {
            if front.len() == mons.len() {
                break;
            }
            if win::is_work_window(h) {
                let r = win::window_rect(h);
                front.entry(win::monitor_at(r.left + r.width / 2, r.top + r.height / 2, &mons)).or_insert_with(|| win::shown_title(h).chars().take(60).collect());
            }
        }
        let per = (0..mons.len())
            .map(|i| format!("{}{}: {:?}", i + 1, if i == 0 { " (primary)" } else { "" }, front.get(&i).cloned().unwrap_or("(desktop)".into())))
            .collect::<Vec<_>>()
            .join("; ");
        note += &format!(" Front window per monitor: {per}");
    }
    note + "\n"
}

fn capture_window_view(hwnd: windows::Win32::Foundation::HWND) -> Result<Shot, String> {
    // reading is held to the same policy as acting: not the AI app's own window, not a security app,
    // the lock screen or an app outside apps.allowed (checked before the window is even restored)
    if win::is_host(hwnd) {
        return Err("not read: the window is the AI app's own and is never read".into());
    }
    guards::guard_target(hwnd)?;
    unsafe {
        if windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd).as_bool() {
            let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_RESTORE);
            // was a fixed 300 ms: wait for the restored window to stop changing
            settle::wait_for_target(Some(capture::Target::Window(hwnd.0 as isize)), settle::WINDOW, true);
        }
    }
    if !win::is_host(hwnd) {
        session::note_target(hwnd);
    }
    let outer = win::window_rect(hwnd);
    let frame = win::frame_rect(hwnd);
    let mut img = capture::capture_window(hwnd, outer, frame)?;
    // a window of a protected or unidentifiable program embedded in this one as a child window is blacked out of the picture
    guards::mask_embedded(&mut img, hwnd, frame);
    let mons = win::monitors();
    let scale = win::scale_for(frame.width);
    let changed = win::VIEW.lock().unwrap().hwnd != Some(hwnd.0 as isize);
    {
        let mut v = win::VIEW.lock().unwrap();
        v.index = win::monitor_at(frame.left + frame.width / 2, frame.top + frame.height / 2, &mons);
        v.rect = frame;
        v.scale = scale;
        v.hwnd = Some(hwnd.0 as isize);
        v.frame = Some(frame);
        v.pinned = None;
    }
    if changed {
        crate::uia::clear_elements(); // element ids belong to the view they were listed in
    }
    overlay::sync_monitor();
    let rgb = capture::scaled(&img, scale);
    Ok(Shot { rgb, key: ViewKey::Window(hwnd.0 as isize), label: format!("window {:?}", win::shown_title(hwnd).chars().take(50).collect::<String>()), full: img })
}

/// The window a window-scoped view follows: a dialog or other window its app put in front,
/// else the viewed window while it still exists.
pub fn view_window() -> Option<windows::Win32::Foundation::HWND> {
    let seen = win::VIEW.lock().unwrap().hwnd.map(win::hwnd_from)?;
    let fg = win::foreground();
    if !fg.0.is_null() && fg != seen && win::is_work_window(fg) && win::pid_of(fg) == win::pid_of(seen) {
        return Some(fg);
    }
    win::is_window(seen).then_some(seen)
}

/// Capture one window, or one monitor (0-based index, or the work monitor), and make it the
/// click space. keep: stay with the window of the latest screenshot (after-action screenshots).
pub fn grab(monitor: Option<usize>, window: Option<&str>, keep: bool) -> Result<Shot, String> {
    if let Some(name) = window {
        let (hwnd, _t) = win::find_window(name, 1.0).ok_or_else(|| format!("No open window with {name:?} in its title."))?;
        return capture_window_view(hwnd);
    }
    if keep && monitor.is_none() {
        if let Some(h) = view_window() {
            return capture_window_view(h);
        }
    }
    let mons = win::monitors();
    let index = match monitor {
        Some(n) => {
            let mut v = win::VIEW.lock().unwrap();
            v.pinned = Some(n);
            v.pin_hwnd = win::work_window().map(|h| h.0 as isize);
            n.min(mons.len() - 1)
        }
        None => win::follow(&mons),
    };
    let m = mons[index];
    let mut img = capture::capture_monitor(windows::Win32::Graphics::Gdi::HMONITOR(m.handle as *mut _))?;
    // security and password-manager windows are blacked out of a picture of the whole monitor
    guards::mask_protected(&mut img, m.rect, false);
    let (was_window, was_index) = {
        let v = win::VIEW.lock().unwrap();
        (v.hwnd.is_some(), v.index)
    };
    win::set_monitor_view(index, &m);
    if was_window || was_index != index {
        crate::uia::clear_elements();
    }
    overlay::sync_monitor();
    Ok(Shot { rgb: capture::scaled(&img, win::scale_for(m.rect.width)), key: ViewKey::Monitor(m.handle), label: format!("monitor {}", index + 1), full: img })
}

/// Run an action tool body as task activity with its observation: the "before" snapshot is
/// taken ahead of the body, the settle and the summary or screenshot after it.
fn act(a: &Value, doing: &str, default: Mode, body: impl FnOnce() -> Result<String, String>) -> Result<Vec<Content>, String> {
    session::activity(doing, || {
        let ob = Obs::begin(a, default)?;
        let msg = body()?;
        ob.finish(msg)
    })
}

// ---------- tools ----------

fn screenshot(a: Value) -> Result<Vec<Content>, String> {
    session::activity("looking at the screen", || {
        observe::apply_size(&a, true)?;
        let window = s(&a, "window").map(str::to_string);
        if u(&a, "monitor") == Some(0) {
            let mons = win::monitors();
            let mut out = vec![Content::text(format!(
                "All monitors. Active window: {}. Coordinates without a monitor number refer to monitor {}.",
                win::active_title(),
                win::VIEW.lock().unwrap().index + 1
            ))];
            for (i, m) in mons.iter().enumerate() {
                out.push(Content::text(format!("Monitor {}{}:", i + 1, if i == 0 { " (primary)" } else { "" })));
                let mut img = capture::capture_monitor(windows::Win32::Graphics::Gdi::HMONITOR(m.handle as *mut _))?;
                guards::mask_protected(&mut img, m.rect, false);
                out.push(observe::untracked_image(&capture::scaled(&img, win::scale_for(m.rect.width))));
            }
            let work = win::follow(&mons);
            win::set_monitor_view(work, &mons[work]);
            return Ok(out);
        }
        let monitor = actions::monitor_index(u(&a, "monitor"))?;
        let shot = grab(monitor, window.as_deref(), false)?;
        // only a screenshot the agent asked for as its own tool call is a fresh look that ends a takeover (not a screenshot
        // inside run_script, nor the picture an action returns afterwards)
        guards::acknowledge();
        let mut out = vec![Content::text(format!("{}Active window: {}", monitor_note(true), win::active_title())), observe::image(shot, b_or(&a, "force", false))];
        if b_or(&a, "include_elements", false) {
            out.push(Content::text(crate::uia::list_elements(i_or(&a, "max_items", 300).max(1) as usize)?.text));
        }
        Ok(out)
    })
}

fn bg(a: &Value) -> bool {
    b_or(a, "background", false)
}

fn click(a: Value) -> Result<Vec<Content>, String> {
    act(&a, "clicking", Mode::Text, || {
        let clicks = if b_or(&a, "double", false) { 2 } else { i_or(&a, "clicks", 1).clamp(1, 3) as u32 };
        if bg(&a) {
            return background::click(f(&a, "x")?, f(&a, "y")?, &s_or(&a, "button", "left"), clicks);
        }
        actions::click(f(&a, "x")?, f(&a, "y")?, &s_or(&a, "button", "left"), clicks, actions::monitor_index(u(&a, "monitor"))?)
    })
}

fn drag(a: Value) -> Result<Vec<Content>, String> {
    act(&a, "dragging", Mode::Text, || {
        actions::drag(
            f(&a, "x")?, f(&a, "y")?, f(&a, "to_x")?, f(&a, "to_y")?,
            actions::monitor_index(u(&a, "monitor"))?, actions::monitor_index(u(&a, "to_monitor"))?, &s_or(&a, "button", "left"),
        )
    })
}

fn scroll(a: Value) -> Result<Vec<Content>, String> {
    act(&a, "scrolling", Mode::Text, || {
        if bg(&a) {
            return background::scroll(f(&a, "x")?, f(&a, "y")?, i_or(&a, "amount", 0) as i32, i_or(&a, "horizontal", 0) as i32);
        }
        actions::scroll(f(&a, "x")?, f(&a, "y")?, i_or(&a, "amount", 0) as i32, i_or(&a, "horizontal", 0) as i32, actions::monitor_index(u(&a, "monitor"))?)
    })
}

fn type_text(a: Value) -> Result<Vec<Content>, String> {
    act(&a, "typing", Mode::Text, || {
        let text = req_s(&a, "text")?;
        if bg(&a) { background::type_text(&text) } else { actions::type_text(&text) }
    })
}

fn press_keys(a: Value) -> Result<Vec<Content>, String> {
    act(&a, "pressing keys", Mode::Text, || {
        let keys = strings(&a, "keys")?;
        if bg(&a) { background::keys(&keys) } else { actions::keys(&keys) }
    })
}

fn focus_window(a: Value) -> Result<Vec<Content>, String> {
    act(&a, "switching windows", Mode::Text, || actions::focus_window(&req_s(&a, "name")?))
}

fn move_window(a: Value) -> Result<Vec<Content>, String> {
    act(&a, "moving a window", Mode::Text, || {
        let m = actions::monitor_index(u(&a, "monitor"))?.ok_or("monitor is required")?;
        actions::move_window(&req_s(&a, "name")?, m, b_or(&a, "maximize", false))
    })
}

fn open_app(a: Value) -> Result<Vec<Content>, String> {
    act(&a, "opening an app", Mode::Text, || {
        let mut msg = actions::open_app(&req_s(&a, "name")?)?;
        if let Some(w) = s(&a, "wait_for") {
            msg += "; ";
            msg += &actions::wait_for(w, 15.0, "any", actions::Source::Auto)?;
        }
        Ok(msg)
    })
}

fn list_apps(a: Value) -> Result<Vec<Content>, String> {
    session::activity("looking at the apps", || {
        let mut lines = vec!["Open windows, front to back (#id app: title; #id works wherever a window name is taken):".to_string()];
        let mut running_exes = std::collections::HashSet::new();
        let mut titles = Vec::new();
        let mut windows_data = Vec::new();
        let mut installed_data = Vec::new();
        for (h, t) in win::titled_windows() {
            let pid = win::pid_of(h);
            let host = win::is_host(h);
            if !win::is_work_window(h) && !host {
                continue;
            }
            let exe = win::app_exe(h); // a Store app is listed under its own program, not ApplicationFrameHost
            running_exes.insert(exe.clone());
            let note = if host { "   <- your AI app's own window (host: never read)" } else { "" };
            // the title of a security or password-manager window is not shown (it can name an account or a vault)
            let hidden = guards::hides_title(&exe);
            // a protected or unidentifiable window must not mark an installed app as "running" through its title
            if !hidden {
                titles.push(win::normalize_title(&t));
            }
            let title80: String = if hidden { win::HIDDEN_TITLE.to_string() } else { t.chars().take(80).collect() };
            lines.push(format!("  #{} {}: {:?}{note}", h.0 as isize, if exe.is_empty() { "?" } else { &exe }, title80));
            windows_data.push(json!({"id": h.0 as isize, "app": exe, "title": title80, "host": host}));
        }
        match s(&a, "query") {
            Some(q) => {
                let usage = apps::usage();
                let q = q.to_lowercase();
                let mut hits: Vec<(u32, String, bool, String)> = apps::installed_apps()
                    .into_iter()
                    .filter(|(n, _)| n.to_lowercase().contains(&q))
                    .map(|(n, p)| {
                        let (count, when) = usage.get(&p.to_lowercase()).cloned().unwrap_or((0, String::new()));
                        let stem = p.rsplit(['\\', '/']).next().unwrap_or(&p).trim_end_matches(".exe").to_lowercase();
                        let running = running_exes.contains(&format!("{stem}.exe")) || titles.iter().any(|t| t.contains(&win::normalize_title(&n)));
                        (count, n, running, when)
                    })
                    .collect();
                hits.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.to_lowercase().cmp(&y.1.to_lowercase())));
                lines.push(format!("Installed apps matching {q:?}, most used first (open_app takes the name):"));
                for (count, name, running, when) in hits.iter().take(40) {
                    let extra: Vec<String> = [
                        running.then(|| "running".to_string()),
                        (*count > 0).then(|| format!("used {count}x")),
                        (!when.is_empty()).then(|| format!("last {when}")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    lines.push(if extra.is_empty() { format!("  {name}") } else { format!("  {name} ({})", extra.join(", ")) });
                    installed_data.push(json!({"name": name, "running": running, "usedCount": count, "lastUsed": when}));
                }
                if hits.is_empty() {
                    lines.push("  (none)".into());
                }
            }
            None => lines.push("Pass query='part of a name' to search the installed apps.".into()),
        }
        set_structured(json!({"windows": windows_data, "installed": installed_data}));
        Ok(vec![Content::text(lines.join("\n"))])
    })
}

fn list_elements(a: Value) -> Result<Vec<Content>, String> {
    session::activity("reading the window", || {
        let l = crate::uia::list_elements(i_or(&a, "max_items", 300).max(1) as usize)?;
        guards::acknowledge(); // a listing the agent asked for itself is a fresh look (one inside run_script is not)
        set_structured(l.data);
        Ok(vec![Content::text(l.text)])
    })
}

fn read_text(a: Value) -> Result<Vec<Content>, String> {
    session::activity("reading the window", || {
        let (text, data) = crate::uia::read_text(s(&a, "target"), i_or(&a, "max_chars", 4000).max(1) as usize)?;
        set_structured(data);
        Ok(vec![Content::text(text)])
    })
}

pub fn describe(kind: &str) -> &'static str {
    match kind {
        "click" | "click_element" | "click_text" => "clicking",
        "find" | "find_text" => "reading the screen",
        "type" | "set_value" => "typing",
        "keys" => "pressing keys",
        "read_text" => "reading the window",
        "element_action" => "working with a control",
        "scroll" => "scrolling",
        "wait" | "wait_for" | "wait_gone" => "waiting",
        "open_app" => "opening an app",
        "focus_window" => "switching windows",
        "drag" => "dragging",
        "move_window" => "moving a window",
        _ => "working",
    }
}

/// find_text arguments shared by the tool, run_actions and run_script.
fn find_text_args(a: &Value) -> Result<(String, crate::finder::ViewHits), String> {
    let text = req_s(a, "text")?;
    let mode = crate::ocr::MatchMode::parse(s(a, "match"))?;
    observe::apply_size(a, false)?;
    let hits = crate::finder::text_in_view(&text, mode, s(a, "window"), actions::monitor_index(u(a, "monitor"))?, s(a, "lang"))?;
    Ok((text, hits))
}

/// click_text arguments shared by the tool, run_actions and run_script.
fn click_text_args(a: &Value) -> Result<String, String> {
    let text = req_s(a, "text")?;
    observe::apply_size(a, false)?;
    let button = s_or(a, "button", "left");
    crate::finder::click_text(&crate::finder::ClickTextOpts {
        text: &text,
        nth: u(a, "nth").map(|n| n as usize),
        button: &button,
        double: b_or(a, "double", false),
        offset_x: f_or(a, "offset_x", 0.0),
        offset_y: f_or(a, "offset_y", 0.0),
        mode: crate::ocr::MatchMode::parse(s(a, "match"))?,
        window: s(a, "window"),
        monitor: actions::monitor_index(u(a, "monitor"))?,
        lang: s(a, "lang"),
    })
}

fn find_text(a: Value) -> Result<Vec<Content>, String> {
    session::activity("reading the screen", || {
        let (text, hits) = find_text_args(&a)?;
        Ok(vec![Content::text(crate::finder::find_text_report(&text, &hits, i_or(&a, "max", 20).clamp(1, 200) as usize))])
    })
}

fn click_text(a: Value) -> Result<Vec<Content>, String> {
    act(&a, "clicking", Mode::Text, || click_text_args(&a))
}

fn find(a: Value) -> Result<Vec<Content>, String> {
    session::activity("reading the window", || {
        let l = crate::finder::find(&req_s(&a, "target")?)?;
        set_structured(l.to_json());
        Ok(vec![Content::text(l.describe())])
    })
}

fn run_script(a: Value) -> Result<Vec<Content>, String> {
    session::activity("running a script", || {
        let code = req_s(&a, "script")?;
        let ob = Obs::begin(&a, Mode::Text)?;
        let out = crate::script::run(&code, f_or(&a, "timeout_s", 60.0));
        if let Some(why) = out.stopped {
            if why == guards::CANCELLED {
                return Err(why);
            }
            return Err(format!("{why}

{}", out.report));
        }
        let mut content = ob.finish(out.report)?;
        // images from screenshot() calls go right after the report text
        for (i, img) in out.images.into_iter().enumerate() {
            content.insert(1 + i, img);
        }
        Ok(content)
    })
}

pub fn run_step(step: &Value) -> Result<String, String> {
    let kind = s(step, "action").unwrap_or("");
    let target = |st: &Value| st.get("id").and_then(Value::as_u64).map(|i| i.to_string()).or_else(|| s(st, "name").map(str::to_string));
    match kind {
        "click" => {
            let clicks = if b_or(step, "double", false) { 2 } else { i_or(step, "clicks", 1).clamp(1, 3) as u32 };
            if bg(step) {
                return background::click(f(step, "x")?, f(step, "y")?, &s_or(step, "button", "left"), clicks);
            }
            actions::click(f(step, "x")?, f(step, "y")?, &s_or(step, "button", "left"), clicks, actions::monitor_index(u(step, "monitor"))?)
        }
        "click_element" => crate::finder::click_element(&target(step).ok_or("click_element needs id or name")?, &s_or(step, "button", "left"), b_or(step, "double", false)),
        "click_text" => click_text_args(step),
        "find" => crate::finder::find(&target(step).ok_or("find needs id or name")?).map(|l| l.describe()),
        "find_text" => find_text_args(step).map(|(t, h)| crate::finder::find_text_report(&t, &h, i_or(step, "max", 20).clamp(1, 200) as usize)),
        "set_value" => crate::uia::set_value(&target(step).ok_or("set_value needs id or name")?, &req_s(step, "value")?),
        "read_text" => crate::uia::read_text(target(step).as_deref(), 4000).map(|t| t.0),
        "element_action" => crate::uia::element_action(&target(step).ok_or("element_action needs id or name")?, &req_s(step, "do")?),
        "type" => {
            if bg(step) { background::type_text(&req_s(step, "text")?) } else { actions::type_text(&req_s(step, "text")?) }
        }
        "keys" => {
            if bg(step) { background::keys(&strings(step, "keys")?) } else { actions::keys(&strings(step, "keys")?) }
        }
        "scroll" => {
            if bg(step) {
                return background::scroll(f(step, "x")?, f(step, "y")?, i_or(step, "amount", 0) as i32, i_or(step, "horizontal", 0) as i32);
            }
            actions::scroll(f(step, "x")?, f(step, "y")?, i_or(step, "amount", 0) as i32, i_or(step, "horizontal", 0) as i32, actions::monitor_index(u(step, "monitor"))?)
        }
        "drag" => actions::drag(f(step, "x")?, f(step, "y")?, f(step, "to_x")?, f(step, "to_y")?, actions::monitor_index(u(step, "monitor"))?, actions::monitor_index(u(step, "to_monitor"))?, &s_or(step, "button", "left")),
        "move_window" => actions::move_window(&req_s(step, "name")?, actions::monitor_index(u(step, "monitor"))?.ok_or("monitor is required")?, b_or(step, "maximize", false)),
        "wait" => actions::wait(f_or(step, "seconds", 1.0)),
        "wait_for" => {
            let source = actions::Source::parse(s(step, "source"))?;
            let scope = s_or(step, "scope", if source == actions::Source::Ocr { "element" } else { "window" });
            actions::wait_for(&req_s(step, "name")?, f_or(step, "timeout", 10.0), &scope, source)
        }
        "wait_gone" => actions::wait_gone(&req_s(step, "name")?, f_or(step, "timeout", 30.0), &s_or(step, "scope", "element"), actions::Source::parse(s(step, "source"))?),
        "open_app" => actions::open_app(&req_s(step, "name")?),
        "focus_window" => actions::focus_window(&req_s(step, "name")?),
        "screenshot" => Ok("a screenshot comes with the result at the end".into()),
        other => Err(format!("unknown action {other:?}; valid: click, click_element, click_text, find, find_text, set_value, read_text, element_action, type, keys, scroll, drag, move_window, wait, wait_for, wait_gone, open_app, focus_window")),
    }
}

fn run_actions(a: Value) -> Result<Vec<Content>, String> {
    session::activity("working", || {
        let steps = a.get("actions").and_then(Value::as_array).cloned().ok_or("actions (list) is required")?;
        // a screenshot step asks for the picture, unless observe says otherwise
        let wants_shot = steps.iter().any(|st| s(st, "action") == Some("screenshot"));
        let ob = Obs::begin(&a, if wants_shot { Mode::Both } else { Mode::Text })?;
        let started = Instant::now();
        let total = steps.len();
        let mut log = Vec::new();
        for (n, step) in steps.iter().enumerate() {
            let n = n + 1;
            if crate::mcp::cancelled() {
                log.push(format!("{n}. CANCELLED by the client; remaining steps skipped"));
                break;
            }
            let kind = s(step, "action").unwrap_or("");
            session::set_status(describe(kind));
            let t = Instant::now();
            match run_step(step) {
                Ok(msg) => {
                    log.push(format!("{n}. {msg} ({:.1}s)", t.elapsed().as_secs_f64()));
                    guards::note_fg(false);
                    if !matches!(kind, "wait" | "wait_for" | "wait_gone" | "read_text" | "screenshot") {
                        guards::reset_baseline();
                    }
                    crate::mcp::progress(n as f64, total as f64, &format!("step {n}/{total}: {kind}"));
                }
                Err(e) if e == guards::CANCELLED => {
                    log.push(format!("{n}. CANCELLED by the client; remaining steps skipped"));
                    break;
                }
                Err(e) if e.starts_with("Computer Use was stopped") || e.contains("screen corner to stop") => {
                    log.push(format!("{n}. STOPPED: user triggered the fail-safe (Esc or mouse in a corner)"));
                    overlay::end(overlay::STOPPED_TEXT, overlay::STOP_COLOR);
                    break;
                }
                Err(e) => {
                    log.push(format!("{n}. FAILED ({kind}) after {:.1}s: {e}; remaining steps skipped", t.elapsed().as_secs_f64()));
                    break;
                }
            }
        }
        log.push(format!("total {:.1}s", started.elapsed().as_secs_f64()));
        ob.finish(log.join("\n"))
    })
}

fn status(_a: Value) -> Result<Vec<Content>, String> {
    let mons = win::monitors();
    let v = *win::VIEW.lock().unwrap();
    // the task lock is not held while windows are queried below: asking an overlay window of this process for its
    // title sends it a message, and the overlay thread may be waiting for that very lock
    let task_active = session::TASK.lock().unwrap().active;
    let work = win::work_window().map(|h| win::shown_title(h).chars().take(60).collect::<String>());
    let view = match v.hwnd {
        Some(h) => format!("window {:?}", win::shown_title(win::hwnd_from(h)).chars().take(40).collect::<String>()),
        None => format!("monitor {}", v.index + 1),
    };
    let hooked = hooks::HOOKED.load(std::sync::atomic::Ordering::Relaxed);
    let stopped = hooks::STOP.load(std::sync::atomic::Ordering::Relaxed);
    let allowed = guards::ALLOWED.lock().unwrap().clone();
    let lines = vec![
        format!("desktop-control {} (Rust); protocol {} (also {})", env!("CARGO_PKG_VERSION"), crate::mcp::PROTOCOL, crate::mcp::SUPPORTED[1..].join(", ")),
        format!("monitors: {}",
            mons.iter().enumerate().map(|(i, m)| format!("{}: {}x{} at ({}, {})", i + 1, m.rect.width, m.rect.height, m.rect.left, m.rect.top)).collect::<Vec<String>>().join("; ")),
        match &work {
            Some(w) => format!("work window: {w:?}"),
            None => "work window: none".into(),
        },
        format!("view: {view}"),
        crate::home::describe(),
        match crate::client::raw() {
            Some((n, v)) => format!("client: {n} {v} (display {}, accent {})", crate::client::display().unwrap_or("AI"), crate::client::accent()),
            None => format!("client: not identified (accent {})", crate::client::accent()),
        },
        format!(
            "indicator: {}, task {}, hooks {}",
            if overlay::ready() { "ready" } else { "not rendered" },
            if task_active { "active" } else { "idle" },
            if hooked {
                let n = hooks::REINSTALLS.load(std::sync::atomic::Ordering::Relaxed);
                if n == 0 { "live".to_string() } else { format!("live (Windows dropped them and they were installed again {n}x)") }
            } else { "MISSING (Esc stop and user-input guard degraded)".to_string() }
        ),
        format!("stopped by the user (Esc or a screen corner): {}", if stopped { "yes (finish does not lift it)" } else { "no" }),
        format!("config: allowed apps {allowed:?}"),
        format!("input clock: user_at {} us, baseline {} us, now {} us", hooks::user_at_us(), guards::BASELINE_US.load(std::sync::atomic::Ordering::Relaxed), hooks::now_us()),
    ];
    set_structured(json!({
        "server": "desktop-control",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": crate::mcp::PROTOCOL,
        "supportedVersions": crate::mcp::SUPPORTED,
        "monitors": mons.iter().enumerate().map(|(i, m)| json!({"index": i + 1, "width": m.rect.width, "height": m.rect.height, "x": m.rect.left, "y": m.rect.top})).collect::<Vec<_>>(),
        "workWindow": work.unwrap_or_default(),
        "view": view,
        "indicatorReady": overlay::ready(),
        "client": crate::client::raw().map(|(n, v)| json!({"name": n, "version": v})).unwrap_or(Value::Null),
        "clientDisplay": crate::client::display().unwrap_or("AI"),
        "accent": crate::client::accent(),
        "taskActive": task_active,
        "hooksLive": hooked,
        "stoppedByUser": stopped,
        "allowedApps": allowed,
    }));
    Ok(vec![Content::text(lines.join("\n"))])
}

fn finish(a: Value) -> Result<Vec<Content>, String> {
    let summary = s_or(&a, "summary", "Done");
    // finish hides the indicator and takes the user home, but it never lifts the user's stop
    let stopped = hooks::STOP.load(std::sync::atomic::Ordering::Relaxed);
    let ended = session::end_task(session::Reason::Finish);
    let mut msg = session::complete(ended, Some(&summary));
    if stopped && !hooks::try_clear_stop() {
        msg += " The user stopped you with Esc and that still holds: finish does not lift it. Do not use the screen again until the user tells you to continue; tell them why you stopped and wait for the user.";
    }
    Ok(vec![Content::text(msg)])
}

pub fn registry() -> Registry {
    let mon = ("monitor", "integer", "1 = primary, 2 = next; coordinates then refer to that monitor's screenshot", false);
    Registry {
        tools: vec![
            Tool {
                name: "screenshot",
                title: "Take a screenshot",
                description: "Capture the screen as a JPEG; use its pixel coordinates for other tools. window=\"part of its title\" \
                    (or \"#id\" from list_apps): just that window, sharp and small, even when other windows cover it or it is on \
                    another monitor; later clicks go to it (it is brought to the front first) and after-action screenshots keep \
                    following it and its dialogs. Prefer this when working inside one app. Without arguments: the whole monitor \
                    with the window being worked in (this also ends a window view); monitor=N (1 = primary) another monitor; \
                    monitor=0 every monitor. include_elements=true adds list_elements' listing to the same result. If the screen is \
                    exactly as in the last screenshot you get a short text instead of the image (force=true always sends the image). \
                    size=\"small\" returns a 1024 px wide image instead of 1366 px.",
                schema: obj(&[("monitor", "integer", "1 = primary, 2 = next, 0 = all; omit for the monitor being worked on", false),
                              ("window", "string", "part of a window title, or #id", false),
                              ("include_elements", "boolean", "also return the control tree", false),
                              ("max_items", "integer", "element budget for include_elements (default 300)", false),
                              ("size", "string", "normal (default, 1366 px wide) or small (1024 px wide)", false),
                              ("force", "boolean", "send the image even if the screen is unchanged since the last screenshot", false)]),
                output_schema: None,
                annotations: ann(true, false, true, true),
                handler: screenshot,
            },
            Tool {
                name: "click",
                title: "Click at a position",
                description: "Click at (x, y) in screenshot coordinates; the pointer glides there first. button: left, right or middle \
                    (l/r/m); double=true or clicks=2/3 for double and triple clicks. monitor=N makes the coordinates refer to that \
                    monitor's screenshot instead of the latest one. background=true posts the click to the window of the latest \
                    screenshot(window=...) without moving the real pointer or changing focus (best-effort).",
                schema: act_schema(&[("x", "number", "x in screenshot pixels", true), ("y", "number", "y in screenshot pixels", true),
                              ("button", "string", "left (default), right, middle", false), ("double", "boolean", "double-click", false),
                              ("clicks", "integer", "1, 2 or 3", false), mon], true),
                output_schema: None,
                annotations: ann(false, true, false, true),
                handler: click,
            },
            Tool {
                name: "drag",
                title: "Drag between two positions",
                description: "Press at (x, y), move to (to_x, to_y) and release: drag files, sliders, selections, or a window's title \
                    bar. monitor / to_monitor pick the monitor each point is on (default: the latest screenshot's), so a drag can \
                    cross from one monitor to another.",
                schema: act_schema(&[("x", "number", "", true), ("y", "number", "", true), ("to_x", "number", "", true), ("to_y", "number", "", true),
                              mon, ("to_monitor", "integer", "monitor of the end point", false), ("button", "string", "left (default), right, middle", false)], false),
                output_schema: None,
                annotations: ann(false, true, false, true),
                handler: drag,
            },
            Tool {
                name: "scroll",
                title: "Scroll at a position",
                description: "Scroll with the pointer at (x, y). Positive amount scrolls up, negative down (about 3 lines per unit); \
                    horizontal: positive right, negative left. background=true posts wheel messages to the window of the latest \
                    screenshot(window=...) instead (best-effort).",
                schema: act_schema(&[("x", "number", "", true), ("y", "number", "", true), ("amount", "integer", "vertical units (+ up, - down)", false),
                              ("horizontal", "integer", "horizontal units (+ right, - left)", false), mon], true),
                output_schema: None,
                annotations: ann(false, false, false, true),
                handler: scroll,
            },
            Tool {
                name: "type_text",
                title: "Type text",
                description: "Type text into the focused field instantly; any language works. Newlines press Enter. background=true posts \
                    the text to the focused control of the window of the latest screenshot(window=...) (EM_REPLACESEL for classic edit \
                    controls, WM_CHAR otherwise; best-effort).",
                schema: act_schema(&[("text", "string", "", true)], true),
                output_schema: None,
                annotations: ann(false, true, false, true),
                handler: type_text,
            },
            Tool {
                name: "press_keys",
                title: "Press keys",
                description: "Press one key or a shortcut: [\"enter\"], [\"ctrl\", \"s\"], [\"alt\", \"tab\"], or a chord string like \
                    [\"ctrl+shift+s\"]. X keysym names (Control_L, Return, KP_0, period) work as well. background=true posts the keys \
                    to the window of the latest screenshot(window=...) (best-effort; Ctrl/Alt shortcuts rarely work that way).",
                schema: act_schema(&[("keys", "array:string", "keys pressed together", true)], true),
                output_schema: None,
                annotations: ann(false, true, false, true),
                handler: press_keys,
            },
            Tool {
                name: "focus_window",
                title: "Bring a window to the front",
                description: "Bring an already-open window to the front by part of its title (or #id from list_apps). Restores it if \
                    minimized. Prefer this over clicking taskbar icons.",
                schema: act_schema(&[("name", "string", "part of the title, or #id", true)], false),
                output_schema: None,
                annotations: ann(false, false, true, true),
                handler: focus_window,
            },
            Tool {
                name: "move_window",
                title: "Move a window to a monitor",
                description: "Move an open window (by part of its title or #id) to monitor N (1 = primary), centred there, or maximized on it.",
                schema: act_schema(&[("name", "string", "", true), ("monitor", "integer", "1 = primary", true), ("maximize", "boolean", "", false)], false),
                output_schema: None,
                annotations: ann(false, false, true, true),
                handler: move_window,
            },
            Tool {
                name: "open_app",
                title: "Open an app",
                description: "Open an app by name from Windows' app list (e.g. \"Notepad\", \"Microsoft Edge\"; list_apps(query=...) searches \
                    it) or by the full path of a .exe under Program Files or Windows\\System32, wait for its window (up to 8 s) and \
                    bring it to the front. URLs, files, scripts, shortcuts, installers and anything from Downloads or Temp are refused: \
                    ask the user. A name that is not an installed app is an error that lists the closest installed names: nothing is typed \
                    into Start or any search box, so ask the user. Optionally wait until a window or element whose \
                    name contains wait_for appears (up to 15 s).",
                schema: act_schema(&[("name", "string", "app name or full .exe path", true), ("wait_for", "string", "window or element name to wait for", false)], false),
                output_schema: None,
                annotations: ann(false, false, false, true),
                handler: open_app,
            },
            Tool {
                name: "list_apps",
                title: "List open windows and installed apps",
                description: "The open windows (front to back, with #id and app) and, with query, the installed apps whose name contains \
                    it (Windows' own app list: Store apps and installed programs), most used first with running state and last use.",
                schema: obj(&[("query", "string", "part of an app name", false)]),
                output_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "windows": {"type": "array", "items": {"type": "object", "properties": {
                            "id": {"type": "integer"}, "app": {"type": "string"}, "title": {"type": "string"}, "host": {"type": "boolean"}},
                            "required": ["id", "app", "title", "host"]}},
                        "installed": {"type": "array", "items": {"type": "object", "properties": {
                            "name": {"type": "string"}, "running": {"type": "boolean"}, "usedCount": {"type": "integer"}, "lastUsed": {"type": "string"}},
                            "required": ["name", "running", "usedCount", "lastUsed"]}}
                    },
                    "required": ["windows", "installed"]
                })),
                annotations: ann(true, false, true, false),
                handler: list_apps,
            },
            Tool {
                name: "list_elements",
                title: "List the window's controls",
                description: "List visible buttons, fields, menus, tabs, links and list items in the window being worked in as an \
                    indented tree with ids, states ([focused], [checked], [expanded], field values) and screenshot coordinates, headed \
                    by the focused control and any selected text. Interactive controls come first, then other visible text, then the \
                    items of long lists/trees/tables, within a budget of max_items lines (default 300); a final note says what was left \
                    out. Then use click_element, set_value, read_text or element_action with an id or name.",
                schema: obj(&[("max_items", "integer", "line budget, default 300", false)]),
                output_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "window": {"type": "string"},
                        "focused": {"type": "string"},
                        "elements": {"type": "array", "items": {"type": "object", "properties": {
                            "id": {"type": "integer"}, "type": {"type": "string"}, "name": {"type": "string"},
                            "states": {"type": "array", "items": {"type": "string"}}, "value": {"type": "string"},
                            "x": {"type": "integer"}, "y": {"type": "integer"}, "monitor": {"type": "integer"}},
                            "required": ["id", "type", "name", "states"]}},
                        "shown": {"type": "integer"}, "total": {"type": "integer"}, "truncated": {"type": "boolean"}
                    },
                    "required": ["window", "elements", "shown", "total", "truncated"]
                })),
                annotations: ann(true, false, true, true),
                handler: list_elements,
            },
            Tool {
                name: "click_element",
                title: "Click a control",
                description: "Click a control by its id from list_elements (e.g. \"12\") or by visible name or automation id (e.g. \
                    \"Save\"; partial, case-insensitive; exact matches win). Tries UI Automation first; if no control matches it \
                    looks for that text on screen (OCR) in the same window and clicks it, and the result says which one found it \
                    (\"found via UI Automation\" / \"found via on-screen text (OCR)\"). Several equal OCR matches are listed and \
                    refused, not guessed. After an OCR click the result says whether the screen changed; clicks are never retried. \
                    If nothing is found it says what was tried.",
                schema: act_schema(&[("target", "string", "id or name", true), ("button", "string", "left (default), right, middle", false), ("double", "boolean", "", false)], false),
                output_schema: None,
                annotations: ann(false, true, false, true),
                handler: |a| act(&a, "clicking", Mode::Text, || crate::finder::click_element(&req_s(&a, "target")?, &s_or(&a, "button", "left"), b_or(&a, "double", false))),
            },
            Tool {
                name: "find",
                title: "Find a control or text",
                description: "Where and how a target would be found, without clicking: UI Automation (control name or automation id \
                    in the window being worked in), then the text on screen (OCR). Says which method found it, where (in screenshot \
                    coordinates) or what was tried. Read-only.",
                schema: obj(&[("target", "string", "control name, automation id, or visible text", true)]),
                output_schema: None,
                annotations: ann(true, false, true, true),
                handler: find,
            },
            Tool {
                name: "find_text",
                title: "Find text on screen (OCR)",
                description: "Find text on screen with the OCR engine built into Windows (Windows.Media.Ocr, in-process): the universal \
                    fallback for apps UI Automation can't see (canvas apps, games, Qt/Java, remote-desktop or VM windows). Returns \
                    each match's text, box and centre in the coordinates of the latest screenshot (same as click x/y) and the line it \
                    is in, in reading order. Phrases spanning several words match. window (part of a title or #id) or monitor read \
                    that instead and make it the view, as screenshot does; default: the current view. match: contains (default, \
                    case-insensitive), exact (whole words) or regex. max: matches shown (default 20). lang: BCP-47 tag such as \
                    \"de-DE\" (default: your Windows languages, else en-US). Results are cached per picture, so asking again on an \
                    unchanged screen costs nothing. OCR can misread small text, so verify before risky clicks.",
                schema: obj(&[("text", "string", "text to find (a phrase, or a regex with match=regex)", true),
                              ("window", "string", "part of a window title, or #id", false), mon,
                              ("match", "string", "contains (default), exact, regex", false),
                              ("max", "integer", "matches to show, default 20", false),
                              ("lang", "string", "OCR language tag, e.g. en-US", false),
                              ("size", "string", "normal (default) or small: the screenshot scale the coordinates refer to", false)]),
                output_schema: None,
                annotations: ann(true, false, true, true),
                handler: find_text,
            },
            Tool {
                name: "click_text",
                title: "Click text found on screen (OCR)",
                description: "Find text by OCR in the current view (or window / monitor) and click its centre through the normal \
                    guarded click path. With several matches and no nth it lists them and refuses instead of guessing; nth=N picks \
                    the Nth in reading order. offset_x / offset_y move the click from the centre (screenshot pixels), e.g. to hit a \
                    checkbox left of its label. match: contains (default), exact (whole words) or regex. The result says whether the \
                    screen changed; a click is never retried automatically. For apps with working UI Automation prefer click_element.",
                schema: act_schema(&[("text", "string", "text to click", true), ("nth", "integer", "which match, 1 = first in reading order", false),
                              ("button", "string", "left (default), right, middle", false), ("double", "boolean", "double-click", false),
                              ("offset_x", "number", "pixels right of the centre (screenshot scale)", false), ("offset_y", "number", "pixels below the centre", false),
                              ("match", "string", "contains (default), exact, regex", false),
                              ("window", "string", "part of a window title, or #id", false), mon,
                              ("lang", "string", "OCR language tag", false)], false),
                output_schema: None,
                annotations: ann(false, true, false, true),
                handler: click_text,
            },
            Tool {
                name: "run_script",
                title: "Run a script of many steps",
                description: "Run many steps with logic in ONE call, locally, in a sandboxed Rhai script (let, if/else, for/while, fn, \
                    arrays [..], maps #{k: v}, try/catch, backtick strings with ${x}). Every action goes through the same guards as \
                    the single tools (Esc / corner fail-safe, forbidden apps, stale-state checks, cancellation); an error stops the \
                    script with its line number and the log. Actions (opts is a map): click(x,y[,opts]) click_element(target[,opts]) \
                    click_text(text[,opts]) type_text(s) press_keys(\"ctrl+s\" or [..]) scroll(x,y,amount[,horizontal]) \
                    drag(x,y,tx,ty) set_value(target,v) read_text([target])->string list_elements([max])->string focus_window(name) \
                    open_app(name) wait_for(name[,secs|opts])->bool wait_gone(name[,secs|opts])->bool (opts: timeout, scope \
                    window|element|any (default any), source auto|uia|ocr) sleep(ms) active_window()->string \
                    screenshot([#{window,monitor,size}]) (adds an image, at most 3) log(msg) \
                    find(target)->#{found,method,name,x,y,count,ambiguous,tried} \
                    find_text(text[,#{match,window,monitor,max,lang}])->#{found,count,matches:[#{text,x,y,w,h,cx,cy,line}],cx,cy,..}. \
                    click opts: button,double,clicks,monitor,background; click_text opts: nth,match,button,double,offset_x,offset_y,window. \
                    Coordinates are those of the latest screenshot, so take one (screenshot()) before clicking by x/y; click_element and \
                    click_text need none. Limits: timeout_s (default 60, max 300), 2M operations, sleep <= 10 s. Result: log lines, the \
                    script's return value, action count and time, images, then the usual observation (observe, default text). \
                    Example: open_app(\"Notepad\"); for l in [\"one\",\"two\"] { type_text(l + \"\\n\"); } let r = find_text(\"two\"); \
                    if r.found { log(`two at ${r.cx},${r.cy}`); } else { screenshot(); } r.count",
                schema: act_schema(&[("script", "string", "the Rhai script; its last expression is returned", true),
                              ("timeout_s", "number", "deadline in seconds, default 60, max 300", false)], false),
                output_schema: None,
                annotations: ann(false, true, false, true),
                handler: run_script,
            },
            Tool {
                name: "set_value",
                title: "Set a field's value",
                description: "Put text into a field directly through UI Automation, by id from list_elements or by visible name: no \
                    clicking or key presses, replaces the whole content, and reports what the field reads afterwards. Apps without \
                    that support get focus + select-all + typing instead.",
                schema: act_schema(&[("target", "string", "id or name", true), ("value", "string", "", true)], false),
                output_schema: None,
                annotations: ann(false, true, true, true),
                handler: |a| act(&a, "typing", Mode::Text, || crate::uia::set_value(&req_s(&a, "target")?, &req_s(&a, "value")?)),
            },
            Tool {
                name: "read_text",
                title: "Read a control's text",
                description: "Read the focused control, or one by id/name from list_elements: its value, selected text and document \
                    text, without a screenshot.",
                schema: obj(&[("target", "string", "id or name; omit for the focused control", false), ("max_chars", "integer", "default 4000", false)]),
                output_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "type": {"type": "string"}, "name": {"type": "string"}, "value": {"type": "string"},
                        "selected": {"type": "string"}, "text": {"type": "string"}
                    },
                    "required": ["type", "name"]
                })),
                annotations: ann(true, false, true, true),
                handler: read_text,
            },
            Tool {
                name: "element_action",
                title: "Act on a control",
                description: "Act on a control through UI Automation, by id from list_elements or by visible name: action is expand, \
                    collapse, toggle, select, invoke, focus, scroll_into_view, scroll_up, scroll_down, scroll_left or scroll_right.",
                schema: act_schema(&[("target", "string", "id or name", true), ("action", "string", "", true)], false),
                output_schema: None,
                annotations: ann(false, true, false, true),
                handler: |a| act(&a, "working with a control", Mode::Text, || crate::uia::element_action(&req_s(&a, "target")?, &req_s(&a, "action")?)),
            },
            Tool {
                name: "run_actions",
                title: "Run several steps",
                description: "Run several steps in one call, stopping at the first error. Each step is a dict with \"action\": \
                    open_app {name}; focus_window {name}; wait_for {name, timeout, scope: window|element|any, source?: auto|uia|ocr} (\"a|b\" waits for \
                    whichever appears first; auto falls back to on-screen text OCR); wait_gone {name, timeout, scope, source?} (waits until a progress message or window disappears); \
                    click_element {name | id} (UI Automation, then on-screen text OCR); click_text {text, nth?, match?}; find {name}; find_text {text}; set_value {name | id, value}; read_text {id?}; element_action {name | id, do}; \
                    click {x, y, button?, double?, clicks?, monitor?, background?}; type {text, background?}; keys {keys, background?}; \
                    scroll {x, y, amount, horizontal?, background?}; \
                    drag {x, y, to_x, to_y, to_monitor?}; move_window {name, monitor, maximize?}; wait {seconds}. Each step's time is \
                    reported. The final observation follows observe (default text; a screenshot step makes it both).",
                schema: act_schema(&[("actions", "array:object", "steps", true)], false),
                output_schema: None,
                annotations: ann(false, true, false, true),
                handler: run_actions,
            },
            Tool {
                name: "status",
                title: "Server status",
                description: "Diagnostics without touching the screen: monitors, the window being worked in, the home window (the AI \
                    app's window the user is taken back to), whether the indicator and the input hooks are live, and whether the user \
                    stopped the agent with Esc.",
                schema: obj(&[]),
                output_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "server": {"type": "string"}, "version": {"type": "string"}, "protocol": {"type": "string"},
                        "supportedVersions": {"type": "array", "items": {"type": "string"}},
                        "monitors": {"type": "array", "items": {"type": "object", "properties": {
                            "index": {"type": "integer"}, "width": {"type": "integer"}, "height": {"type": "integer"},
                            "x": {"type": "integer"}, "y": {"type": "integer"}}, "required": ["index", "width", "height", "x", "y"]}},
                        "workWindow": {"type": "string"}, "view": {"type": "string"},
                        "client": {"type": ["object", "null"]}, "clientDisplay": {"type": "string"}, "accent": {"type": "string"},
                        "indicatorReady": {"type": "boolean"}, "taskActive": {"type": "boolean"}, "hooksLive": {"type": "boolean"},
                        "stoppedByUser": {"type": "boolean"}, "allowedApps": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["server", "version", "protocol", "monitors", "hooksLive", "stoppedByUser"]
                })),
                annotations: ann(true, false, true, false),
                handler: status,
            },
            Tool {
                name: "finish",
                title: "Finish the computer task",
                description: "Call once at the very end of a computer task: when everything the user asked is done, or when you have \
                    to stop (failure, need the user's input, fail-safe). Hides the indicator, flashes the summary on screen with a \
                    chime and brings the user back to the window of their AI app (the one this session was started from). It never \
                    lifts the user's Esc stop: after Esc, call it, say why you stopped and wait. Keep summary short.",
                schema: obj(&[("summary", "string", "", false)]),
                output_schema: None,
                annotations: ann(false, false, true, false),
                handler: finish,
            },
        ],
    }
}
