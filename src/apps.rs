//! App catalogue (the shell's app list plus UserAssist usage counts): the shell's AppsFolder lists every
//! app Windows itself knows, Store apps included; `shell:AppsFolder\<id>` launches one.

use crate::sync::Mutex;
use std::time::Instant;

use windows::core::{PCWSTR, HSTRING};
use windows::Win32::UI::Shell::{ShellExecuteW, SHCreateItemFromParsingName, IShellItem, IEnumShellItems, BHID_EnumItems, SIGDN_NORMALDISPLAY, SIGDN_DESKTOPABSOLUTEPARSING};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

static CACHE: Mutex<Option<(Instant, Vec<(String, String)>)>> = Mutex::new(None);

/// (display name, launch id) of every installed app, sorted by name.
pub fn installed_apps() -> Vec<(String, String)> {
    if let Some((t, apps)) = CACHE.lock().unwrap().as_ref() {
        if t.elapsed().as_secs() < 120 {
            return apps.clone();
        }
    }
    let mut apps = Vec::new();
    unsafe {
        let folder: Result<IShellItem, _> = SHCreateItemFromParsingName(PCWSTR(HSTRING::from("shell:AppsFolder").as_ptr()), None);
        if let Ok(folder) = folder {
            if let Ok(e) = folder.BindToHandler::<_, IEnumShellItems>(None::<&windows::Win32::System::Com::IBindCtx>, &BHID_EnumItems) {
                loop {
                    let mut items = [None];
                    let mut got = 0u32;
                    if e.Next(&mut items, Some(&mut got)).is_err() || got == 0 {
                        break;
                    }
                    if let Some(item) = items[0].take() {
                        let name = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok().map(|p| p.to_string().unwrap_or_default());
                        let id = item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING).ok().map(|p| p.to_string().unwrap_or_default());
                        if let (Some(n), Some(i)) = (name, id) {
                            if !n.is_empty() && !i.is_empty() {
                                apps.push((n, i));
                            }
                        }
                    }
                }
            }
        }
    }
    apps.sort_by_key(|a| a.0.to_lowercase());
    apps.dedup();
    *CACHE.lock().unwrap() = Some((Instant::now(), apps.clone()));
    apps
}

/// The installed app best matching a name: exact, then prefix, then substring; shortest wins.
pub fn find_app(name: &str) -> Option<(String, String)> {
    let want = name.trim().to_lowercase();
    let apps = installed_apps();
    for pred in [
        Box::new(|n: &str| n == want) as Box<dyn Fn(&str) -> bool>,
        Box::new(|n: &str| n.starts_with(&want)),
        Box::new(|n: &str| n.contains(&want)),
    ] {
        if let Some(best) = apps.iter().filter(|(n, _)| pred(&n.to_lowercase())).min_by_key(|(n, _)| n.len()) {
            return Some(best.clone());
        }
    }
    None
}

pub fn start_file(target: &str) -> Result<(), String> {
    let r = unsafe { ShellExecuteW(None, PCWSTR(HSTRING::from("open").as_ptr()), PCWSTR(HSTRING::from(target).as_ptr()), None, None, SW_SHOWNORMAL) };
    if (r.0 as isize) <= 32 {
        return Err(format!("Windows failed to launch {target:?} (ShellExecute code {})", r.0 as isize));
    }
    Ok(())
}

/// Launch count and last-used date per app from Explorer's UserAssist counters, keyed by the
/// lower-case launch id or path.
pub fn usage() -> std::collections::HashMap<String, (u32, String)> {
    use windows::Win32::System::Registry::{RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ};
    let mut out = std::collections::HashMap::new();
    unsafe {
        let mut root = HKEY::default();
        let path = HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist");
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), None, KEY_READ, &mut root).is_err() {
            return out;
        }
        for i in 0..64u32 {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(root, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            let sub = HSTRING::from(format!("{}\\Count", String::from_utf16_lossy(&name[..len as usize])));
            let mut counts = HKEY::default();
            if RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), None, KEY_READ, &mut counts).is_err() {
                continue;
            }
            for j in 0..4096u32 {
                let mut vname = [0u16; 1024];
                let mut vlen = vname.len() as u32;
                let mut data = [0u8; 1024];
                let mut dlen = data.len() as u32;
                if RegEnumValueW(counts, j, Some(windows::core::PWSTR(vname.as_mut_ptr())), &mut vlen, None, None, Some(data.as_mut_ptr()), Some(&mut dlen)).is_err() {
                    break;
                }
                if dlen < 68 {
                    continue;
                }
                let count = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                let ft = u64::from_le_bytes(data[60..68].try_into().unwrap());
                let when = if ft == 0 { String::new() } else { filetime_date(ft) };
                let decoded: String = String::from_utf16_lossy(&vname[..vlen as usize]).chars().map(rot13).collect();
                out.insert(decoded.to_lowercase(), (count, when));
            }
            let _ = RegCloseKey(counts);
        }
        let _ = RegCloseKey(root);
    }
    out
}

