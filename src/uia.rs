//! UI Automation: the control tree with states, element clicks,
//! set_value / read_text, secondary actions, name search and focus text polling. Properties are
//! fetched with a cache request so each element costs one cross-process call.

use crate::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{Interface, BSTR};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_I4};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Accessibility::*;

use crate::{guards, input, overlay, win};

struct Uia {
    auto: IUIAutomation,
    /// name, type, rectangle, visibility, enabled, focus: enough to find and click things
    cache: IUIAutomationCacheRequest,
    /// `cache` plus the state properties (value, toggle, expand/collapse, selection, automation
    /// id): one cross-process call per element for a whole listing or snapshot
    ext: IUIAutomationCacheRequest,
    walker: IUIAutomationTreeWalker,
}
unsafe impl Send for Uia {}
unsafe impl Sync for Uia {}

fn uia() -> Result<&'static Uia, String> {
    static U: OnceLock<Result<Uia, String>> = OnceLock::new();
    U.get_or_init(|| unsafe {
        let auto: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("UI Automation: {e}"))?;
        let cache = auto.CreateCacheRequest().map_err(|e| e.to_string())?;
        let ext = auto.CreateCacheRequest().map_err(|e| e.to_string())?;
        for id in [UIA_NamePropertyId, UIA_ControlTypePropertyId, UIA_BoundingRectanglePropertyId, UIA_IsOffscreenPropertyId,
                   UIA_IsEnabledPropertyId, UIA_HasKeyboardFocusPropertyId, UIA_ProcessIdPropertyId] {
            let _ = cache.AddProperty(id);
            let _ = ext.AddProperty(id);
        }
        for id in [UIA_AutomationIdPropertyId, UIA_ValueValuePropertyId, UIA_ToggleToggleStatePropertyId,
                   UIA_ExpandCollapseExpandCollapseStatePropertyId, UIA_SelectionItemIsSelectedPropertyId] {
            let _ = ext.AddProperty(id);
        }
        let _ = cache.SetTreeScope(TreeScope_Element);
        let _ = ext.SetTreeScope(TreeScope_Element);
        let walker = auto.ControlViewWalker().map_err(|e| e.to_string())?;
        Ok(Uia { auto, cache, ext, walker })
    })
    .as_ref()
    .map_err(|e| e.clone())
}

#[derive(Clone)]
pub struct Elem {
    pub name: String,
    pub ctype: String,
    pub cx: i32,
    pub cy: i32,
    pub control: IUIAutomationElement,
    /// the window it was found in (the one being read at the time)
    pub hwnd: isize,
}
unsafe impl Send for Elem {}

static ELEMENTS: Mutex<Vec<Elem>> = Mutex::new(Vec::new());

/// Forget the ids handed out by list_elements: they belong to the view they were listed in.
pub fn clear_elements() {
    // the control objects are released on another thread: releasing them can call into the owning app, and
    // callers hold the task lock
    let old = std::mem::take(&mut *ELEMENTS.lock().unwrap());
    if !old.is_empty() {
        let _ = std::thread::Builder::new().name("drop-elements".into()).spawn(move || drop(old));
    }
}

const INTERACTIVE: &[i32] = &[
    UIA_ButtonControlTypeId.0, UIA_SplitButtonControlTypeId.0, UIA_EditControlTypeId.0, UIA_ComboBoxControlTypeId.0,
    UIA_CheckBoxControlTypeId.0, UIA_RadioButtonControlTypeId.0, UIA_MenuItemControlTypeId.0, UIA_TabItemControlTypeId.0,
    UIA_ListItemControlTypeId.0, UIA_TreeItemControlTypeId.0, UIA_HyperlinkControlTypeId.0, UIA_DataItemControlTypeId.0,
    UIA_SliderControlTypeId.0, UIA_SpinnerControlTypeId.0, UIA_DocumentControlTypeId.0,
];

/// Long collections: their items are listed last so they cannot push buttons and fields out.
const COLLECTIONS: &[i32] = &[UIA_ListControlTypeId.0, UIA_TreeControlTypeId.0, UIA_TableControlTypeId.0, UIA_DataGridControlTypeId.0];
const LONG_COLLECTION: usize = 25;

fn type_name(id: i32) -> &'static str {
    match UIA_CONTROLTYPE_ID(id) {
        UIA_ButtonControlTypeId => "Button", UIA_CalendarControlTypeId => "Calendar", UIA_CheckBoxControlTypeId => "CheckBox",
        UIA_ComboBoxControlTypeId => "ComboBox", UIA_EditControlTypeId => "Edit", UIA_HyperlinkControlTypeId => "Hyperlink",
        UIA_ImageControlTypeId => "Image", UIA_ListItemControlTypeId => "ListItem", UIA_ListControlTypeId => "List",
        UIA_MenuControlTypeId => "Menu", UIA_MenuBarControlTypeId => "MenuBar", UIA_MenuItemControlTypeId => "MenuItem",
        UIA_ProgressBarControlTypeId => "ProgressBar", UIA_RadioButtonControlTypeId => "RadioButton",
        UIA_ScrollBarControlTypeId => "ScrollBar", UIA_SliderControlTypeId => "Slider", UIA_SpinnerControlTypeId => "Spinner",
        UIA_StatusBarControlTypeId => "StatusBar", UIA_TabControlTypeId => "Tab", UIA_TabItemControlTypeId => "TabItem",
        UIA_TextControlTypeId => "Text", UIA_ToolBarControlTypeId => "ToolBar", UIA_ToolTipControlTypeId => "ToolTip",
        UIA_TreeControlTypeId => "Tree", UIA_TreeItemControlTypeId => "TreeItem", UIA_CustomControlTypeId => "Custom",
        UIA_GroupControlTypeId => "Group", UIA_ThumbControlTypeId => "Thumb", UIA_DataGridControlTypeId => "DataGrid",
        UIA_DataItemControlTypeId => "DataItem", UIA_DocumentControlTypeId => "Document", UIA_SplitButtonControlTypeId => "SplitButton",
        UIA_WindowControlTypeId => "Window", UIA_PaneControlTypeId => "Pane", UIA_HeaderControlTypeId => "Header",
        UIA_HeaderItemControlTypeId => "HeaderItem", UIA_TableControlTypeId => "Table", UIA_TitleBarControlTypeId => "TitleBar",
        UIA_SeparatorControlTypeId => "Separator", UIA_SemanticZoomControlTypeId => "SemanticZoom", UIA_AppBarControlTypeId => "AppBar",
        _ => "Unknown",
    }
}

