//! run_script: many steps in one tool call, run locally in a sandboxed Rhai script.
//!
//! Rhai is pure Rust and has no file, network or process access unless the host registers it;
//! only the actions below are registered, and every one goes through the same guarded code as
//! the single tools (`tools::run_step`, `finder`, `actions`): Esc and corner fail-safe,
//! forbidden apps, `apps.allowed`, stale-state refusals, MCP cancellation. On top of that the
//! engine is limited: operations, call depth, expression depth, string/array/map sizes, a
//! wall-clock deadline (`timeout_s`), at most 3 screenshots, sleep capped.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use rhai::{Array, Dynamic, Engine, EvalAltResult, Map, Position, Scope};
use serde_json::{json, Value};

use crate::ocr::MatchMode;
use crate::tools::{self, Content};
use crate::{actions, finder, guards, observe, session, win};

const MAX_OPERATIONS: u64 = 2_000_000;
const MAX_SCREENSHOTS: usize = 3;
const MAX_SLEEP_MS: i64 = 10_000;
const MAX_LOG_LINES: usize = 200;
const MAX_LOG_CHARS: usize = 20_000;
const TRACE_KEPT: usize = 8;

struct Ctx {
    started: Instant,
    deadline: Instant,
    timeout_s: f64,
    steps: usize,
    log: Vec<String>,
    log_chars: usize,
    trace: Vec<String>,
    images: Vec<Content>,
    /// why the engine was told to stop (deadline, Esc, cancellation), for the report
    stop: Option<&'static str>,
    /// a refusal that ends the run for good (the user's stop, a cancellation, the user having taken over): once an
    /// action returned one, no further action runs and the engine is terminated, however the script handled the error
    fatal: Option<String>,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

type R<T> = Result<T, Box<EvalAltResult>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_takeover_ends_a_script_like_the_stop_does() {
        assert!(is_stop(guards::TAKEOVER_MESSAGE));
        assert!(is_stop(&format!("{}; the drag was undone", guards::TAKEOVER_MESSAGE)));
        assert!(is_stop(guards::STOP_MESSAGE));
        assert!(is_stop(guards::CANCELLED));
        assert!(!is_stop("the window moved or resized since the screenshot; take a fresh screenshot before retrying"));
    }
}

fn rt(msg: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(Dynamic::from(msg.into()), Position::NONE))
}

fn with<T>(f: impl FnOnce(&mut Ctx) -> T) -> T {
    CTX.with(|c| f(c.borrow_mut().as_mut().expect("script context")))
}

/// Errors that end the run: the stop, the cancellation, and the user having taken over (it can only be ended by the
/// agent asking the user and looking again outside the script).
fn is_stop(e: &str) -> bool {
    e.starts_with("Computer Use was stopped") || e.contains("screen corner to stop") || e == guards::CANCELLED || e.contains(guards::TAKEOVER_MESSAGE)
}

/// Remember a terminal error so that a script cannot go on after catching it.
fn note_terminal(e: &str) {
    if is_stop(e) {
        with(|c| {
            if c.fatal.is_none() {
                c.fatal = Some(e.to_string());
                c.stop = Some(if e == guards::CANCELLED { "cancelled" } else { "stopped" });
            }
        });
    }
}

/// The stop / cancellation check, with the terminal marking.
fn failsafe() -> R<()> {
    guards::check_failsafe().map_err(|e| {
        note_terminal(&e);
        rt(e)
    })
}

/// Before every action: Esc, corner, cancellation, deadline; counts the step and reports progress.
fn tick(kind: &str) -> R<usize> {
    if let Some(f) = with(|c| c.fatal.clone()) {
        return Err(rt(f)); // an earlier action ended the run: nothing more is done, whatever the script caught
    }
    let (n, over) = with(|c| {
        c.steps += 1;
        (c.steps, Instant::now() >= c.deadline)
    });
    if over {
        with(|c| c.stop = Some("timeout"));
        return Err(rt(format!("script timed out after {} s", with(|c| c.timeout_s))));
    }
    failsafe()?;
    session::set_status(tools::describe(kind));
    crate::mcp::progress_count(n as f64, &format!("script step {n}: {kind}"));
    Ok(n)
}