fn rot13(c: char) -> char {
    match c {
        'a'..='m' | 'A'..='M' => ((c as u8) + 13) as char,
        'n'..='z' | 'N'..='Z' => ((c as u8) - 13) as char,
        _ => c,
    }
}

fn filetime_date(ft: u64) -> String {
    // FILETIME: 100 ns intervals since 1601-01-01; convert to days since epoch then to y-m-d.
    let secs = ft / 10_000_000;
    let days = (secs / 86400) as i64 - 134774; // days between 1601-01-01 and 1970-01-01
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------- what open_app may launch ----------

/// Files that run code or open a script host or an installer: never started from a name the model
/// passes (it may have read the name on a web page or in a file).
const REFUSED_EXTENSIONS: &[&str] = &[
    "lnk", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta", "msi", "msp", "msc", "reg", "scr", "com", "pif",
    "cpl", "jar", "url", "appref-ms", "sct", "gadget", "chm", "dll", "sys",
];

const ASK_USER: &str = "ask the user to open it themselves (or to confirm exactly what to open); do not retry with another spelling";

/// What a name given to open_app turned out to be.
#[derive(Debug, PartialEq)]
pub enum Launch {
    /// an installed app from the shell's app list: (display name, launch id)
    Installed(String, String),
    /// a program file under an allowed folder (canonical path)
    Exe(String),
}

/// The longest app name or path `open_app` looks at (Windows' MAX_PATH).
pub const MAX_NAME_CHARS: usize = 260;

fn lower_stem(exe: &str) -> String {
    exe.trim_end_matches(".exe").to_string()
}

/// A name that is a URL, a UNC path, a script, an installer, or a path into a download or temp
/// folder is refused without looking at the disk.
pub fn refusal(name: &str) -> Option<String> {
    let t = name.trim();
    if t.is_empty() {
        return Some("name is required".into());
    }
    if t.chars().any(|c| c.is_control()) {
        return Some(format!("{t:?} contains control characters; {ASK_USER}"));
    }
    if t.starts_with("\\\\") || t.starts_with("//") {
        return Some(format!("refusing {t:?}: a network (UNC) path; {ASK_USER}"));
    }
    if t.contains("://") {
        return Some(format!("refusing {t:?}: a URL is not an app; {ASK_USER}"));
    }
    if let Some(i) = t.find(':') {
        // "c:\..." is a drive; anything longer before the colon is a URI scheme (http:, file:, ms-settings:, shell: ...)
        if i >= 2 && t[..i].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
            return Some(format!("refusing {t:?}: a {}: address is not an app; {ASK_USER}", &t[..i]));
        }
    }
    let low = t.to_lowercase().replace('/', "\\");
    let last = low.rsplit('\\').next().unwrap_or("");
    if let Some((_, ext)) = last.rsplit_once('.') {
        if REFUSED_EXTENSIONS.contains(&ext) {
            return Some(format!("refusing {t:?}: .{ext} files are scripts, shortcuts or installers; {ASK_USER}"));
        }
    }
    if folder_denied(&low) {
        return Some(format!("refusing {t:?}: programs in a Downloads or Temp folder are not started; {ASK_USER}"));
    }
    None
}

/// A path (lower-case, backslashes) with a Downloads, Temp or Tmp folder in it.
fn folder_denied(low: &str) -> bool {
    low.split('\\').any(|part| matches!(part, "downloads" | "temp" | "tmp"))
}

fn env_dir(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|s| !s.is_empty()).map(|s| s.trim_end_matches('\\').to_lowercase())
}

