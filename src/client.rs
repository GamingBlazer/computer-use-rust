//! Which AI client is connected (MCP clientInfo), and the indicator colour and name that follow it.
//!
//! The name comes from `initialize` params `clientInfo` (legacy handshake) or, for the 2026-07-28
//! protocol, from `_meta["io.modelcontextprotocol/clientInfo"]` on any request; the latest one seen wins.
//! Colour precedence: config.json `clientColors` (substring of the client name -> hex), then
//! `accentColor` (a global override, kept for older configs), then the built-in table, then indigo.

use crate::sync::Mutex;

use serde_json::Value;

pub const DEFAULT_ACCENT: &str = "#6366F1";
const META_CLIENT: &str = "io.modelcontextprotocol/clientInfo";

static CLIENT: Mutex<Option<(String, String)>> = Mutex::new(None);

/// (display name, built-in accent) for a client name: case-insensitive substrings, first match wins.
/// Cursor is tested before VS Code because "cursor-vscode" contains "vscode".
pub fn builtin(name: &str) -> Option<(&'static str, &'static str)> {
    let n = name.to_lowercase();
    let has = |subs: &[&str]| subs.iter().any(|s| n.contains(s));
    if has(&["claude"]) {
        Some(("Claude", "#D97757"))
    } else if has(&["codex"]) {
        Some(("Codex", "#10A37F"))
    } else if has(&["chatgpt", "openai"]) {
        Some(("ChatGPT", "#10A37F"))
    } else if has(&["gemini"]) {
        Some(("Gemini", "#4285F4"))
    } else if has(&["cursor"]) {
        Some(("Cursor", "#A1A1AA"))
    } else if has(&["windsurf", "codeium"]) {
        Some(("Windsurf", "#09B6A2"))
    } else if has(&["copilot", "visual studio code", "vscode"]) {
        Some(("Copilot", "#8957E5"))
    } else {
        None
    }
}

/// The accent for a client name given the config (pure, for tests): clientColors, accentColor, built-in, default.
pub fn accent_for(name: &str, config: &Value) -> String {
    let n = name.to_lowercase();
    if let Some(map) = config.get("clientColors").and_then(Value::as_object) {
        for (sub, hex) in map {
            if let Some(hex) = hex.as_str() {
                if !sub.is_empty() && n.contains(&sub.to_lowercase()) {
                    return hex.to_string();
                }
            }
        }
    }
    if let Some(a) = config.get("accentColor").and_then(Value::as_str).filter(|a| !a.is_empty()) {
        return a.to_string();
    }
    builtin(name).map(|(_, c)| c.to_string()).unwrap_or_else(|| DEFAULT_ACCENT.to_string())
}

/// Remember the client named in a request's params, and re-colour the indicator when it changed.
pub fn note(params: &Value) {
    let info = params.get("clientInfo").or_else(|| params.get("_meta").and_then(|m| m.get(META_CLIENT)));
    let Some(info) = info else { return };
    let name = info.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let version = info.get("version").and_then(Value::as_str).unwrap_or("").to_string();
    if name.is_empty() {
        return;
    }
    let changed = {
        let mut c = CLIENT.lock().unwrap();
        let changed = c.as_ref().map(|(n, _)| n != &name).unwrap_or(true);
        *c = Some((name, version));
        changed
    };
    if changed {
        crate::overlay::set_style(&accent(), &using_text());
    }
}

pub fn raw() -> Option<(String, String)> {
    CLIENT.lock().unwrap().clone()
}

/// Display name of the connected client, None when unknown.
pub fn display() -> Option<&'static str> {
    raw().and_then(|(n, _)| builtin(&n).map(|(d, _)| d))
}

/// The accent colour in use now.
pub fn accent() -> String {
    accent_for(&raw().map(|(n, _)| n).unwrap_or_default(), crate::config())
}

/// "<Name> is using your computer", "AI is using your computer" when unknown; strings.usingComputer overrides.
pub fn using_text() -> String {
    if let Some(s) = crate::config().pointer("/strings/usingComputer").and_then(Value::as_str) {
        return s.to_string();
    }
    format!("{} is using your computer", display().unwrap_or("AI"))
}

/// "✓  <Name> finished" or "✓  Done", with the summary after it.
pub fn done_banner(summary: Option<&str>) -> String {
    let (head, trivial) = match display() {
        Some(n) => (format!("✓  {n} finished"), false),
        None => ("✓  Done".to_string(), true),
    };
    match summary {
        Some(s) if !(trivial && s == "Done") => format!("{head}  ·  {s}"),
        _ => head,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_map_to_colours() {
        for (name, disp, hex) in [
            ("claude-code", "Claude", "#D97757"),
            ("claude-ai", "Claude", "#D97757"),
            ("codex-mcp-client", "Codex", "#10A37F"),
            ("openai-mcp", "ChatGPT", "#10A37F"),
            ("gemini-cli-mcp-client", "Gemini", "#4285F4"),
            ("Visual Studio Code", "Copilot", "#8957E5"),
            ("cursor-vscode", "Cursor", "#A1A1AA"),
            ("windsurf", "Windsurf", "#09B6A2"),
        ] {
            assert_eq!(builtin(name), Some((disp, hex)), "{name}");
        }
        assert_eq!(builtin("mystery-agent"), None);
    }

    #[test]
    fn config_precedence() {
        let none = json!({});
        assert_eq!(accent_for("mystery-agent", &none), DEFAULT_ACCENT);
        assert_eq!(accent_for("claude-code", &none), "#D97757");
        let global = json!({"accentColor": "#112233"});
        assert_eq!(accent_for("claude-code", &global), "#112233");
        assert_eq!(accent_for("mystery-agent", &global), "#112233");
        let per = json!({"accentColor": "#112233", "clientColors": {"CLAUDE": "#445566"}});
        assert_eq!(accent_for("claude-code", &per), "#445566");
        assert_eq!(accent_for("codex", &per), "#112233");
    }
}