fn finish_step(n: usize, kind: &str, r: Result<String, String>) -> R<String> {
    match r {
        Ok(msg) => {
            guards::note_fg(false);
            if !matches!(kind, "wait_for" | "wait_gone" | "read_text" | "screenshot" | "find" | "find_text" | "list_elements") {
                guards::reset_baseline();
            }
            with(|c| {
                c.trace.push(format!("{n}. {kind}: {}", msg.lines().next().unwrap_or("").chars().take(120).collect::<String>()));
                if c.trace.len() > TRACE_KEPT {
                    c.trace.remove(0);
                }
            });
            Ok(msg)
        }
        Err(e) => {
            note_terminal(&e);
            with(|c| c.trace.push(format!("{n}. {kind}: FAILED {}", e.chars().take(160).collect::<String>())));
            Err(rt(e))
        }
    }
}

// ---------- Dynamic <-> JSON ----------

fn to_json(d: &Dynamic) -> Value {
    if d.is_unit() {
        Value::Null
    } else if let Some(b) = d.clone().try_cast::<bool>() {
        json!(b)
    } else if let Some(i) = d.clone().try_cast::<i64>() {
        json!(i)
    } else if let Some(f) = d.clone().try_cast::<f64>() {
        json!(f)
    } else if let Some(s) = d.clone().try_cast::<rhai::ImmutableString>() {
        json!(s.as_str())
    } else if let Some(a) = d.clone().try_cast::<Array>() {
        Value::Array(a.iter().map(to_json).collect())
    } else if let Some(m) = d.clone().try_cast::<Map>() {
        Value::Object(m.iter().map(|(k, v)| (k.to_string(), to_json(v))).collect())
    } else {
        json!(d.to_string())
    }
}