fn s(b: Result<BSTR, windows::core::Error>) -> String {
    b.map(|b| b.to_string()).unwrap_or_default()
}

fn cvariant(e: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<VARIANT> {
    unsafe { e.GetCachedPropertyValueEx(id, true).ok() }
}
fn cstr(e: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<String> {
    let v = cvariant(e, id)?;
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        (inner.vt == VT_BSTR).then(|| inner.Anonymous.bstrVal.to_string())
    }
}
fn ci32(e: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<i32> {
    let v = cvariant(e, id)?;
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        (inner.vt == VT_I4).then(|| inner.Anonymous.lVal)
    }
}
fn cbool(e: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<bool> {
    let v = cvariant(e, id)?;
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        (inner.vt == VT_BOOL).then(|| inner.Anonymous.boolVal.0 != 0)
    }
}

/// The window to read: the one a window-scoped screenshot follows, else the foreground one.
fn root_hwnd() -> HWND {
    let v = *win::VIEW.lock().unwrap();
    match v.hwnd.map(win::hwnd_from) {
        Some(h) if win::is_window(h) => {
            let fg = win::foreground();
            if !fg.0.is_null() && fg != h && win::is_work_window(fg) && win::pid_of(fg) == win::pid_of(h) { fg } else { h }
        }
        _ => win::foreground(),
    }
}

/// The root element of the window being read, with its handle. The AI app's own window, security and
/// password-manager apps, the lock screen and apps outside apps.allowed are never read.
fn root() -> Result<(IUIAutomationElement, HWND), String> {
    let u = uia()?;
    let hwnd = root_hwnd();
    if win::is_host(hwnd) {
        return Err("the window in front is the AI app's own window, which is never read; use focus_window or screenshot(window=...) to pick the window to work in".into());
    }
    guards::guard_target(hwnd)?;
    let el = unsafe { u.auto.ElementFromHandleBuildCache(hwnd, &u.ext) }.map_err(|e| format!("UI Automation: {e}"))?;
    Ok((el, hwnd))
}

/// Centre of an on-screen element from its cached rectangle.
fn visible_centre(e: &IUIAutomationElement, cached: bool) -> Option<(i32, i32)> {
    unsafe {
        if cached {
            if e.CachedIsOffscreen().ok()?.as_bool() {
                return None;
            }
        } else if e.CurrentIsOffscreen().ok()?.as_bool() {
            return None;
        }
        let r = if cached { e.CachedBoundingRectangle().ok()? } else { e.CurrentBoundingRectangle().ok()? };
        if r.right - r.left <= 0 || r.bottom - r.top <= 0 {
            return None;
        }
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let mons = win::monitors();
        mons.iter().any(|m| m.rect.contains(cx, cy)).then_some((cx, cy))
    }
}

const LIST_TIME_BUDGET: Duration = Duration::from_millis(2000);

struct Limits {
    time: Duration,
    /// nodes visited in all
    nodes: usize,
    /// nodes visited under one list/tree/table; the rest of it is skipped
    per_collection: usize,
}

struct Raw {
    e: IUIAutomationElement,
    ctype: i32,
    name: String,
    depth: usize,
    /// nearest list/tree/table ancestor (an index into `Crawl::counts`)
    coll: Option<usize>,
}

struct Crawl {
    /// per collection: nodes visited below it
    counts: Vec<usize>,
    visited: usize,
    /// false when the time or node budget ended the walk early
    complete: bool,
}

/// Walk the control view of `top` depth-first with cached properties and hand every visible
/// node that `keep` accepts to `visit` (return false to stop). Bounded by time, node count and,
/// per list/tree/table, the nodes visited below it.
fn crawl(top: &IUIAutomationElement, cache: &IUIAutomationCacheRequest, limits: &Limits, keep: impl Fn(i32, &str) -> bool, mut visit: impl FnMut(Raw) -> bool) -> Result<Crawl, String> {
    let u = uia()?;
    let deadline = Instant::now() + limits.time;
    let mut out = Crawl { counts: Vec::new(), visited: 0, complete: true };
    // elements of a protected program (a password manager's window embedded in this one) are skipped with their subtrees
    let top_pid = unsafe { top.CachedProcessId() }.unwrap_or(0);
    let mut protected_pids: std::collections::HashMap<i32, bool> = Default::default();
    // explicit stack of (element, depth, nearest collection ancestor): first child, then siblings
    let mut stack: Vec<(IUIAutomationElement, usize, Option<usize>)> = Vec::new();
    if let Ok(c) = unsafe { u.walker.GetFirstChildElementBuildCache(top, cache) } {
        stack.push((c, 1, None));
    }
    while let Some((e, depth, coll)) = stack.pop() {
        if Instant::now() > deadline || out.visited >= limits.nodes {
            out.complete = false;
            break;
        }
        if let Some(c) = coll {
            if out.counts[c] >= limits.per_collection {
                out.complete = false; // the rest of this collection is skipped; siblings outside it are still pending
                continue;
            }
            out.counts[c] += 1;
        }
        out.visited += 1;
        let ctype = unsafe { e.CachedControlType() }.map(|t| t.0).unwrap_or(0);
        let blocked = unsafe { e.CachedProcessId() }.map(|p| p != top_pid).unwrap_or(false) && in_protected_process(&e, &mut protected_pids);
        let mut child_coll = coll;
        if COLLECTIONS.contains(&ctype) {
            out.counts.push(0);
            child_coll = Some(out.counts.len() - 1);
        }
        if let Ok(next) = unsafe { u.walker.GetNextSiblingElementBuildCache(&e, cache) } {
            stack.push((next, depth, coll));
        }
        if blocked {
            continue; // not listed, not read, not entered
        }
        if depth < 40 {
            if let Ok(child) = unsafe { u.walker.GetFirstChildElementBuildCache(&e, cache) } {
                stack.push((child, depth + 1, child_coll));
            }
        }
        let name = unsafe { s(e.CachedName()) }.trim().to_string();
        if !keep(ctype, &name) || visible_centre(&e, true).is_none() {
            continue;
        }
        if !visit(Raw { e, ctype, name, depth, coll }) {
            break;
        }
    }
    Ok(out)
}

fn is_interactive(ctype: i32, name: &str) -> bool {
    INTERACTIVE.contains(&ctype) && (!name.is_empty() || matches!(UIA_CONTROLTYPE_ID(ctype), UIA_EditControlTypeId | UIA_ComboBoxControlTypeId | UIA_DocumentControlTypeId))
}

fn pattern<T: Interface>(e: &IUIAutomationElement, id: UIA_PATTERN_ID) -> Option<T> {
    unsafe { e.GetCurrentPattern(id).ok().and_then(|p| p.cast::<T>().ok()) }
}

/// State of an element read from the cached properties of the `ext` request: state tags
/// (disabled, focused, checked, expanded, selected) and the field value.
fn state_cached(e: &IUIAutomationElement, ctype: i32) -> (Vec<String>, Option<String>) {
    let mut tags: Vec<String> = Vec::new();
    unsafe {
        if e.CachedIsEnabled().map(|b| !b.as_bool()).unwrap_or(false) {
            tags.push("disabled".into());
        }
        if e.CachedHasKeyboardFocus().map(|b| b.as_bool()).unwrap_or(false) {
            tags.push("focused".into());
        }
    }
    let t = UIA_CONTROLTYPE_ID(ctype);
    if matches!(t, UIA_CheckBoxControlTypeId | UIA_RadioButtonControlTypeId | UIA_ButtonControlTypeId) {
        if let Some(st) = ci32(e, UIA_ToggleToggleStatePropertyId) {
            tags.push(if st == ToggleState_On.0 { "checked" } else { "unchecked" }.into());
        }
    }
    if matches!(t, UIA_ComboBoxControlTypeId | UIA_TreeItemControlTypeId | UIA_MenuItemControlTypeId | UIA_ButtonControlTypeId | UIA_SplitButtonControlTypeId | UIA_ListItemControlTypeId) {
        match ci32(e, UIA_ExpandCollapseExpandCollapseStatePropertyId) {
            Some(x) if x == ExpandCollapseState_Collapsed.0 => tags.push("collapsed".into()),
            Some(x) if x == ExpandCollapseState_Expanded.0 => tags.push("expanded".into()),
            Some(x) if x == ExpandCollapseState_PartiallyExpanded.0 => tags.push("partially expanded".into()),
            _ => {}
        }
    }
    if matches!(t, UIA_ListItemControlTypeId | UIA_TabItemControlTypeId | UIA_TreeItemControlTypeId | UIA_RadioButtonControlTypeId | UIA_DataItemControlTypeId)
        && cbool(e, UIA_SelectionItemIsSelectedPropertyId) == Some(true)
    {
        tags.push("selected".into());
    }
    let mut value = None;
    if matches!(t, UIA_EditControlTypeId | UIA_ComboBoxControlTypeId | UIA_DocumentControlTypeId) {
        value = cstr(e, UIA_ValueValuePropertyId).filter(|v| !v.is_empty());
    }
    (tags, value)
}

/// Does this element belong to a process whose contents are never read (the lock screen, a security or password-manager
/// app, a program that can't be identified)? An element's own process (cached) is what counts: a control can be a window of another program embedded in the
/// window being read. `memo` remembers the answer per process id.
fn in_protected_process(e: &IUIAutomationElement, memo: &mut std::collections::HashMap<i32, bool>) -> bool {
    let pid = unsafe { e.CachedProcessId() }.unwrap_or(0);
    // a process whose program can't be identified counts as protected (an unreadable program is never read, as for masking)
    pid > 0
        && *memo.entry(pid).or_insert_with(|| {
            let exe = win::exe_name(pid as u32);
            exe.is_empty() || guards::is_protected_exe(&exe)
        })
}

fn focused() -> Option<IUIAutomationElement> {
    let fg = win::foreground();
    if win::is_host(fg) || guards::guard_target(fg).is_err() {
        return None;
    }
    let u = uia().ok()?;
    let f = unsafe { u.auto.GetFocusedElementBuildCache(&u.cache) }.ok()?;
    // the focus can sit in a protected program's window embedded in the one in front
    if in_protected_process(&f, &mut Default::default()) {
        return None;
    }
    Some(f)
}

fn selected_text(e: &IUIAutomationElement, max: i32) -> String {
    let Some(tp) = pattern::<IUIAutomationTextPattern>(e, UIA_TextPatternId) else { return String::new() };
    unsafe {
        let Ok(sel) = tp.GetSelection() else { return String::new() };
        let n = sel.Length().unwrap_or(0);
        (0..n).filter_map(|i| sel.GetElement(i).ok()).map(|r| s(r.GetText(max))).collect::<Vec<_>>().join(" | ")
    }
}

/// Is the focused control a password field? Its text must not travel through the clipboard.
pub fn focused_is_password() -> bool {
    let Some(f) = focused() else { return false };
    unsafe { f.CurrentIsPassword().map(|b| b.as_bool()).unwrap_or(false) }
}

/// The focused element and selected text, as header lines.
fn focus_summary() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(f) = focused() {
        unsafe {
            let t = f.CachedControlType().map(|t| type_name(t.0)).unwrap_or("Unknown");
            out.push(format!("Focused: [{t}] {:?}", s(f.CachedName()).chars().take(60).collect::<String>()));
        }
        let sel = selected_text(&f, 200);
        if !sel.trim().is_empty() {
            out.push(format!("Selected text: {:?}", sel.chars().take(200).collect::<String>()));
        }
    }
    out
}

