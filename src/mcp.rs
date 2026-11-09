//! MCP (Model Context Protocol) server over stdio: newline-delimited JSON-RPC 2.0.
//!
//! Dual-era. The 2026-07-28 revision is stateless: no `initialize`, every request carries
//! `_meta["io.modelcontextprotocol/protocolVersion"]`, and `server/discover` advertises the
//! supported versions, capabilities, identity and instructions. A request that carries that
//! version is served on its own, with no dependency on any handshake. The legacy handshake
//! (`initialize` / `notifications/initialized`, protocol versions 2025-03-26, 2025-06-18 and
//! 2025-11-25) keeps working unchanged for older clients; a request without the version key is
//! only accepted after `initialize`.
//!
//! Deliberately not implemented in this revision:
//!  * Tasks (extension `io.modelcontextprotocol/tasks`): every tool call here is short or can be
//!    cancelled; a task handle would only add a polling round-trip.
//!  * Multi round-trip requests (MRTR, `input_required`): the server never needs mid-call input
//!    from the user; the screen is the only interface and the safety prompts live in Claude Code.
//!  * Streamable HTTP and `subscriptions/listen`: this is a local stdio server and the tool list
//!    is static, so there are no list-changed notifications to subscribe to.
//!
//! Threads: stdin is read on its own thread so a `notifications/cancelled` is seen while a tool
//! runs; tools still execute one at a time on the main thread (one mouse, one keyboard).

use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use crate::sync::Mutex;

use serde_json::{json, Value};

use crate::tools::{Content, Registry};

/// The revision this server is written for.
pub const PROTOCOL: &str = "2026-07-28";
/// Everything it serves: the modern revision first, then the legacy ones with an initialize handshake.
pub const SUPPORTED: [&str; 4] = ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"];
pub const INSTRUCTIONS: &str = include_str!("instructions.txt");

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
/// error codes: -32602 invalid params; -32022 UnsupportedProtocolVersion (reserved by the 2026-07-28 spec)
const INVALID_PARAMS: i64 = -32602;
const UNSUPPORTED_VERSION: i64 = -32022;
/// The tool list never changes while the process runs; one hour is a conservative hint.
const LIST_TTL_MS: u64 = 3_600_000;

/// A legacy `initialize` happened on this process.
static INITIALIZED: AtomicBool = AtomicBool::new(false);
/// Request ids (JSON text) received and not yet answered.
static PENDING: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// Ids the client cancelled while pending.
static CANCELLED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// The request being executed.
static CURRENT: Mutex<Option<String>> = Mutex::new(None);
/// progressToken of the request being executed.
static PROGRESS: Mutex<Option<Value>> = Mutex::new(None);

fn server_info() -> Value {
    json!({"name": "desktop-control", "version": env!("CARGO_PKG_VERSION")})
}

/// True when the client cancelled the request that is executing; long operations poll this.
pub fn cancelled() -> bool {
    let cur = CURRENT.lock().unwrap();
    match cur.as_ref() {
        Some(id) => CANCELLED.lock().unwrap().as_ref().map(|s| s.contains(id)).unwrap_or(false),
        None => false,
    }
}

/// `notifications/progress` for the request being executed, when it carried a progressToken.
pub fn progress(done: f64, total: f64, message: &str) {
    let Some(token) = PROGRESS.lock().unwrap().clone() else { return };
    write_msg(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {"progressToken": token, "progress": done, "total": total, "message": message}
    }));
}

/// `notifications/progress` without a known total (a script: the number of steps so far).
pub fn progress_count(done: f64, message: &str) {
    let Some(token) = PROGRESS.lock().unwrap().clone() else { return };
    write_msg(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {"progressToken": token, "progress": done, "message": message}
    }));
}

/// The longest request line accepted (bytes). A longer one is skipped and answered with an error, so a
/// broken or hostile client can't make the server buffer without limit.
pub const MAX_LINE: usize = 32 * 1024 * 1024;

/// Read one line (without its newline) into `buf`, keeping at most `max` bytes. Some(true): a line is in
/// `buf`; Some(false): the line was longer than `max` and has been skipped; None: end of input.
pub fn read_line_limited<R: BufRead>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> io::Result<Option<bool>> {
    buf.clear();
    let mut seen = 0usize;
    let mut too_long = false;
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if seen == 0 { None } else { Some(!too_long) });
        }
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            if buf.len() + take > max {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(&chunk[..take]);
            }
        }
        seen += take + usize::from(done);
        r.consume(take + usize::from(done));
        if done {
            return Ok(Some(!too_long));
        }
    }
}