fn from_json(v: &Value) -> Dynamic {
    match v {
        Value::Null => Dynamic::UNIT,
        Value::Bool(b) => Dynamic::from(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Dynamic::from(i),
            None => Dynamic::from(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Dynamic::from(s.clone()),
        Value::Array(a) => Dynamic::from(a.iter().map(from_json).collect::<Array>()),
        Value::Object(o) => Dynamic::from(o.iter().map(|(k, v)| (k.as_str().into(), from_json(v))).collect::<Map>()),
    }
}

fn num(d: &Dynamic, what: &str) -> R<f64> {
    if let Some(f) = d.clone().try_cast::<f64>() {
        return Ok(f);
    }
    if let Some(i) = d.clone().try_cast::<i64>() {
        return Ok(i as f64);
    }
    Err(rt(format!("{what} must be a number, not {}", d.type_name())))
}

fn text(d: &Dynamic, what: &str) -> R<String> {
    d.clone().try_cast::<rhai::ImmutableString>().map(|s| s.to_string()).ok_or_else(|| rt(format!("{what} must be a string, not {}", d.type_name())))
}

fn opts(d: &Dynamic) -> R<Value> {
    if d.is_unit() {
        return Ok(json!({}));
    }
    match d.clone().try_cast::<Map>() {
        Some(m) => Ok(to_json(&Dynamic::from(m))),
        None => Err(rt(format!("options must be a map like #{{button: \"right\"}}, not {}", d.type_name()))),
    }
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    base
}

// ---------- registered actions ----------

/// Run one `run_step` action through the guarded paths.
fn step(kind: &str, action: &str, args: Value) -> R<String> {
    let n = tick(kind)?;
    let mut s = args;
    s["action"] = json!(action);
    let r = tools::run_step(&s);
    finish_step(n, kind, r)
}

fn add_log(line: String) {
    with(|c| {
        if c.log.len() < MAX_LOG_LINES && c.log_chars < MAX_LOG_CHARS {
            c.log_chars += line.len();
            c.log.push(line);
        } else if c.log.len() == MAX_LOG_LINES || c.log_chars >= MAX_LOG_CHARS {
            if c.log.last().map(|l| l != "... (log truncated)").unwrap_or(true) {
                c.log.push("... (log truncated)".into());
            }
        }
    });
}

fn find_map(target: &str) -> R<Map> {
    let n = tick("find")?;
    let r = finder::find(target);
    let (out, res) = match &r {
        Ok(l) => (l.to_json(), Ok(l.describe())),
        Err(e) => (json!({"found": false, "method": "none", "count": 0, "ambiguous": false, "tried": e}), Ok(format!("not found: {}", e.chars().take(100).collect::<String>()))),
    };
    finish_step(n, "find", res)?;
    Ok(from_json(&out).try_cast::<Map>().unwrap_or_default())
}

fn find_text_map(txt: &str, o: Value) -> R<Map> {
    let n = tick("find_text")?;
    let mode = MatchMode::parse(tools::s(&o, "match")).map_err(rt)?;
    let max = tools::i_or(&o, "max", 20).clamp(1, 200) as usize;
    let monitor = actions::monitor_index(tools::u(&o, "monitor")).map_err(rt)?;
    if let Some(sz) = o.get("size") {
        observe::apply_size(&json!({"size": sz}), false).map_err(rt)?;
    }
    let hits = match finder::text_in_view(txt, mode, tools::s(&o, "window"), monitor, tools::s(&o, "lang")) {
        Ok(h) => h,
        Err(e) => return Err(finish_step(n, "find_text", Err(e)).unwrap_err()),
    };
    let ms: Vec<Value> = hits
        .matches
        .iter()
        .take(max)
        .map(|m| json!({"text": m.text, "line": m.line, "x": m.x.round(), "y": m.y.round(), "w": m.w.round(), "h": m.h.round(), "cx": m.cx.round(), "cy": m.cy.round(), "exact": m.exact}))
        .collect();
    let mut out = json!({"found": !hits.matches.is_empty(), "count": hits.matches.len(), "matches": ms, "lang": hits.rec.lang, "ms": hits.rec.ms as i64, "cached": hits.cached, "lines": hits.rec.lines.len()});
    if let Some(first) = hits.matches.first() {
        for (k, v) in [("text", json!(first.text)), ("x", json!(first.x.round())), ("y", json!(first.y.round())), ("w", json!(first.w.round())), ("h", json!(first.h.round())), ("cx", json!(first.cx.round())), ("cy", json!(first.cy.round()))] {
            out[k] = v;
        }
    }
    finish_step(n, "find_text", Ok(format!("{} match(es) for {txt:?}", hits.matches.len())))?;
    Ok(from_json(&out).try_cast::<Map>().unwrap_or_default())
}

fn wait_generic(kind: &str, name: &str, o: Value, gone: bool) -> R<bool> {
    let n = tick(kind)?;
    let remaining = with(|c| c.deadline.saturating_duration_since(Instant::now()).as_secs_f64());
    let timeout = tools::f_or(&o, "timeout", if gone { 30.0 } else { 10.0 }).min(remaining).max(0.0);
    let scope = tools::s_or(&o, "scope", "any");
    let source = actions::Source::parse(tools::s(&o, "source")).map_err(rt)?;
    let r = if gone { actions::wait_gone(name, timeout, &scope, source) } else { actions::wait_for(name, timeout, &scope, source) };
    match r {
        Ok(m) => {
            finish_step(n, kind, Ok(m))?;
            Ok(true)
        }
        Err(e) if is_stop(&e) => {
            note_terminal(&e);
            Err(rt(e))
        }
        Err(e) if e.contains("scope must be") => Err(rt(e)),
        Err(e) => {
            // a timeout is an answer, not a failure: the script can branch on it
            finish_step(n, kind, Ok(format!("false: {}", e.chars().take(100).collect::<String>())))?;
            Ok(false)
        }
    }
}

fn wait_opts(d: &Dynamic) -> R<Value> {
    if d.is_unit() {
        return Ok(json!({}));
    }
    if let Ok(secs) = num(d, "timeout") {
        return Ok(json!({"timeout": secs}));
    }
    opts(d)
}

fn sleep_ms(ms: i64) -> R<()> {
    let n = tick("sleep")?;
    let want = Duration::from_millis(ms.clamp(0, MAX_SLEEP_MS) as u64);
    let end = Instant::now() + want;
    while Instant::now() < end {
        failsafe()?;
        if with(|c| Instant::now() >= c.deadline) {
            with(|c| c.stop = Some("timeout"));
            return Err(rt(format!("script timed out after {} s", with(|c| c.timeout_s))));
        }
        std::thread::sleep(Duration::from_millis(20).min(end.saturating_duration_since(Instant::now())));
    }
    finish_step(n, "sleep", Ok(format!("slept {} ms", want.as_millis()))).map(|_| ())
}

fn screenshot_fn(o: Value) -> R<String> {
    let n = tick("screenshot")?;
    if with(|c| c.images.len()) >= MAX_SCREENSHOTS {
        return Err(rt(format!("at most {MAX_SCREENSHOTS} screenshots per script")));
    }
    let r = (|| -> Result<String, String> {
        observe::apply_size(&o, false)?;
        let monitor = actions::monitor_index(tools::u(&o, "monitor"))?;
        let window = tools::s(&o, "window");
        let shot = tools::grab(monitor, window, window.is_none() && monitor.is_none())?;
        let desc = format!("screenshot {}: {} {}x{}", with(|c| c.images.len()) + 1, shot.label, shot.rgb.width(), shot.rgb.height());
        let img = observe::image(shot, true);
        with(|c| c.images.push(img));
        Ok(desc)
    })();
    finish_step(n, "screenshot", r)
}

fn register(engine: &mut Engine) {
    // clicks and input
    engine.register_fn("click", |x: Dynamic, y: Dynamic| -> R<String> {
        step("click", "click", json!({"x": num(&x, "x")?, "y": num(&y, "y")?}))
    });
    engine.register_fn("click", |x: Dynamic, y: Dynamic, o: Dynamic| -> R<String> {
        step("click", "click", merge(json!({"x": num(&x, "x")?, "y": num(&y, "y")?}), opts(&o)?))
    });
    engine.register_fn("click_element", |t: Dynamic| -> R<String> { step("click_element", "click_element", json!({"name": text(&t, "target")?})) });
    engine.register_fn("click_element", |t: Dynamic, o: Dynamic| -> R<String> {
        step("click_element", "click_element", merge(json!({"name": text(&t, "target")?}), opts(&o)?))
    });
    engine.register_fn("click_text", |t: Dynamic| -> R<String> { step("click_text", "click_text", json!({"text": text(&t, "text")?})) });
    engine.register_fn("click_text", |t: Dynamic, o: Dynamic| -> R<String> {
        step("click_text", "click_text", merge(json!({"text": text(&t, "text")?}), opts(&o)?))
    });
    engine.register_fn("type_text", |t: Dynamic| -> R<String> { step("type_text", "type", json!({"text": text(&t, "text")?})) });
    engine.register_fn("type_text", |t: Dynamic, o: Dynamic| -> R<String> { step("type_text", "type", merge(json!({"text": text(&t, "text")?}), opts(&o)?)) });
    engine.register_fn("press_keys", |k: Dynamic| -> R<String> {
        let keys = if let Some(a) = k.clone().try_cast::<Array>() { Value::Array(a.iter().map(|x| json!(x.to_string())).collect()) } else { json!([text(&k, "keys")?]) };
        step("press_keys", "keys", json!({"keys": keys}))
    });
    engine.register_fn("scroll", |x: Dynamic, y: Dynamic, amount: Dynamic| -> R<String> {
        step("scroll", "scroll", json!({"x": num(&x, "x")?, "y": num(&y, "y")?, "amount": num(&amount, "amount")? as i64}))
    });
    engine.register_fn("scroll", |x: Dynamic, y: Dynamic, amount: Dynamic, h: Dynamic| -> R<String> {
        step("scroll", "scroll", json!({"x": num(&x, "x")?, "y": num(&y, "y")?, "amount": num(&amount, "amount")? as i64, "horizontal": num(&h, "horizontal")? as i64}))
    });
    engine.register_fn("drag", |x: Dynamic, y: Dynamic, tx: Dynamic, ty: Dynamic| -> R<String> {
        step("drag", "drag", json!({"x": num(&x, "x")?, "y": num(&y, "y")?, "to_x": num(&tx, "to_x")?, "to_y": num(&ty, "to_y")?}))
    });
    engine.register_fn("set_value", |t: Dynamic, v: Dynamic| -> R<String> {
        step("set_value", "set_value", json!({"name": text(&t, "target")?, "value": text(&v, "value")?}))
    });
    engine.register_fn("read_text", || -> R<String> { step("read_text", "read_text", json!({})) });
    engine.register_fn("read_text", |t: Dynamic| -> R<String> { step("read_text", "read_text", json!({"name": text(&t, "target")?})) });
    engine.register_fn("list_elements", || -> R<String> { list_elements_fn(300) });
    engine.register_fn("list_elements", |max: Dynamic| -> R<String> { list_elements_fn(num(&max, "max")? as usize) });
    engine.register_fn("focus_window", |n: Dynamic| -> R<String> { step("focus_window", "focus_window", json!({"name": text(&n, "name")?})) });
    engine.register_fn("open_app", |n: Dynamic| -> R<String> { step("open_app", "open_app", json!({"name": text(&n, "name")?})) });
    // finding
    engine.register_fn("find", |t: Dynamic| -> R<Map> { find_map(&text(&t, "target")?) });
    engine.register_fn("find_text", |t: Dynamic| -> R<Map> { find_text_map(&text(&t, "text")?, json!({})) });
    engine.register_fn("find_text", |t: Dynamic, o: Dynamic| -> R<Map> { find_text_map(&text(&t, "text")?, opts(&o)?) });
    // waiting
    engine.register_fn("wait_for", |n: Dynamic| -> R<bool> { wait_generic("wait_for", &text(&n, "name")?, json!({}), false) });
    engine.register_fn("wait_for", |n: Dynamic, o: Dynamic| -> R<bool> { wait_generic("wait_for", &text(&n, "name")?, wait_opts(&o)?, false) });
    engine.register_fn("wait_gone", |n: Dynamic| -> R<bool> { wait_generic("wait_gone", &text(&n, "name")?, json!({}), true) });
    engine.register_fn("wait_gone", |n: Dynamic, o: Dynamic| -> R<bool> { wait_generic("wait_gone", &text(&n, "name")?, wait_opts(&o)?, true) });
    // rhai's own sleep(seconds) and log(x) (natural logarithm) take INT / FLOAT: register the same
    // signatures so ours win for numbers (a Dynamic overload would lose to the built-in)
    engine.register_fn("sleep", |ms: i64| -> R<()> { sleep_ms(ms) });
    engine.register_fn("sleep", |ms: f64| -> R<()> { sleep_ms(ms.round() as i64) });
    // state and output
    engine.register_fn("active_window", || -> R<String> {
        tick("active_window")?;
        Ok(win::active_title())
    });
    engine.register_fn("screenshot", || -> R<String> { screenshot_fn(json!({})) });
    engine.register_fn("screenshot", |o: Dynamic| -> R<String> { screenshot_fn(opts(&o)?) });
    engine.register_fn("log", |m: Dynamic| {
        add_log(m.to_string());
    });
    engine.register_fn("log", |m: i64| {
        add_log(m.to_string());
    });
    engine.register_fn("log", |m: f64| {
        add_log(m.to_string());
    });
    engine.on_print(|s| add_log(s.to_string()));
    engine.on_debug(|s, _, _| add_log(s.to_string()));
}

fn list_elements_fn(max: usize) -> R<String> {
    let n = tick("list_elements")?;
    let r = crate::uia::list_elements(max.max(1)).map(|l| l.text);
    finish_step(n, "list_elements", r.clone().map(|t| format!("{} lines", t.lines().count())))?;
    r.map_err(rt)
}

// ---------- running ----------

pub struct Outcome {
    /// the script's report (log, return value, counts), always present
    pub report: String,
    pub images: Vec<Content>,
    /// the script ended because the user pressed Esc / moved to a corner, or the client cancelled
    pub stopped: Option<String>,
}

/// "line N: message" from an engine error (the innermost position wins).
fn describe_error(e: &EvalAltResult) -> (String, Option<usize>) {
    match e {
        EvalAltResult::ErrorInFunctionCall(name, _src, inner, pos) => {
            let (m, l) = describe_error(inner);
            (format!("{m} (in function {name})"), l.or(pos.line()))
        }
        EvalAltResult::ErrorRuntime(d, pos) => (d.to_string(), pos.line()),
        EvalAltResult::ErrorTooManyOperations(pos) => (format!("operation limit reached ({MAX_OPERATIONS} operations; an endless loop?)"), pos.line()),
        EvalAltResult::ErrorStackOverflow(pos) => ("call depth limit reached (recursion too deep)".into(), pos.line()),
        EvalAltResult::ErrorDataTooLarge(what, pos) => (format!("{what} too large (limits: strings 100000 chars, arrays 1000, maps 500)"), pos.line()),
        EvalAltResult::ErrorTerminated(tok, pos) => (tok.to_string(), pos.line()),
        other => (other.to_string().split(" (line").next().unwrap_or("").to_string(), other.position().line()),
    }
}

pub fn run(code: &str, timeout_s: f64) -> Outcome {
    let timeout_s = timeout_s.clamp(1.0, 300.0);
    let started = Instant::now();
    CTX.with(|c| {
        *c.borrow_mut() = Some(Ctx {
            started,
            deadline: started + Duration::from_secs_f64(timeout_s),
            timeout_s,
            steps: 0,
            log: Vec::new(),
            log_chars: 0,
            trace: Vec::new(),
            images: Vec::new(),
            stop: None,
            fatal: None,
        })
    });
    let mut engine = Engine::new();
    engine.set_max_operations(MAX_OPERATIONS);
    engine.set_max_call_levels(24);
    engine.set_max_expr_depths(48, 24);
    engine.set_max_string_size(100_000);
    engine.set_max_array_size(1_000);
    engine.set_max_map_size(500);
    engine.disable_symbol("eval");
    engine.on_progress(|ops| {
        // a terminal refusal (stop, cancellation, takeover) ends the run at the very next operation, even inside try/catch
        if let Some(f) = with(|c| c.fatal.clone()) {
            return Some(Dynamic::from(f));
        }
        if ops % 64 == 0 {
            if with(|c| Instant::now() >= c.deadline) {
                with(|c| c.stop = Some("timeout"));
                return Some(Dynamic::from("timeout".to_string()));
            }
            if let Err(e) = guards::check_failsafe() {
                note_terminal(&e);
                return Some(Dynamic::from(e));
            }
        }
        None
    });
    register(&mut engine);

    let mut stopped: Option<String> = None;
    let outcome: Result<Dynamic, (String, Option<usize>)> = match engine.compile(code) {
        Err(e) => Err((format!("syntax error: {}", e.err_type()), e.position().line())),
        Ok(ast) => {
            let mut scope = Scope::new();
            match engine.eval_ast_with_scope::<Dynamic>(&mut scope, &ast) {
                Ok(v) => Ok(v),
                Err(e) => {
                    let (m, l) = describe_error(&e);
                    if is_stop(&m) {
                        stopped = Some(m.clone());
                    }
                    if matches!(*e, EvalAltResult::ErrorTerminated(..)) {
                        let why = with(|c| c.stop);
                        match why {
                            Some("timeout") => Err((format!("script timed out after {timeout_s} s"), l)),
                            _ => {
                                stopped = Some(m.clone());
                                Err((m, l))
                            }
                        }
                    } else {
                        Err((m, l))
                    }
                }
            }
        }
    };
    let ctx = CTX.with(|c| c.borrow_mut().take()).expect("ctx");
    if stopped.is_none() {
        // the script caught a terminal refusal and then finished without another operation: it still ended the run
        stopped = ctx.fatal.clone();
    }
    let secs = ctx.started.elapsed().as_secs_f64();
    let mut lines: Vec<String> = Vec::new();
    match &outcome {
        Ok(v) => {
            lines.push(format!("Script finished: {} action(s) in {:.1} s.", ctx.steps, secs));
            if !ctx.log.is_empty() {
                lines.push("Log:".into());
                lines.extend(ctx.log.iter().map(|l| format!("  {l}")));
            }
            let shown = if v.is_unit() { "(nothing)".to_string() } else { v.to_string() };
            lines.push(format!("Returned: {}", shown.chars().take(2000).collect::<String>()));
        }
        Err((m, line)) => {
            let at = line.map(|l| format!(" at line {l}")).unwrap_or_default();
            lines.push(format!("SCRIPT FAILED{at}: {m}"));
            lines.push(format!("{} action(s) ran in {:.1} s before it stopped.", ctx.steps, secs));
            if !ctx.log.is_empty() {
                lines.push("Log so far:".into());
                lines.extend(ctx.log.iter().map(|l| format!("  {l}")));
            }
            if !ctx.trace.is_empty() {
                lines.push("Last actions:".into());
                lines.extend(ctx.trace.iter().map(|l| format!("  {l}")));
            }
        }
    }
    if !ctx.images.is_empty() {
        lines.push(format!("{} screenshot(s) follow.", ctx.images.len()));
    }
    Outcome { report: lines.join("\n"), images: ctx.images, stopped }
}
