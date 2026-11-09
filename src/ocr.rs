//! On-screen text as a universal fallback (apps with no or poor UI Automation: canvas apps,
//! games, Qt/Java, remote-desktop and VM windows). Uses the Windows.Media.Ocr engine that ships
//! with Windows, in-process, on the full-resolution capture (not the downscaled JPEG).
//!
//! * Engine: the user-profile languages, English as the fallback; `lang` picks a BCP-47 tag.
//! * `OcrEngine.MaxImageDimension` (10000 on current Windows, 2600 on old ones) is respected:
//!   a larger image is scaled down to fit and the boxes are scaled back up.
//! * Results are cached per picture (hash of the pixels, plus language), so asking again about
//!   an unchanged screen costs a capture and a hash, not a recognition run.
//! * OCR returns words with boxes; matching joins the words of one line, so a phrase spanning
//!   several words matches, and a match box is the union of its words.

use crate::sync::Mutex;
use std::sync::Arc;
use std::time::Instant;

use image::RgbaImage;
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;

#[derive(Clone, Debug)]
pub struct Word {
    pub text: String,
    /// box in capture pixels
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub words: Vec<Word>,
}

impl Line {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Debug)]
pub struct Recognised {
    pub lines: Vec<Line>,
    pub lang: String,
    /// time of the recognition run that produced this (not of a cache hit)
    pub ms: u128,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MatchMode {
    Contains,
    Exact,
    Regex,
}

impl MatchMode {
    pub fn parse(s: Option<&str>) -> Result<MatchMode, String> {
        match s.map(|s| s.trim().to_lowercase()).as_deref() {
            None | Some("") | Some("contains") => Ok(MatchMode::Contains),
            Some("exact") => Ok(MatchMode::Exact),
            Some("regex") => Ok(MatchMode::Regex),
            Some(o) => Err(format!("match must be contains, exact or regex, not {o:?}")),
        }
    }
}

/// One match: the matched text, its line, and its box in capture pixels.
#[derive(Clone, Debug)]
pub struct TextMatch {
    pub text: String,
    pub line: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// the match is the whole line, or whole words
    pub exact: bool,
}

// ---------- engine ----------

struct EngineBox(OcrEngine, String);
unsafe impl Send for EngineBox {}

static ENGINES: Mutex<Vec<(String, EngineBox)>> = Mutex::new(Vec::new());

fn make_engine(lang: Option<&str>) -> Result<(OcrEngine, String), String> {
    if let Some(tag) = lang.filter(|l| !l.trim().is_empty()) {
        let l = Language::CreateLanguage(&HSTRING::from(tag.trim())).map_err(|e| format!("language {tag:?}: {e}"))?;
        if !OcrEngine::IsLanguageSupported(&l).unwrap_or(false) {
            let avail = OcrEngine::AvailableRecognizerLanguages()
                .map(|v| v.into_iter().filter_map(|l| l.LanguageTag().ok()).map(|t| t.to_string()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            return Err(format!("OCR language {tag:?} is not installed (available: {avail}); add it in Settings > Time & language > Language & region"));
        }
        let e = OcrEngine::TryCreateFromLanguage(&l).map_err(|e| format!("OCR engine for {tag:?}: {e}"))?;
        return Ok((e, tag.trim().to_string()));
    }
    let tag_of = |e: &OcrEngine| e.RecognizerLanguage().and_then(|l| l.LanguageTag()).map(|t| t.to_string()).unwrap_or_default();
    if let Ok(e) = OcrEngine::TryCreateFromUserProfileLanguages() {
        let t = tag_of(&e);
        return Ok((e, t));
    }
    let en = Language::CreateLanguage(&HSTRING::from("en-US")).map_err(|e| e.to_string())?;
    if let Ok(e) = OcrEngine::TryCreateFromLanguage(&en) {
        return Ok((e, "en-US".into()));
    }
    // any installed recognizer language
    if let Ok(list) = OcrEngine::AvailableRecognizerLanguages() {
        for l in list {
            if let Ok(e) = OcrEngine::TryCreateFromLanguage(&l) {
                let t = tag_of(&e);
                return Ok((e, t));
            }
        }
    }
    Err("OCR is unavailable: Windows has no OCR language installed (Settings > Time & language > Language & region > add a language with 'Text recognition')".into())
}

fn engine(lang: Option<&str>) -> Result<(OcrEngine, String), String> {
    let key = lang.unwrap_or("").trim().to_lowercase();
    let mut list = ENGINES.lock().unwrap();
    if let Some((_, b)) = list.iter().find(|(k, _)| *k == key) {
        return Ok((b.0.clone(), b.1.clone()));
    }
    let (e, tag) = make_engine(lang)?;
    list.push((key, EngineBox(e.clone(), tag.clone())));
    Ok((e, tag))
}

// ---------- recognition ----------

fn hash_image(img: &RgbaImage) -> u64 {
    let mut hash = 0xcbf29ce484222325u64 ^ ((img.width() as u64) << 32 | img.height() as u64);
    let mut chunks = img.as_raw().chunks_exact(8);
    for c in &mut chunks {
        hash = (hash ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(0x100000001b3).rotate_left(23);
    }
    for &b in chunks.remainder() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    hash
}

struct CacheEntry {
    hash: u64,
    lang: String,
    rec: Arc<Recognised>,
}
static CACHE: Mutex<Vec<CacheEntry>> = Mutex::new(Vec::new());
const CACHE_SIZE: usize = 6;

/// How the last `recognise` was served (for timing reports and tests).
pub static LAST_CACHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn software_bitmap(img: &RgbaImage) -> Result<SoftwareBitmap, String> {
    let (w, h) = (img.width(), img.height());
    // Gray8 keeps the buffer a quarter of BGRA; the engine works on luminance anyway
    let mut gray = Vec::with_capacity((w * h) as usize);
    for p in img.as_raw().chunks_exact(4) {
        gray.push(((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8);
    }
    let writer = DataWriter::new().map_err(|e| e.to_string())?;
    writer.WriteBytes(&gray).map_err(|e| e.to_string())?;
    let buffer = writer.DetachBuffer().map_err(|e| e.to_string())?;
    SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Gray8, w as i32, h as i32).map_err(|e| format!("SoftwareBitmap: {e}"))
}

/// Recognise the text of a full-resolution capture. Cached by picture hash.
pub fn recognise(img: &RgbaImage, lang: Option<&str>) -> Result<Arc<Recognised>, String> {
    recognise_with(img, lang, true)
}

pub fn recognise_with(img: &RgbaImage, lang: Option<&str>, use_cache: bool) -> Result<Arc<Recognised>, String> {
    let hash = hash_image(img);
    let lang_key = lang.unwrap_or("").trim().to_lowercase();
    if use_cache {
        let mut cache = CACHE.lock().unwrap();
        if let Some(i) = cache.iter().position(|c| c.hash == hash && c.lang == lang_key) {
            let e = cache.remove(i);
            let rec = e.rec.clone();
            cache.insert(0, e);
            LAST_CACHED.store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(rec);
        }
    }
    LAST_CACHED.store(false, std::sync::atomic::Ordering::Relaxed);
    let t = Instant::now();
    let (eng, tag) = engine(lang)?;
    let max = OcrEngine::MaxImageDimension().unwrap_or(2600).max(256);
    let (w, h) = (img.width(), img.height());
    // larger than the engine accepts: scale down to fit (boxes are scaled back below)
    let (scaled, factor): (Option<RgbaImage>, f32) = if w.max(h) > max {
        let f = max as f32 / w.max(h) as f32;
        let nw = ((w as f32 * f).floor() as u32).max(1);
        let nh = ((h as f32 * f).floor() as u32).max(1);
        (Some(image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle)), f)
    } else {
        (None, 1.0)
    };
    let bmp = software_bitmap(scaled.as_ref().unwrap_or(img))?;
    let result = eng.RecognizeAsync(&bmp).map_err(|e| format!("OCR: {e}"))?.get().map_err(|e| format!("OCR: {e}"))?;
    let mut lines = Vec::new();
    for l in result.Lines().map_err(|e| e.to_string())? {
        let mut words = Vec::new();
        for wd in l.Words().map_err(|e| e.to_string())? {
            let text = wd.Text().map_err(|e| e.to_string())?.to_string();
            let r = wd.BoundingRect().map_err(|e| e.to_string())?;
            if text.trim().is_empty() {
                continue;
            }
            words.push(Word { text, x: r.X / factor, y: r.Y / factor, w: r.Width / factor, h: r.Height / factor });
        }
        if !words.is_empty() {
            lines.push(Line { words });
        }
    }
    let rec = Arc::new(Recognised { lines, lang: tag, ms: t.elapsed().as_millis(), width: w, height: h });
    if use_cache {
        let mut cache = CACHE.lock().unwrap();
        cache.insert(0, CacheEntry { hash, lang: lang_key, rec: rec.clone() });
        cache.truncate(CACHE_SIZE);
    }
    Ok(rec)
}

/// `desktop-control.exe ocr-bench <image> [--repeat N] [--find TEXT]`: OCR timing on an image file
/// (the engine, not the capture): the first run includes creating the engine.
pub fn bench_file(path: &str, repeat: usize, find: Option<&str>) -> i32 {
    let img = match image::open(path) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            eprintln!("ocr-bench: cannot read {path}: {e}");
            return 2;
        }
    };
    println!("ocr-bench {path}: {}x{} px, MaxImageDimension {:?}", img.width(), img.height(), OcrEngine::MaxImageDimension());
    let mut times = Vec::new();
    for i in 0..repeat.max(1) {
        let t = Instant::now();
        match recognise_with(&img, None, false) {
            Ok(rec) => {
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                let words: usize = rec.lines.iter().map(|l| l.words.len()).sum();
                println!("run {}: {:.0} ms total ({} ms of it recognition), {} lines, {} words, language {}", i + 1, ms, rec.ms, rec.lines.len(), words, rec.lang);
                times.push(ms);
                if i == 0 {
                    if let Some(q) = find {
                        match find_matches(&rec, q, MatchMode::Contains) {
                            Ok(m) => println!("  find {q:?}: {} match(es){}", m.len(), m.first().map(|f| format!(", first at ({:.0}, {:.0}) {:.0}x{:.0}", f.x, f.y, f.w, f.h)).unwrap_or_default()),
                            Err(e) => println!("  find {q:?}: {e}"),
                        }
                    }
                }
            }
            Err(e) => {
                println!("run {}: failed: {e}", i + 1);
                return 1;
            }
        }
    }
    if times.len() > 1 {
        let warm = &times[1..];
        println!("warm average {:.0} ms (runs 2..{})", warm.iter().sum::<f64>() / warm.len() as f64, times.len());
    }
    0
}

// ---------- matching ----------

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Find `query` in the recognised lines. Phrases may span several words of one line.
/// contains: case-insensitive substring; exact: case-insensitive, whole words (so "OK" matches
/// the word OK in the line "OK Cancel", not "BOOK"); regex: Rust regex syntax, case-insensitive
/// unless the pattern starts with (?-i). Results are in reading order.
pub fn find_matches(rec: &Recognised, query: &str, mode: MatchMode) -> Result<Vec<TextMatch>, String> {
    if query.trim().is_empty() {
        return Err("text must not be empty".into());
    }
    let re = if mode == MatchMode::Regex {
        let pat = if query.starts_with("(?") { query.to_string() } else { format!("(?i){query}") };
        Some(regex::Regex::new(&pat).map_err(|e| format!("bad regex {query:?}: {e}"))?)
    } else {
        None
    };
    let q = collapse(&query.to_lowercase());
    let mut out = Vec::new();
    for line in &rec.lines {
        // the line text, word by word, lower-cased for the non-regex modes; remember each word's byte range
        let mut hay = String::new();
        let orig = line.text();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for (i, w) in line.words.iter().enumerate() {
            if i > 0 {
                hay.push(' ');
            }
            let start = hay.len();
            if re.is_some() {
                hay.push_str(&w.text);
            } else {
                hay.push_str(&w.text.to_lowercase());
            }
            ranges.push((start, hay.len()));
        }
        let found: Vec<(usize, usize)> = match &re {
            Some(re) => re.find_iter(&hay).filter(|m| !m.as_str().is_empty()).map(|m| (m.start(), m.end())).collect(),
            None => hay
                .match_indices(&q)
                .map(|(s, m)| (s, s + m.len()))
                .filter(|&(s, e)| mode == MatchMode::Contains || (boundary_before(&hay, s) && boundary_after(&hay, e)))
                .collect(),
        };
        for (s, e) in found {
            let whole = boundary_before(&hay, s) && boundary_after(&hay, e);
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for (i, &(ws, we)) in ranges.iter().enumerate() {
                if ws >= e || we <= s {
                    continue;
                }
                let w = &line.words[i];
                // sub-word precision: the matched part of a word, by character position
                let total = hay[ws..we].chars().count().max(1) as f32;
                let before = hay[ws..s.max(ws)].chars().count() as f32;
                let upto = hay[ws..e.min(we)].chars().count() as f32;
                let (l, r) = (w.x + w.w * before / total, w.x + w.w * upto / total);
                x0 = x0.min(l);
                x1 = x1.max(r);
                y0 = y0.min(w.y);
                y1 = y1.max(w.y + w.h);
            }
            if x0 <= x1 {
                let text = if orig.len() == hay.len() { orig.get(s..e).unwrap_or(&hay[s..e]).to_string() } else { hay[s..e].to_string() };
                out.push(TextMatch { text, line: line.text(), x: x0, y: y0, w: x1 - x0, h: y1 - y0, exact: whole });
            }
        }
    }
    reading_order(&mut out);
    Ok(out)
}

fn boundary_before(s: &str, i: usize) -> bool {
    i == 0 || s[..i].ends_with(' ')
}
fn boundary_after(s: &str, i: usize) -> bool {
    i >= s.len() || s[i..].starts_with(' ')
}

/// Top to bottom, left to right within a row (rows by vertical position, not exact y).
pub fn reading_order(m: &mut Vec<TextMatch>) {
    m.sort_by(|a, b| (a.y + a.h / 2.0).partial_cmp(&(b.y + b.h / 2.0)).unwrap_or(std::cmp::Ordering::Equal));
    let mut rows: Vec<Vec<TextMatch>> = Vec::new();
    let mut row_y = f32::MIN;
    for t in m.drain(..) {
        let yc = t.y + t.h / 2.0;
        if rows.is_empty() || yc - row_y > 0.6 * t.h.max(8.0) {
            rows.push(Vec::new());
            row_y = yc;
        }
        rows.last_mut().unwrap().push(t);
    }
    for r in rows.iter_mut() {
        r.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    }
    *m = rows.into_iter().flatten().collect();
}

/// The first few recognised lines, to show what OCR did read when nothing matched.
pub fn sample_lines(rec: &Recognised, n: usize) -> String {
    let mut s: Vec<String> = rec.lines.iter().take(n).map(|l| format!("{:?}", l.text().chars().take(70).collect::<String>())).collect();
    if rec.lines.len() > n {
        s.push(format!("... {} more lines", rec.lines.len() - n));
    }
    s.join(", ")
}

/// Fraction (0..1) of sampled pixels that differ between two captures of equal size; 1 when the sizes differ.
pub fn diff_fraction(a: &RgbaImage, b: &RgbaImage) -> f64 {
    if a.dimensions() != b.dimensions() {
        return 1.0;
    }
    let (ra, rb) = (a.as_raw(), b.as_raw());
    let mut diff = 0usize;
    let mut total = 0usize;
    for i in (0..ra.len()).step_by(4 * 3) {
        total += 1;
        if (ra[i] as i32 - rb[i] as i32).abs() > 12 || (ra[i + 1] as i32 - rb[i + 1] as i32).abs() > 12 || (ra[i + 2] as i32 - rb[i + 2] as i32).abs() > 12 {
            diff += 1;
        }
    }
    diff as f64 / total.max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(lines: &[&[(&str, f32)]]) -> Recognised {
        let mut ls = Vec::new();
        for (row, words) in lines.iter().enumerate() {
            let mut x = 10.0;
            let mut ws = Vec::new();
            for (t, w) in words.iter() {
                ws.push(Word { text: t.to_string(), x, y: 20.0 + row as f32 * 30.0, w: *w, h: 12.0 });
                x += w + 6.0;
            }
            ls.push(Line { words: ws });
        }
        Recognised { lines: ls, lang: "en-US".into(), ms: 0, width: 800, height: 600 }
    }

    #[test]
    fn phrase_spans_words_and_box_is_union() {
        let r = rec(&[&[("Save", 40.0), ("As", 20.0), ("Cancel", 50.0)]]);
        let m = find_matches(&r, "save as", MatchMode::Contains).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].x, 10.0);
        assert_eq!(m[0].w, 40.0 + 6.0 + 20.0);
        assert_eq!(m[0].text, "Save As");
    }

    #[test]
    fn exact_needs_whole_words() {
        let r = rec(&[&[("BOOK", 40.0), ("OK", 20.0)]]);
        assert_eq!(find_matches(&r, "ok", MatchMode::Contains).unwrap().len(), 2);
        let m = find_matches(&r, "ok", MatchMode::Exact).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].x, 10.0 + 40.0 + 6.0);
    }

    #[test]
    fn regex_and_order() {
        let r = rec(&[&[("Item", 30.0), ("12", 10.0)], &[("Item", 30.0), ("7", 10.0)]]);
        let m = find_matches(&r, r"item \d+", MatchMode::Regex).unwrap();
        assert_eq!(m.len(), 2);
        assert!(m[0].y < m[1].y);
        assert!(find_matches(&r, "(", MatchMode::Regex).is_err());
    }
}