fn spawn_reader() -> mpsc::Receiver<Value> {
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut raw: Vec<u8> = Vec::new();
            loop {
                match read_line_limited(&mut stdin, &mut raw, MAX_LINE) {
                    Ok(Some(true)) => {}
                    Ok(Some(false)) => {
                        write_msg(&error(Value::Null, -32600, &format!("request too large: a line is limited to {} MB and was skipped", MAX_LINE / (1024 * 1024)), None));
                        continue;
                    }
                    _ => break,
                }
                let Ok(line) = std::str::from_utf8(&raw) else {
                    write_msg(&error(Value::Null, -32700, "parse error: the line is not valid UTF-8", None));
                    continue;
                };
                let line = line.trim_end_matches('\r');
                if line.trim().is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(e) => {
                        write_msg(&error(Value::Null, -32700, &format!("parse error: {e}"), None));
                        continue;
                    }
                };
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
                if method == "notifications/cancelled" {
                    // only a request still pending can be cancelled; anything else is ignored
                    if let Some(rid) = msg.pointer("/params/requestId") {
                        let key = rid.to_string();
                        let pending = PENDING.lock().unwrap().as_ref().map(|s| s.contains(&key)).unwrap_or(false);
                        if pending {
                            let reason = msg.pointer("/params/reason").and_then(Value::as_str).unwrap_or("");
                            eprintln!("mcp: request {key} cancelled by the client {reason}");
                            CANCELLED.lock().unwrap().get_or_insert_with(HashSet::new).insert(key);
                        }
                    }
                    continue;
                }
                if let Some(id) = msg.get("id").filter(|i| !i.is_null()) {
                    PENDING.lock().unwrap().get_or_insert_with(HashSet::new).insert(id.to_string());
                }
                if tx.send(msg).is_err() {
                    break;
                }
            }
            // stdin closed: the client is gone; dropping tx ends serve()
        })
        .expect("stdin thread");
    rx
}

pub fn serve(registry: Registry) {
    let rx = spawn_reader();
    for msg in rx {
        handle(&registry, msg);
    }
}

fn handle(registry: &Registry, msg: Value) {
    let Some(id) = msg.get("id").filter(|i| !i.is_null()).cloned() else {
        return; // notification (initialized, ...): nothing to answer
    };
    let key = id.to_string();
    let finish_request = |key: &str| {
        PENDING.lock().unwrap().as_mut().map(|s| s.remove(key));
        CANCELLED.lock().unwrap().as_mut().map(|s| s.remove(key));
        *CURRENT.lock().unwrap() = None;
        *PROGRESS.lock().unwrap() = None;
    };
    // cancelled before it started
    if CANCELLED.lock().unwrap().as_ref().map(|s| s.contains(&key)).unwrap_or(false) {
        finish_request(&key);
        return;
    }
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    *CURRENT.lock().unwrap() = Some(key.clone());
    *PROGRESS.lock().unwrap() = params.pointer("/_meta/progressToken").filter(|t| t.is_string() || t.is_number()).cloned();
    let reply = dispatch(registry, id, &method, &params);
    let was_cancelled = cancelled();
    finish_request(&key);
    if was_cancelled {
        return; // per spec: no response for a cancelled request
    }
    write_msg(&reply);
}

/// How a request identifies its protocol era.
#[derive(Clone, Copy, PartialEq)]
enum Era {
    Modern,
    Legacy,
}

/// Version handling per request: the `_meta` key decides; without it the request must follow a
/// legacy `initialize`.
fn era(id: &Value, params: &Value) -> Result<Era, Value> {
    match params.pointer("/_meta").and_then(|m| m.get(META_VERSION)) {
        Some(Value::String(v)) => {
            if !SUPPORTED.contains(&v.as_str()) {
                return Err(error(
                    id.clone(),
                    UNSUPPORTED_VERSION,
                    "Unsupported protocol version",
                    Some(json!({"supported": SUPPORTED, "requested": v})),
                ));
            }
            Ok(if v == PROTOCOL { Era::Modern } else { Era::Legacy })
        }
        Some(_) => Err(error(id.clone(), INVALID_PARAMS, &format!("_meta[{META_VERSION}] must be a string"), None)),
        None if INITIALIZED.load(Ordering::Relaxed) => Ok(Era::Legacy),
        None => Err(error(
            id.clone(),
            INVALID_PARAMS,
            &format!("missing _meta[{META_VERSION}] (e.g. \"{PROTOCOL}\"); legacy clients must call initialize first"),
            Some(json!({"supported": SUPPORTED})),
        )),
    }
}