/// Folders a program may be started from by path.
fn allowed_roots() -> Vec<String> {
    let mut roots = Vec::new();
    for v in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        roots.extend(env_dir(v));
    }
    if let Some(l) = env_dir("LOCALAPPDATA") {
        roots.push(format!("{l}\\programs"));
    }
    if let Some(w) = env_dir("SystemRoot") {
        roots.push(format!("{w}\\system32"));
        roots.push(format!("{w}\\syswow64"));
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Is this canonical path (any case) under one of the allowed folders and clear of the denied ones?
pub fn path_allowed(path: &str, roots: &[String]) -> bool {
    let low = path.to_lowercase().replace('/', "\\");
    let low = low.strip_prefix("\\\\?\\").unwrap_or(&low).to_string();
    !folder_denied(&low) && roots.iter().any(|r| low.starts_with(&format!("{r}\\")))
}

fn canonical(p: &std::path::Path) -> Option<String> {
    let c = std::fs::canonicalize(p).ok()?;
    let s = c.to_string_lossy().to_string();
    Some(s.strip_prefix("\\\\?\\").unwrap_or(&s).to_string())
}

/// The security policy for a program by its file name: the lock screen, security and password-manager
/// apps, and the allow-list.
fn exe_policy(file: &str, title: &str, allowed: &[String]) -> Result<(), String> {
    crate::guards::check_exe(&file.to_lowercase(), title, allowed)
}

/// An id from the shell's app list that names a forbidden program (a Store app has no file name in its id).
fn id_forbidden(id: &str) -> Option<&'static str> {
    let low = id.to_lowercase();
    crate::guards::FORBIDDEN.iter().chain(crate::guards::LOCKED.iter()).find(|f| {
        let stem = lower_stem(f);
        stem.len() >= 6 && low.contains(&stem)
    }).copied()
}

/// The checks a program path must pass AFTER it has been resolved (links, "..", relative parts): the name rules
/// again on the real path (a link called x.exe can lead to a script or into Downloads), it must still be a .exe,
/// lie under an allowed folder, and pass the program policy. Run before every launch by path.
pub fn check_resolved_exe(path: &str, allowed: &[String]) -> Result<(), String> {
    if let Some(why) = refusal(path) {
        return Err(why);
    }
    if !path.to_lowercase().ends_with(".exe") {
        return Err(format!("refusing {path:?}: it resolves to something that is not a .exe program; {ASK_USER}"));
    }
    if !path_allowed(path, &allowed_roots()) {
        return Err(format!(
            "refusing {path:?}: programs are only started from Program Files, Program Files (x86), %LOCALAPPDATA%\\Programs or Windows\\System32; {ASK_USER}"
        ));
    }
    let file = path.rsplit('\\').next().unwrap_or("").to_string();
    exe_policy(&file, path, allowed)
}

/// Decide what open_app may do with a name. Nothing is launched here.
///  - an installed app's name (or its id) from Windows' own list opens through the shell;
///  - a path opens only when it is a .exe under Program Files, Program Files (x86),
///    %LOCALAPPDATA%\Programs or Windows\System32 (WindowsApps is inside Program Files), after resolving
///    "..", links and relative parts, and not in a Downloads or Temp folder;
///  - URLs, UNC paths, scripts, shortcuts and installers are refused with a pointer to the user;
///  - any other text is an error that names the closest installed apps: nothing is ever typed into Start or
///    searched for, because text typed there can run a command line.
pub fn classify(name: &str, allowed: &[String]) -> Result<Launch, String> {
    let name = name.trim();
    // before any matching (the closest-name search compares against every installed app): no program name or path is this long
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("refusing a name of {} characters: an app name or path is at most {MAX_NAME_CHARS} characters; {ASK_USER}", name.chars().count()));
    }
    if let Some(why) = refusal(name) {
        return Err(why);
    }
    let low = name.to_lowercase();
    let looks_like_path = low.ends_with(".exe") || name.contains('\\') || name.contains('/');
    if looks_like_path {
        if !low.ends_with(".exe") {
            return Err(format!("refusing {name:?}: only .exe programs can be started by path; {ASK_USER}"));
        }
        let has_dir = name.contains('\\') || name.contains('/');
        let found = if has_dir {
            let p = std::path::Path::new(name);
            if !p.is_absolute() {
                return Err(format!("{name:?} is a relative path; pass the full path of the program, or the app's name"));
            }
            canonical(p)
        } else {
            let mut hit = None;
            for v in ["SystemRoot"] {
                if let Some(w) = env_dir(v) {
                    for d in [format!("{w}\\system32"), format!("{w}\\syswow64"), w.clone()] {
                        if hit.is_none() {
                            hit = canonical(std::path::Path::new(&format!("{d}\\{name}")));
                        }
                    }
                }
            }
            hit.or_else(|| find_app(&lower_stem(&low)).and_then(|(_, id)| {
                // an installed app by its program name: a desktop app's id is a path to the program
                let p = std::path::Path::new(&id);
                if p.is_absolute() { canonical(p) } else { None }
            }))
        };
        let Some(path) = found else {
            return Err(format!("no program {name:?} was found; pass its full path under Program Files, or the app's name"));
        };
        check_resolved_exe(&path, allowed)?;
        return Ok(Launch::Exe(path));
    }
    if let Some((display, id)) = find_app(name) {
        if let Some(f) = id_forbidden(&id).or_else(|| id_forbidden(&display)) {
            return Err(format!("refusing to open {display:?}: it is {f}, a security or password-manager app"));
        }
        if let Some(why) = refusal(&id.replace('!', "_")) {
            return Err(why);
        }
        // a desktop app's id that is a path: the real file it leads to gets the same name checks as a path the model passed
        if std::path::Path::new(&id).is_absolute() {
            let Some(real) = canonical(std::path::Path::new(&id)) else {
                return Err(format!("{display:?} points to {id:?}, which does not resolve to a file; {ASK_USER}"));
            };
            if let Some(why) = refusal(&real) {
                return Err(why);
            }
            if !real.to_lowercase().ends_with(".exe") {
                return Err(format!("refusing {display:?}: its launch target {real:?} is not a .exe program; {ASK_USER}"));
            }
            exe_policy(real.rsplit('\\').next().unwrap_or(""), &real, allowed)?;
        }
        let tail = id.rsplit('\\').next().unwrap_or("").to_lowercase();
        if tail.ends_with(".exe") {
            exe_policy(&tail, &display, allowed)?;
        } else if !allowed.is_empty() {
            // a Store app's program is not in its id: it must at least look like one of the allowed ones; the window
            // that opens is checked against the real program afterwards (actions::open_app)
            let hay = format!("{display} {id}").to_lowercase().replace(' ', "");
            if !allowed.iter().any(|a| lower_stem(a).len() >= 3 && hay.contains(&lower_stem(a))) {
                return Err(format!("{display:?} is a Store app that does not look like any program in config.json apps.allowed {allowed:?}; ask the user to allow it"));
            }
        }
        return Ok(Launch::Installed(display, id));
    }
    // not an installed app: nothing is launched, nothing is typed anywhere
    if let Some(f) = id_forbidden(name).or_else(|| crate::guards::FORBIDDEN.iter().find(|f| lower_stem(f) == low.trim_end_matches(".exe")).copied()) {
        return Err(format!("refusing to open {name:?}: it is {f}, a security or password-manager app"));
    }
    Err(not_installed(name, allowed))
}