/// One line for the text observation: the focused control with a truncated value. Nothing for
/// the AI app's own windows or forbidden apps.
pub fn focus_line() -> Option<String> {
    let u = uia().ok()?;
    let f = unsafe { u.auto.GetFocusedElementBuildCache(&u.ext) }.ok()?;
    let ctype = unsafe { f.CachedControlType() }.map(|t| t.0).unwrap_or(0);
    let fg = win::foreground();
    if win::is_host(fg) || guards::guard_target(fg).is_err() || in_protected_process(&f, &mut Default::default()) {
        return None;
    }
    let name = unsafe { s(f.CachedName()) };
    let (tags, value) = state_cached(&f, ctype);
    let mut line = format!("Focused: [{}] {:?}", type_name(ctype), name.chars().take(50).collect::<String>());
    if let Some(v) = value {
        line += &format!(" = {}", preview(&v, 80));
    }
    let extra: Vec<&str> = tags.iter().map(String::as_str).filter(|t| *t != "focused").collect();
    if !extra.is_empty() {
        line += &format!(" [{}]", extra.join(", "));
    }
    Some(line)
}

/// Quoted text, long values shortened to head and tail (typing appends at the end).
fn preview(v: &str, max: usize) -> String {
    let v = v.replace(['\r', '\n'], "\u{23ce}");
    let n = v.chars().count();
    if n <= max {
        return format!("{v:?}");
    }
    let head: String = v.chars().take(max / 3).collect();
    let tail: String = v.chars().skip(n - (max * 2 / 3)).collect();
    format!("{:?} ({n} chars)", format!("{head}\u{2026}{tail}"))
}

