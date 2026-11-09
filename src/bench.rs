//! `desktop-control.exe bench <calls.json> [--repeat N]`: runs a JSON list of
//! `{"tool": ..., "args": {...}}` in-process through the same registry the MCP server uses and
//! prints one line per step (time, images, image size, estimated image tokens, text characters)
//! plus totals. Image data is never printed.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::tools::{Content, Registry};

struct Stat {
    ms: f64,
    images: usize,
    tokens: f64,
    chars: usize,
    error: bool,
}

fn image_size(b64: &str) -> Option<(u32, u32)> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

pub fn run(path: &str, repeat: usize, registry: &Registry, show: bool) -> i32 {
    let calls: Vec<Value> = match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bench: cannot read {path}: {e}");
            return 2;
        }
    };
    println!("bench {path}: {} steps x {repeat} run(s)", calls.len());
    let mut all: Vec<Stat> = Vec::new();
    let mut run_totals: Vec<f64> = Vec::new();
    for run in 1..=repeat {
        println!("--- run {run}/{repeat}");
        println!("{:>3}  {:<14} {:>7}  {:>4}  {:<18} {:>9}  {:>6}  {}", "#", "tool", "ms", "imgs", "image px", "est_tok", "chars", "note");
        let run_start = Instant::now();
        for (i, call) in calls.iter().enumerate() {
            let tool = call.get("tool").and_then(Value::as_str).unwrap_or("?");
            let args = call.get("args").cloned().unwrap_or(Value::Object(Default::default()));
            let t = Instant::now();
            let out = registry.call(tool, args);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let (mut images, mut tokens, mut chars, mut sizes, mut note, mut error) = (0usize, 0f64, 0usize, Vec::new(), String::new(), false);
            match out {
                Ok(o) => {
                    for c in &o.content {
                        match c {
                            Content::Text { text } => {
                                chars += text.chars().count();
                                if show {
                                    for l in text.lines() {
                                        println!("       | {l}");
                                    }
                                }
                            }
                            Content::Image { data, .. } => {
                                images += 1;
                                if let Some((w, h)) = image_size(data) {
                                    sizes.push(format!("{w}x{h}"));
                                    tokens += w as f64 * h as f64 / 750.0;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error = true;
                    note = format!("ERR {}", e.chars().take(80).collect::<String>().replace('\n', " "));
                    chars += e.chars().count();
                }
            }
            println!("{:>3}  {:<14} {:>7.0}  {:>4}  {:<18} {:>9.0}  {:>6}  {}", i + 1, tool, ms, images, sizes.join(","), tokens, chars, note);
            all.push(Stat { ms, images, tokens, chars, error });
            if matches!(call.get("pause_ms").and_then(Value::as_u64), Some(p) if p > 0) {
                std::thread::sleep(Duration::from_millis(call["pause_ms"].as_u64().unwrap()));
            }
        }
        let rt = run_start.elapsed().as_secs_f64() * 1000.0;
        run_totals.push(rt);
        println!("run {run} wall time: {rt:.0} ms");
        std::thread::sleep(Duration::from_millis(500));
    }
    let n = repeat.max(1) as f64;
    let ms: f64 = all.iter().map(|s| s.ms).sum();
    let images: usize = all.iter().map(|s| s.images).sum();
    let tokens: f64 = all.iter().map(|s| s.tokens).sum();
    let chars: usize = all.iter().map(|s| s.chars).sum();
    let errors = all.iter().filter(|s| s.error).count();
    println!("=== totals over {repeat} run(s), {} steps", all.len());
    println!("TOTAL ms={ms:.0} images={images} est_image_tokens={tokens:.0} text_chars={chars} errors={errors}");
    println!("PER RUN avg ms={:.0} images={:.1} est_image_tokens={:.0} text_chars={:.0} (sum of step times; wall avg {:.0} ms)", ms / n, images as f64 / n, tokens / n, chars as f64 / n, run_totals.iter().sum::<f64>() / n);
    println!("PER STEP avg ms={:.1}", ms / all.len().max(1) as f64);
    if errors > 0 { 1 } else { 0 }
}