/// The error for a name that is no installed app: the closest installed names (never a forbidden app's) and the instruction to ask.
fn not_installed(name: &str, allowed: &[String]) -> String {
    let names: Vec<String> = installed_apps().into_iter().filter(|(n, id)| id_forbidden(id).or_else(|| id_forbidden(n)).is_none()).map(|(n, _)| n).collect();
    let close = closest_names(name, &names, 5);
    let list = if close.is_empty() || !allowed.is_empty() { String::new() } else { format!(" The closest installed apps: {}.", close.join(", ")) };
    let limit = if allowed.is_empty() { String::new() } else { format!(" (and with config.json apps.allowed set only {allowed:?} may be opened)") };
    format!("{name:?} is not an installed app{limit}, so nothing was opened and nothing was typed into Start or any search box.{list} Ask the user which app they mean, or to open it themselves; do not retry with another spelling")
}

/// Edit distance between two strings (characters), two-row version.
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Up to `n` of `names` that look most like `want`: names that share a word of 3+ letters with it first, then by edit distance.
fn closest_names(want: &str, names: &[String], n: usize) -> Vec<String> {
    let want = want.trim().to_lowercase();
    let words: Vec<&str> = want.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).collect();
    let mut scored: Vec<(bool, usize, &String)> = names
        .iter()
        .map(|nm| {
            let low = nm.to_lowercase();
            let shares = words.iter().any(|w| low.contains(w));
            (!shares, levenshtein(&want, &low), nm)
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)));
    scored.into_iter().take(n).map(|(_, _, nm)| nm.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_scripts_unc_and_download_folders_are_refused() {
        for bad in [
            "http://example.com/a.exe", "https://x.y", "file:///c:/windows/notepad.exe", "ms-settings:privacy", "shell:startup", "javascript:alert(1)",
            "\\\\server\\share\\a.exe", "//server/share/a.exe", "c:\\x\\a.bat", "C:\\x\\run.CMD", "c:\\x\\a.ps1", "d:\\a.vbs", "a.js", "x.hta", "setup.msi",
            "my.reg", "a.scr", "a.com", "link.lnk", "c:\\Users\\me\\Downloads\\tool.exe", "c:\\Users\\me\\AppData\\Local\\Temp\\x\\a.exe", "ab\u{0}c",
        ] {
            assert!(refusal(bad).is_some(), "{bad:?} should be refused");
        }
        for ok in ["Notepad", "notepad.exe", "Calculator", "c:\\windows\\system32\\calc.exe", "Visual Studio Code", "7-Zip File Manager"] {
            assert!(refusal(ok).is_none(), "{ok:?} should pass the name check: {:?}", refusal(ok));
        }
    }

    #[test]
    fn paths_are_checked_against_the_allowed_folders() {
        let roots = vec!["c:\\program files".to_string(), "c:\\windows\\system32".to_string(), "c:\\users\\me\\appdata\\local\\programs".to_string()];
        assert!(path_allowed("C:\\Program Files\\App\\app.exe", &roots));
        assert!(path_allowed("c:\\windows\\system32\\notepad.exe", &roots));
        assert!(path_allowed("\\\\?\\C:\\Users\\Me\\AppData\\Local\\Programs\\X\\x.exe", &roots));
        assert!(!path_allowed("C:\\Users\\me\\Downloads\\app.exe", &roots));
        assert!(!path_allowed("C:\\Program Files\\Temp\\app.exe", &roots));
        assert!(!path_allowed("C:\\Program Files Evil\\app.exe", &roots));
        assert!(!path_allowed("C:\\Windows\\evil.exe", &roots));
        assert!(!path_allowed("D:\\tools\\a.exe", &roots));
    }

    #[test]
    fn a_resolved_path_gets_the_name_checks_again() {
        let win = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        assert!(check_resolved_exe(&format!("{win}\\System32\\notepad.exe"), &[]).is_ok());
        // a link called x.exe that leads to a script, to a download or to a program that must not be started
        assert!(check_resolved_exe(&format!("{win}\\System32\\run.bat"), &[]).is_err());
        assert!(check_resolved_exe(&format!("{win}\\System32\\run.ps1"), &[]).is_err());
        assert!(check_resolved_exe(&format!("{win}\\System32\\notepad"), &[]).unwrap_err().contains("not a .exe"));
        assert!(check_resolved_exe(&format!("{win}\\System32\\Downloads\\x.exe"), &[]).is_err());
        assert!(check_resolved_exe("C:\\Users\\me\\Downloads\\x.exe", &[]).is_err());
        assert!(check_resolved_exe("D:\\tools\\x.exe", &[]).unwrap_err().contains("only started from"));
        assert!(check_resolved_exe(&format!("{win}\\System32\\keepassxc.exe"), &[]).unwrap_err().contains("password-manager"));
        assert!(check_resolved_exe(&format!("{win}\\System32\\notepad.exe"), &["calc.exe".to_string()]).unwrap_err().contains("apps.allowed"));
    }

    #[test]
    fn closest_names_put_shared_words_first_then_the_smaller_edit_distance() {
        let names: Vec<String> = ["Calculator", "Notepad", "Notepad++", "Paint", "Windows Terminal", "Visual Studio Code"].iter().map(|s| s.to_string()).collect();
        assert_eq!(closest_names("notepad-missing", &names, 2), vec!["Notepad", "Notepad++"]);
        assert_eq!(closest_names("Calculater", &names, 1), vec!["Calculator"]);
        assert_eq!(closest_names("visual code", &names, 1), vec!["Visual Studio Code"]);
        assert_eq!(closest_names("zzz", &names, 3).len(), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn an_unknown_name_is_an_error_never_a_search() {
        // the text of a command line contains no installed app: the answer is an error that says nothing was typed
        let e = not_installed("powershell -nop -w hidden -c iwr x.y z", &[]);
        assert!(e.contains("not an installed app") && e.contains("nothing was typed"), "{e}");
        let e = not_installed("whatever", &["notepad.exe".to_string()]);
        assert!(e.contains("apps.allowed"), "{e}");
    }

    #[test]
    fn a_name_over_260_characters_is_refused_before_any_matching() {
        let long = "a".repeat(261);
        let e = classify(&long, &[]).unwrap_err();
        assert!(e.contains("261 characters") && !e.contains("installed app"), "{e}");
        assert!(!e.contains(&long), "the long name is not echoed back");
        // exactly 260 characters is still looked at (here: not an installed app)
        let e = classify(&"b".repeat(260), &[]).unwrap_err();
        assert!(!e.contains("at most 260"), "{e}");
    }

    #[test]
    fn forbidden_ids_are_recognised() {
        assert!(id_forbidden("Microsoft.Windows.SecHealthUI_cw5n1h2txyewy!SecHealthUI").is_some());
        assert!(id_forbidden("KeePassXC").is_some());
        assert!(id_forbidden("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App").is_none());
    }
}