pub struct Listing {
    pub text: String,
    pub data: serde_json::Value,
}

pub fn list_elements(max_items: usize) -> Result<Listing, String> {
    let max_items = max_items.max(1);
    let (top, top_hwnd) = root()?;
    let title = unsafe { s(top.CachedName()) };
    let mut lines = vec![format!("Window: {}", if title.is_empty() { "(no title)".into() } else { title.clone() })];
    let focus = focus_summary();
    lines.extend(focus.iter().cloned());
    let v = *win::VIEW.lock().unwrap();
    let mons = win::monitors();
    struct Cand {
        raw: Raw,
        class: u8,
        cx: i32,
        cy: i32,
    }
    let mut cands: Vec<Cand> = Vec::new();
    let stats = crawl(
        &top,
        &uia()?.ext,
        &Limits { time: LIST_TIME_BUDGET, nodes: 4000, per_collection: max_items.max(60) },
        |ctype, name| is_interactive(ctype, name) || (ctype == UIA_TextControlTypeId.0 && !name.is_empty()),
        |raw| {
            if let Some((cx, cy)) = visible_centre(&raw.e, true) {
                let class = if is_interactive(raw.ctype, &raw.name) { 0 } else { 1 };
                cands.push(Cand { raw, class, cx, cy });
            }
            true
        },
    )?;
    for c in cands.iter_mut() {
        if matches!(c.raw.coll, Some(k) if stats.counts[k] > LONG_COLLECTION) {
            c.class = 2; // long lists, trees, tables last
        }
    }
    // Interactive controls first, then other content, then long collections; tree order inside the budget.
    let total = cands.len();
    let mut picked: Vec<usize> = (0..total).collect();
    picked.sort_by_key(|&i| (cands[i].class, i));
    let mut hidden = [0usize; 3];
    for &i in picked.iter().skip(max_items) {
        hidden[cands[i].class as usize] += 1;
    }
    picked.truncate(max_items);
    picked.sort_unstable();
    let mut elems = Vec::new();
    let mut data = Vec::new();
    for &ci in &picked {
        let c = &cands[ci];
        let i = elems.len() + 1;
        let tn = type_name(c.raw.ctype);
        let (tags, value) = state_cached(&c.raw.e, c.raw.ctype);
        let mut shown_tags = tags.clone();
        if let Some(val) = &value {
            shown_tags.push(format!("= {:?}", val.chars().take(40).collect::<String>()));
        }
        let st = if shown_tags.is_empty() { String::new() } else { format!(" [{}]", shown_tags.join(", ")) };
        let (x, y, mon) = if v.rect.contains(c.cx, c.cy) {
            (Some(((c.cx - v.rect.left) as f64 / v.scale).round() as i32), Some(((c.cy - v.rect.top) as f64 / v.scale).round() as i32), None)
        } else {
            (None, None, Some(win::monitor_at(c.cx, c.cy, &mons) + 1))
        };
        let where_ = match (x, y, mon) {
            (Some(x), Some(y), _) => format!("({x}, {y})"),
            (_, _, Some(m)) => format!("on monitor {m}"),
            _ => String::new(),
        };
        let shown_name: String = if c.class == 1 && c.raw.name.chars().count() > 100 { format!("{}\u{2026}", c.raw.name.chars().take(100).collect::<String>()) } else { c.raw.name.clone() };
        lines.push(format!("{i}. {}[{tn}] {shown_name:?}{st} {where_}", "  ".repeat(c.raw.depth.saturating_sub(1).min(6))));
        let mut d = serde_json::json!({"id": i, "type": tn, "name": c.raw.name, "states": tags});
        if let Some(val) = &value {
            d["value"] = serde_json::json!(val.chars().take(200).collect::<String>());
        }
        if let (Some(x), Some(y)) = (x, y) {
            d["x"] = serde_json::json!(x);
            d["y"] = serde_json::json!(y);
        }
        if let Some(m) = mon {
            d["monitor"] = serde_json::json!(m);
        }
        data.push(d);
        elems.push(Elem { name: if c.raw.name.is_empty() { tn.to_string() } else { c.raw.name.clone() }, ctype: tn.into(), cx: c.cx, cy: c.cy, control: c.raw.e.clone(), hwnd: top_hwnd.0 as isize });
    }
    if elems.is_empty() {
        lines.push("(no accessible elements found; fall back to screenshot + click)".into());
    }
    let hidden_total: usize = hidden.iter().sum();
    if hidden_total > 0 || !stats.complete {
        let mut parts = Vec::new();
        if hidden[0] > 0 {
            parts.push(format!("{} controls", hidden[0]));
        }
        if hidden[1] > 0 {
            parts.push(format!("{} text items", hidden[1]));
        }
        if hidden[2] > 0 {
            parts.push(format!("{} items of long lists/trees/tables", hidden[2]));
        }
        let mut note = format!("Showing {} of {} elements, interactive controls first (max_items={max_items}).", elems.len(), total);
        if !parts.is_empty() {
            note += &format!(" Not shown: {}.", parts.join(", "));
        }
        if !stats.complete {
            note += " The walk stopped early at its time/node budget, so the window may hold more (long lists are only sampled).";
        }
        note += &format!(" For more call list_elements(max_items={}); or reach one control by name with click_element / read_text.", (max_items * 2).max(600));
        lines.push(note);
    }
    *ELEMENTS.lock().unwrap() = elems;
    let data = serde_json::json!({
        "window": title,
        "focused": focus.first().cloned().unwrap_or_default(),
        "elements": data,
        "shown": picked.len(),
        "total": total,
        "truncated": hidden_total > 0 || !stats.complete,
    });
    Ok(Listing { text: lines.join("\n"), data })
}