/// Modern results carry `resultType` and the server's identity in `_meta`.
fn finish_result(era: Era, mut result: Value) -> Value {
    if era == Era::Modern {
        result["resultType"] = json!("complete");
        let meta = result.as_object_mut().unwrap().entry("_meta").or_insert_with(|| json!({}));
        meta[META_SERVER_INFO] = server_info();
    }
    result
}

fn dispatch(registry: &Registry, id: Value, method: &str, params: &Value) -> Value {
    crate::client::note(params); // who is connected: the indicator's colour and name follow the client
    match method {
        "initialize" => {
            // legacy handshake (2025-11-25 and earlier): echo a version we serve, else our newest legacy one
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = if SUPPORTED[1..].contains(&asked) { asked } else { SUPPORTED[1] };
            INITIALIZED.store(true, Ordering::Relaxed);
            result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": server_info(),
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "server/discover" => {
            // answered even without _meta (it is the probe); an unsupported version is still reported
            if let Some(Value::String(v)) = params.pointer("/_meta").and_then(|m| m.get(META_VERSION)) {
                if !SUPPORTED.contains(&v.as_str()) {
                    return error(id, UNSUPPORTED_VERSION, "Unsupported protocol version", Some(json!({"supported": SUPPORTED, "requested": v})));
                }
            }
            result(
                id,
                finish_result(
                    Era::Modern,
                    json!({
                        "supportedVersions": SUPPORTED,
                        "capabilities": {"tools": {"listChanged": false}},
                        "instructions": INSTRUCTIONS,
                        "ttlMs": LIST_TTL_MS,
                        "cacheScope": "public",
                    }),
                ),
            )
        }
        "ping" => result(id, json!({})),
        "tools/list" => match era(&id, params) {
            Err(e) => e,
            Ok(era) => result(
                id,
                finish_result(era, json!({"tools": registry.list(), "ttlMs": LIST_TTL_MS, "cacheScope": "public"})),
            ),
        },
        "tools/call" => match era(&id, params) {
            Err(e) => e,
            Ok(era) => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return error(id, INVALID_PARAMS, "tools/call needs a tool name", None);
                };
                if !registry.has(name) {
                    return error(id, INVALID_PARAMS, &format!("Unknown tool: {name}"), None);
                }
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let body = match registry.call(name, args) {
                    Ok(out) => {
                        let mut r = json!({"content": out.content, "isError": false});
                        if let Some(sc) = out.structured {
                            r["structuredContent"] = sc;
                        }
                        r
                    }
                    Err(e) => json!({"content": [Content::text(e)], "isError": true}),
                };
                result(id, finish_result(era, body))
            }
        },
        _ => error(id, -32601, &format!("method not found: {method}"), None),
    }
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut e = json!({"code": code, "message": message});
    if let Some(d) = data {
        e["data"] = d;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": e})
}

/// One message per line, written atomically (the reader thread and the main thread both write).
fn write_msg(msg: &Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = serde_json::to_writer(&mut out, msg);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_lines_are_skipped_and_the_next_one_is_read() {
        let data = b"short\n0123456789ABCDEF\nok\r\nlast".to_vec();
        let mut r = io::Cursor::new(data);
        let mut buf = Vec::new();
        assert_eq!(read_line_limited(&mut r, &mut buf, 8).unwrap(), Some(true));
        assert_eq!(buf, b"short");
        assert_eq!(read_line_limited(&mut r, &mut buf, 8).unwrap(), Some(false)); // 16 bytes > 8
        assert!(buf.is_empty());
        assert_eq!(read_line_limited(&mut r, &mut buf, 8).unwrap(), Some(true));
        assert_eq!(buf, b"ok\r");
        assert_eq!(read_line_limited(&mut r, &mut buf, 8).unwrap(), Some(true)); // no trailing newline
        assert_eq!(buf, b"last");
        assert_eq!(read_line_limited(&mut r, &mut buf, 8).unwrap(), None);
    }
}