// ---------- snapshots for the text observation ----------

#[derive(Clone)]
struct SVal {
    len: usize,
    hash: u64,
    text: String,
}

#[derive(Clone)]
struct SNode {
    key: String,
    ctype: i32,
    name: String,
    tags: String,
    value: Option<SVal>,
}

/// Interactive, visible controls of the work window: cheap enough to take before and after
/// every action.
pub struct Snap {
    hwnd: isize,
    title: String,
    nodes: Vec<SNode>,
    complete: bool,
}

const SNAP_TIME: Duration = Duration::from_millis(220);
const SNAP_NODES: usize = 500;
const SNAP_PER_COLLECTION: usize = 40;

fn fnv(s: &str) -> u64 {
    let mut h = 1469598103934665603u64;
    for b in s.bytes() {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    h
}

/// None when there is no suitable window (the AI app's own, a forbidden app, nothing) or UI
/// Automation fails.
pub fn snapshot() -> Option<Snap> {
    let hwnd = {
        let h = root_hwnd();
        if !h.0.is_null() && win::is_work_window(h) { h } else { win::work_window()? }
    };
    if guards::guard_target(hwnd).is_err() {
        return None;
    }
    let u = uia().ok()?;
    let top = unsafe { u.auto.ElementFromHandleBuildCache(hwnd, &u.ext) }.ok()?;
    let title = unsafe { s(top.CachedName()) };
    let mut nodes: Vec<SNode> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    let stats = crawl(
        &top,
        &u.ext,
        &Limits { time: SNAP_TIME, nodes: SNAP_NODES, per_collection: SNAP_PER_COLLECTION },
        is_interactive,
        |raw| {
            let (tags, value) = state_cached(&raw.e, raw.ctype);
            let valued = matches!(UIA_CONTROLTYPE_ID(raw.ctype), UIA_EditControlTypeId | UIA_ComboBoxControlTypeId | UIA_DocumentControlTypeId);
            let ident = cstr(&raw.e, UIA_AutomationIdPropertyId).filter(|a| !a.is_empty()).unwrap_or_else(|| if valued { String::new() } else { raw.name.clone() });
            let base = format!("{}|{ident}", raw.ctype);
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            let key = format!("{base}#{n}");
            let tags: Vec<String> = tags.into_iter().filter(|t| t != "focused").collect();
            nodes.push(SNode {
                key,
                ctype: raw.ctype,
                name: raw.name,
                tags: tags.join(", "),
                value: value.map(|v| SVal { len: v.chars().count(), hash: fnv(&v), text: v }),
            });
            true
        },
    )
    .ok()?;
    if std::env::var_os("DC_TRACE").is_some() {
        eprintln!("snapshot: visited {} nodes, kept {}, complete {}", stats.visited, nodes.len(), stats.complete);
    }
    Some(Snap { hwnd: hwnd.0 as isize, title, nodes, complete: stats.complete })
}

fn describe_node(n: &SNode) -> String {
    let mut s = format!("[{}] {:?}", type_name(n.ctype), n.name.chars().take(60).collect::<String>());
    if !n.tags.is_empty() {
        s += &format!(" [{}]", n.tags);
    }
    if let Some(v) = &n.value {
        s += &format!(" = {}", preview(&v.text, 60));
    }
    s
}

/// A compact before/after diff: controls added, removed or changed (name, value, toggle,
/// expanded, selected, enabled), at most `max_lines` lines.
pub fn diff(before: &Snap, after: &Snap, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut note = String::new();
    if before.hwnd != after.hwnd {
        note = format!("Window changed: {:?} -> {:?}.", before.title.chars().take(50).collect::<String>(), after.title.chars().take(50).collect::<String>());
        let shown = max_lines.min(8);
        for n in after.nodes.iter().take(shown) {
            lines.push(format!("+ {}", describe_node(n)));
        }
        if after.nodes.len() > shown {
            lines.push(format!("+{} more (list_elements shows them all)", after.nodes.len() - shown));
        }
        return std::iter::once(note).chain(lines).collect();
    } else {
        // with a partial view on either side only the common leading part is comparable
        let limit = if before.complete && after.complete { usize::MAX } else { before.nodes.len().min(after.nodes.len()) };
        let b: std::collections::HashMap<&str, &SNode> = before.nodes.iter().take(limit).map(|n| (n.key.as_str(), n)).collect();
        let a: std::collections::HashMap<&str, &SNode> = after.nodes.iter().take(limit).map(|n| (n.key.as_str(), n)).collect();
        for n in after.nodes.iter().take(limit) {
            match b.get(n.key.as_str()) {
                None => lines.push(format!("+ {}", describe_node(n))),
                Some(o) => {
                    let mut ch = Vec::new();
                    if o.name != n.name {
                        ch.push(format!("name {:?} -> {:?}", o.name.chars().take(40).collect::<String>(), n.name.chars().take(40).collect::<String>()));
                    }
                    if o.tags != n.tags {
                        ch.push(format!("[{}] -> [{}]", o.tags, n.tags));
                    }
                    match (&o.value, &n.value) {
                        (None, Some(nv)) => ch.push(format!("value -> {}", preview(&nv.text, 60))),
                        (Some(_), None) => ch.push("value cleared".into()),
                        (Some(ov), Some(nv)) if ov.hash != nv.hash || ov.len != nv.len => ch.push(format!("value {} -> {}", preview(&ov.text, 40), preview(&nv.text, 60))),
                        _ => {}
                    }
                    if !ch.is_empty() {
                        lines.push(format!("~ [{}] {:?}: {}", type_name(n.ctype), n.name.chars().take(40).collect::<String>(), ch.join("; ")));
                    }
                }
            }
        }
        for n in before.nodes.iter().take(limit) {
            if !a.contains_key(n.key.as_str()) {
                lines.push(format!("- {}", describe_node(n)));
            }
        }
        if limit != usize::MAX {
            note = "(partial view: the window has more controls than the quick snapshot covers)".into();
        }
    }
    if lines.is_empty() {
        lines.push(if note.is_empty() { "no control changes detected".into() } else { format!("no control changes detected {note}") });
        return lines;
    }
    let total = lines.len();
    lines.truncate(max_lines);
    if total > max_lines {
        lines.push(format!("+{} more", total - max_lines));
    }
    if !note.is_empty() {
        lines.insert(0, note);
    }
    if lines.is_empty() {
        lines.push("no control changes detected".into());
    }
    lines
}

/// First on-screen element in the window being read whose name contains `name` (any case),
/// searched by Windows itself.
fn native_find(name: &str) -> Option<Elem> {
    let u = uia().ok()?;
    let (top, top_hwnd) = root().ok()?;
    unsafe {
        let name_cond = u.auto.CreatePropertyConditionEx(UIA_NamePropertyId, &VARIANT::from(name), PropertyConditionFlags(PropertyConditionFlags_IgnoreCase.0 | PropertyConditionFlags_MatchSubstring.0)).ok()?;
        let on_cond = u.auto.CreatePropertyCondition(UIA_IsOffscreenPropertyId, &VARIANT::from(false)).ok()?;
        let cond = u.auto.CreateAndCondition(&name_cond, &on_cond).ok()?;
        let e = top.FindFirstBuildCache(TreeScope_Descendants, &cond, &u.cache).ok()?;
        if in_protected_process(&e, &mut Default::default()) {
            return None; // a wait must not confirm the name of a control inside a protected program's embedded window
        }
        let (cx, cy) = visible_centre(&e, true)?;
        let ctype = e.CachedControlType().map(|t| t.0).unwrap_or(0);
        Some(Elem { name: s(e.CachedName()), ctype: type_name(ctype).into(), cx, cy, control: e, hwnd: top_hwnd.0 as isize })
    }
}

pub fn find_text(name: &str) -> Option<String> {
    native_find(name).map(|e| e.name)
}

/// Element by id from list_elements or by name; an id must still be where it was listed, in the window
/// it was listed in, and that window must still be the one being worked in and one that may be touched.
fn resolve(target: &str, verify: bool) -> Result<Elem, String> {
    if let Ok(id) = target.trim().parse::<usize>() {
        let list = ELEMENTS.lock().unwrap();
        let e = list.get(id.wrapping_sub(1)).cloned().ok_or_else(|| format!("No element #{id}; call list_elements first."))?;
        drop(list);
        let h = win::hwnd_from(e.hwnd);
        if !win::is_window(h) {
            return Err(format!("element #{id} {:?} belongs to a window that is gone; call list_elements again", e.name));
        }
        guards::refuse_host(h)?;
        guards::guard_target(h)?;
        guard_element(&e)?;
        if h != root_hwnd() {
            return Err(format!(
                "element #{id} {:?} was listed in {:?}, which is no longer the window being worked in (now: {}); call list_elements again for the window you mean",
                e.name,
                win::shown_title(h).chars().take(50).collect::<String>(),
                win::active_title()
            ));
        }
        if verify {
            match visible_centre(&e.control, false) {
                Some((x, y)) if (x - e.cx).abs() <= 4 && (y - e.cy).abs() <= 4 => {}
                _ => return Err(format!("element #{id} {:?} moved or vanished since list_elements; call list_elements again before retrying", e.name)),
            }
        }
        return Ok(e);
    }
    locate_named(target)?.ok_or_else(|| format!("No visible element named like {target:?}."))
}

/// An element by list_elements id (it must still be where it was listed when `verify` is set).
pub fn resolve_id(target: &str, verify: bool) -> Result<Elem, String> {
    resolve(target, verify)
}

/// True when the target is a list_elements id (a plain number), which only UI Automation can resolve.
pub fn is_id(target: &str) -> bool {
    target.trim().parse::<usize>().is_ok()
}

/// The window UI Automation reads (and OCR looks at): the viewed window, else the foreground one.
pub fn root_window() -> HWND {
    root_hwnd()
}

/// Visible control in the work window by name or automation id: exact name wins, then exact
/// automation id, then a name that contains the target, then any element (text included)
/// whose name contains it. Ok(None): nothing visible matches.
pub fn locate_named(target: &str) -> Result<Option<Elem>, String> {
    let want = target.to_lowercase();
    let mut exact: Option<Elem> = None;
    let mut by_id: Option<Elem> = None;
    let mut partial: Option<Elem> = None;
    let (top, top_hwnd) = root()?;
    crawl(&top, &uia()?.ext, &Limits { time: Duration::from_secs(3), nodes: 4000, per_collection: 500 }, |ctype, _| INTERACTIVE.contains(&ctype), |raw| {
        let low = raw.name.to_lowercase();
        if let Some((cx, cy)) = visible_centre(&raw.e, true) {
            let el = || Elem { name: raw.name.clone(), ctype: type_name(raw.ctype).into(), cx, cy, control: raw.e.clone(), hwnd: top_hwnd.0 as isize };
            if !low.is_empty() && low == want {
                exact = Some(el());
                return false;
            }
            if by_id.is_none() && unsafe { s(raw.e.CachedAutomationId()) }.to_lowercase() == want {
                by_id = Some(el());
            }
            if partial.is_none() && !low.is_empty() && low.contains(&want) {
                partial = Some(el());
            }
        }
        true
    })?;
    let found = exact.or(by_id).or(partial).or_else(|| native_find(target));
    if let Some(e) = &found {
        guard_element(e)?;
    }
    Ok(found)
}

/// The element's own process must pass the same policy as the window it was found in: a control can belong to another
/// process than its window (an embedded or hosted window), and the root window's check says nothing about that one.
fn guard_element(e: &Elem) -> Result<(), String> {
    let pid = unsafe { e.control.CurrentProcessId() }.unwrap_or(-1);
    // an element whose process can't be read is treated as unknown (pid 0 would pass as "the window itself")
    let pid = if pid <= 0 { u32::MAX } else { pid as u32 };
    guards::guard_element_process(pid, win::hwnd_from(e.hwnd))
}

pub fn set_value(target: &str, value: &str) -> Result<String, String> {
    if value.len() > crate::actions::MAX_TYPE_BYTES {
        return Err(format!("value is {} bytes; the limit is {} (1 MB)", value.len(), crate::actions::MAX_TYPE_BYTES));
    }
    guards::keep_work_window()?;
    let e = resolve(target, false)?;
    if let Some(p) = pattern::<IUIAutomationValuePattern>(&e.control, UIA_ValuePatternId) {
        let read_only = unsafe { p.CurrentIsReadOnly() }.map(|b| b.as_bool()).unwrap_or(true);
        if !read_only {
            unsafe { p.SetValue(&BSTR::from(value)) }.map_err(|e| format!("SetValue: {e}"))?;
            let now = unsafe { s(p.CurrentValue()) };
            return Ok(format!("set {:?}; it now reads {:?}", e.name, now.chars().take(200).collect::<String>()));
        }
    }
    unsafe { let _ = e.control.SetFocus(); }
    input::press_checked(&["ctrl+a".to_string()], &|| guards::recheck(None))?;
    crate::actions::type_text(value)?;
    let now = focused_text().unwrap_or_default();
    Ok(format!("typed into {:?} (no ValuePattern); it now reads {:?}", e.name, now.chars().take(200).collect::<String>()))
}

/// Text of the focused field, when the app exposes it.
pub fn focused_text() -> Option<String> {
    let f = focused()?;
    if let Some(p) = pattern::<IUIAutomationValuePattern>(&f, UIA_ValuePatternId) {
        return Some(unsafe { s(p.CurrentValue()) });
    }
    let tp = pattern::<IUIAutomationTextPattern>(&f, UIA_TextPatternId)?;
    Some(unsafe { s(tp.DocumentRange().ok()?.GetText(200_000)) })
}

pub fn wait_focused_text_change_from(before: &str, timeout: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < deadline {
        if focused_text().as_deref() != Some(before) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
}

pub fn read_text(target: Option<&str>, max_chars: usize) -> Result<(String, serde_json::Value), String> {
    let e = match target {
        Some(t) => resolve(t, false)?.control,
        None => focused().ok_or("nothing has keyboard focus")?,
    };
    let max = max_chars as i32;
    let (tname, cname) = unsafe { (type_name(e.CachedControlType().map(|t| t.0).unwrap_or(0)), s(e.CachedName())) };
    let mut out = vec![format!("[{tname}] {cname:?}")];
    let mut data = serde_json::json!({"type": tname, "name": cname});
    let value = pattern::<IUIAutomationValuePattern>(&e, UIA_ValuePatternId).map(|p| unsafe { s(p.CurrentValue()) });
    if let Some(v) = &value {
        let v: String = v.chars().take(max_chars).collect();
        out.push(format!("value: {v:?}"));
        data["value"] = serde_json::json!(v);
    }
    if let Some(tp) = pattern::<IUIAutomationTextPattern>(&e, UIA_TextPatternId) {
        let sel = selected_text(&e, max);
        if !sel.is_empty() {
            let sel: String = sel.chars().take(max_chars).collect();
            out.push(format!("selected: {sel:?}"));
            data["selected"] = serde_json::json!(sel);
        }
        if value.is_none() {
            if let Ok(range) = unsafe { tp.DocumentRange() } {
                let t = unsafe { s(range.GetText(max)) };
                out.push(format!("text: {t:?}"));
                data["text"] = serde_json::json!(t);
            }
        }
    }
    Ok((out.join("
"), data))
}

const ELEMENT_ACTIONS: &[&str] = &["expand", "collapse", "toggle", "select", "invoke", "focus", "scroll_into_view", "scroll_up", "scroll_down", "scroll_left", "scroll_right"];

pub fn element_action(target: &str, action: &str) -> Result<String, String> {
    guards::keep_work_window()?;
    let e = resolve(target, false)?;
    let act = action.trim().to_lowercase().replace(' ', "_");
    if !ELEMENT_ACTIONS.contains(&act.as_str()) {
        return Err(format!("action must be one of {}", ELEMENT_ACTIONS.join(", ")));
    }
    let label = e.name.clone();
    let unsupported = |what: &str| format!("{label:?} does not support {what}; try click_element");
    unsafe {
        match act.as_str() {
            "scroll_up" | "scroll_down" | "scroll_left" | "scroll_right" => {
                let p = pattern::<IUIAutomationScrollPattern>(&e.control, UIA_ScrollPatternId).ok_or_else(|| format!("{label:?} has no scroll support; use scroll with coordinates instead"))?;
                let (h, v) = match act.as_str() {
                    "scroll_up" => (ScrollAmount_NoAmount, ScrollAmount_LargeDecrement),
                    "scroll_down" => (ScrollAmount_NoAmount, ScrollAmount_LargeIncrement),
                    "scroll_left" => (ScrollAmount_LargeDecrement, ScrollAmount_NoAmount),
                    _ => (ScrollAmount_LargeIncrement, ScrollAmount_NoAmount),
                };
                p.Scroll(h, v).map_err(|e| e.to_string())?;
            }
            "expand" | "collapse" => {
                let p = pattern::<IUIAutomationExpandCollapsePattern>(&e.control, UIA_ExpandCollapsePatternId).ok_or_else(|| unsupported(&act))?;
                if act == "expand" { p.Expand() } else { p.Collapse() }.map_err(|e| e.to_string())?;
            }
            "toggle" => {
                let p = pattern::<IUIAutomationTogglePattern>(&e.control, UIA_TogglePatternId).ok_or_else(|| unsupported("toggle"))?;
                p.Toggle().map_err(|e| e.to_string())?;
                let on = p.CurrentToggleState().map(|st| st == ToggleState_On).unwrap_or(false);
                return Ok(format!("toggled {label:?}; it is now {}", if on { "on" } else { "off" }));
            }
            "select" => pattern::<IUIAutomationSelectionItemPattern>(&e.control, UIA_SelectionItemPatternId).ok_or_else(|| unsupported("select"))?.Select().map_err(|e| e.to_string())?,
            "invoke" => pattern::<IUIAutomationInvokePattern>(&e.control, UIA_InvokePatternId).ok_or_else(|| unsupported("invoke"))?.Invoke().map_err(|e| e.to_string())?,
            "focus" => e.control.SetFocus().map_err(|e| e.to_string())?,
            _ => pattern::<IUIAutomationScrollItemPattern>(&e.control, UIA_ScrollItemPatternId).ok_or_else(|| unsupported("scroll_into_view"))?.ScrollIntoView().map_err(|e| e.to_string())?,
        }
    }
    Ok(format!("{}: {label:?}", act.replace('_', " ")))
}

pub fn element_from_handle(hwnd: HWND) -> Option<IUIAutomationElement> {
    let u = uia().ok()?;
    unsafe { u.auto.ElementFromHandleBuildCache(hwnd, &u.cache) }.ok()
}
