//! On-screen indicator rendered with Windows.UI.Composition:
//! per monitor a click-through window holding accent glows fading in from the top and bottom
//! edges (opacity pulse + size breath), a status pill with a drop shadow, DirectWrite text and a
//! shimmer (a gradient mask sweeping over a brighter copy of the title) and a click ripple. There is no
//! pointer ring: the system cursors are never touched and the real Windows cursor stays visible
//! (input::wake_pointer brings it back when Windows hides it while typing). All animation runs on the compositor; this thread only changes state.

use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};
use crate::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{h, Interface, HSTRING, PCWSTR};
use windows_numerics::{Matrix3x2, Vector2, Vector3};
use windows::Foundation::TimeSpan;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Graphics::SizeInt32;
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::*;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_METRICS,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop, ICompositorInterop};
use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW,
    SetWindowDisplayAffinity, SetWindowPos, ShowWindow, SystemParametersInfoW, TranslateMessage, HWND_TOPMOST, MSG,
    PM_REMOVE, SPI_SETCURSORS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNA, WDA_EXCLUDEFROMCAPTURE, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::win::{self, Rect};
use crate::session;

pub const STOPPED_TEXT: &str = "■  Stopped  ·  you pressed Esc or moved the mouse to a screen corner";
pub const STOP_COLOR: &str = "#e5484d";
pub const DONE_COLOR: &str = "#2fb35a";
const TEXT_PX: f32 = 15.0;
const EDGE_PX: f32 = 96.0;
const SHIMMER_PERIOD_MS: i64 = 2400;
const BREATH_PERIOD_MS: i64 = 2600;
const FADE_MS: i64 = 300;

pub enum Msg {
    Show,
    Status(String),
    Place(Rect),
    /// the connected client changed: new accent colour and title text
    Style(String, String),
    Ripple(i32, i32),
    End(String, String, u64),
}

static TX: Mutex<Option<Sender<Msg>>> = Mutex::new(None);
static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn send(m: Msg) {
    if let Some(tx) = TX.lock().unwrap().as_ref() {
        let _ = tx.send(m);
    }
}

pub fn status(text: &str) {
    send(Msg::Status(text.to_string()));
}
pub fn show() {
    send(Msg::Show);
}
pub fn sync_monitor() {
    let mons = win::monitors();
    let i = win::follow(&mons);
    send(Msg::Place(mons[i].rect));
}
pub fn place(r: Rect) {
    send(Msg::Place(r));
}
pub fn ripple(x: i32, y: i32) {
    send(Msg::Ripple(x, y));
}
pub fn end(text: &str, color: &str) {
    end_for(text, color, 2000);
}
/// The ending banner, kept on screen for `ms` milliseconds.
pub fn end_for(text: &str, color: &str, ms: u64) {
    send(Msg::End(text.to_string(), color.to_string(), ms));
}
pub fn ready() -> bool {
    READY.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn accent() -> String {
    crate::client::accent()
}
/// The connected AI client changed: recolour the indicator and rename its title.
pub fn set_style(accent: &str, title: &str) {
    send(Msg::Style(accent.to_string(), title.to_string()));
}
pub fn string(key: &str, default: &str) -> String {
    crate::config().pointer(&format!("/strings/{key}")).and_then(|v| v.as_str()).unwrap_or(default).to_string()
}

pub fn start() {
    let (tx, rx) = mpsc::channel();
    *TX.lock().unwrap() = Some(tx);
    std::thread::Builder::new()
        .name("overlay".into())
        .spawn(move || {
            if let Err(e) = run(rx) {
                eprintln!("overlay: {e}");
            }
        })
        .expect("overlay thread");
}

/// Reload the user's cursor scheme from the registry (SPI_SETCURSORS). This server never changes
/// the system cursors any more; the call is run once at start-up to repair a system that an older
/// build (which swapped them for a blank cursor) left with invisible cursors.
pub fn restore_cursors() {
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
}

/// `desktop-control --cursor-manager <pid>`: the watchdog of older builds. It is a no-op now apart
/// from reloading the cursor scheme, so a stale invocation can never leave anything hidden.
pub fn cursor_manager(_pid: u32) {
    restore_cursors();
}

// ---------- colours and helpers ----------

fn rgb(hex: &str) -> (u8, u8, u8) {
    let p = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    (p(1), p(3), p(5))
}
fn color(hex: &str, a: u8) -> Color {
    let (r, g, b) = rgb(hex);
    Color { A: a, R: r, G: g, B: b }
}
fn d2d(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.R as f32 / 255.0, g: c.G as f32 / 255.0, b: c.B as f32 / 255.0, a: c.A as f32 / 255.0 }
}
fn v2(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}
fn v3(x: f32, y: f32, z: f32) -> Vector3 {
    Vector3 { X: x, Y: y, Z: z }
}
fn span(ms: i64) -> TimeSpan {
    TimeSpan { Duration: ms * 10_000 }
}

// ---------- drawing (Direct2D + DirectWrite onto composition surfaces) ----------

struct Gfx {
    compositor: Compositor,
    gdev: CompositionGraphicsDevice,
    dwrite: IDWriteFactory,
    d2d: ID2D1Factory1,
}

impl Gfx {
    fn new(compositor: &Compositor) -> windows::core::Result<Gfx> {
        let d3d = crate::capture::d3d_device().map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), e))?;
        let dxgi: IDXGIDevice = d3d.cast()?;
        let d2d: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let device: ID2D1Device = unsafe { d2d.CreateDevice(&dxgi)? };
        let interop: ICompositorInterop = compositor.cast()?;
        let gdev = unsafe { interop.CreateGraphicsDevice(&device)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Gfx { compositor: compositor.clone(), gdev, dwrite, d2d })
    }

    fn format(&self, bold: bool, px: f32) -> windows::core::Result<IDWriteTextFormat> {
        let f = self.format_raw(bold, px)?;
        unsafe {
            // text sits vertically centred in its box, flush left
            let _ = f.SetParagraphAlignment(windows::Win32::Graphics::DirectWrite::DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = f.SetTextAlignment(windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_LEADING);
            let _ = f.SetWordWrapping(windows::Win32::Graphics::DirectWrite::DWRITE_WORD_WRAPPING_NO_WRAP);
        }
        Ok(f)
    }

    fn format_raw(&self, bold: bool, px: f32) -> windows::core::Result<IDWriteTextFormat> {
        unsafe {
            self.dwrite.CreateTextFormat(
                h!("Segoe UI"),
                None,
                if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                px,
                h!("en-us"),
            )
        }
    }

    fn measure(&self, text: &str, format: &IDWriteTextFormat) -> f32 {
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            let Ok(layout) = self.dwrite.CreateTextLayout(&wide, format, 4096.0, 256.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
    }

    /// A surface of w x h logical px drawn by `draw` (device context already offset).
    fn surface(&self, w: f32, h: f32, scale: f32, draw: impl FnOnce(&ID2D1DeviceContext)) -> windows::core::Result<CompositionSurfaceBrush> {
        let pw = (w * scale).ceil().max(1.0) as i32;
        let ph = (h * scale).ceil().max(1.0) as i32;
        let surface = self.gdev.CreateDrawingSurface2(SizeInt32 { Width: pw, Height: ph }, DirectXPixelFormat::B8G8R8A8UIntNormalized, DirectXAlphaMode::Premultiplied)?;
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        unsafe {
            let mut offset = POINT::default();
            let dc: ID2D1DeviceContext = interop.BeginDraw(None, &mut offset)?;
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: offset.x as f32, M32: offset.y as f32 });
            draw(&dc);
            interop.EndDraw()?;
        }
        let brush = self.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?; // surface is scale x denser than the visual; fill it exactly
        Ok(brush)
    }

    fn text_surface(&self, text: &str, format: &IDWriteTextFormat, w: f32, h: f32, scale: f32, c: Color) -> windows::core::Result<CompositionSurfaceBrush> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        self.surface(w, h, scale, |dc| unsafe {
            if let Ok(brush) = dc.CreateSolidColorBrush(&d2d(c), None) {
                dc.DrawText(&wide, format, &D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h }, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
        })
    }
}

// ---------- per-monitor overlay window ----------

struct Layer {
    rect: Rect,
    hwnd: HWND,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    scale: f32,
    top: SpriteVisual,
    bottom: SpriteVisual,
    pill_root: ContainerVisual,
    pill: ContainerVisual,
    capsule: SpriteVisual,
    ripple: ShapeVisual,
    ripple_shape: CompositionSpriteShape,
    pill_key: String,
    shimmer_props: Option<CompositionPropertySet>,
}

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

fn class() -> PCWSTR {
    static NAME: OnceLock<HSTRING> = OnceLock::new();
    let name = NAME.get_or_init(|| {
        let name = HSTRING::from("ClaudeDesktopControlOverlay");
        unsafe {
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), lpszClassName: PCWSTR(name.as_ptr()), hInstance: windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default().into(), ..Default::default() };
            RegisterClassW(&wc);
        }
        name
    });
    PCWSTR(name.as_ptr())
}

impl Layer {
    fn new(compositor: &Compositor, gfx: &Gfx, rect: Rect, capturable: bool) -> windows::core::Result<Layer> {
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class(), PCWSTR::null(), WS_POPUP, rect.left, rect.top, rect.width, rect.height, None, None, None, None,
            )?
        };
        unsafe {
            if !capturable {
                let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            }
            let _ = ShowWindow(hwnd, SW_SHOWNA);
        }
        let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, true)? };
        let root = compositor.CreateContainerVisual()?;
        root.SetSize(v2(rect.width as f32 / scale, rect.height as f32 / scale))?;
        root.SetScale(v3(scale, scale, 1.0))?;
        root.SetOpacity(0.0)?;
        target.SetRoot(&root)?;
        let lw = rect.width as f32 / scale;
        let lh = rect.height as f32 / scale;

        // edge glows (top and bottom fade) with opacity pulse + size breath
        let make_edge = |top: bool| -> windows::core::Result<SpriteVisual> {
            let v = compositor.CreateSpriteVisual()?;
            v.SetSize(v2(lw, EDGE_PX))?;
            v.SetOffset(v3(0.0, if top { 0.0 } else { lh - EDGE_PX }, 0.0))?;
            if !top {
                v.SetAnchorPoint(v2(0.0, 1.0))?;
                v.SetOffset(v3(0.0, lh, 0.0))?;
            }
            let pulse = compositor.CreateScalarKeyFrameAnimation()?;
            pulse.InsertKeyFrame(0.0, 0.55)?;
            pulse.InsertKeyFrame(1.0, 1.0)?;
            pulse.SetDuration(span(BREATH_PERIOD_MS / 2))?;
            pulse.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
            pulse.SetDirection(AnimationDirection::Alternate)?;
            v.StartAnimation(h!("Opacity"), &pulse)?;
            let breath = compositor.CreateVector2KeyFrameAnimation()?;
            breath.InsertKeyFrame(0.0, v2(lw, EDGE_PX * 0.85))?;
            breath.InsertKeyFrame(1.0, v2(lw, EDGE_PX * 1.15))?;
            breath.SetDuration(span(BREATH_PERIOD_MS / 2))?;
            breath.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
            breath.SetDirection(AnimationDirection::Alternate)?;
            v.StartAnimation(h!("Size"), &breath)?;
            root.Children()?.InsertAtTop(&v)?;
            Ok(v)
        };
        let top = make_edge(true)?;
        let bottom = make_edge(false)?;

        // pill: a capsule sprite (Direct2D surface) that casts the shadow, text on top
        let pill_root = compositor.CreateContainerVisual()?;
        let capsule = compositor.CreateSpriteVisual()?;
        let shadow = compositor.CreateDropShadow()?;
        shadow.SetBlurRadius(18.0)?;
        shadow.SetOffset(v3(0.0, 5.0, 0.0))?;
        shadow.SetColor(Color { A: 120, R: 0, G: 0, B: 0 })?;
        capsule.SetShadow(&shadow)?;
        pill_root.Children()?.InsertAtTop(&capsule)?;
        let pill = compositor.CreateContainerVisual()?;
        pill_root.Children()?.InsertAtTop(&pill)?;
        root.Children()?.InsertAtTop(&pill_root)?;

        // ripple ring
        let ring = compositor.CreateEllipseGeometry()?;
        ring.SetRadius(v2(14.0, 14.0))?;
        ring.SetCenter(v2(18.0, 18.0))?;
        let ripple_shape = compositor.CreateSpriteShapeWithGeometry(&ring)?;
        ripple_shape.SetStrokeThickness(3.0)?;
        let ripple = compositor.CreateShapeVisual()?;
        ripple.SetSize(v2(36.0, 36.0))?;
        ripple.Shapes()?.Append(&ripple_shape)?;
        ripple.SetCenterPoint(v3(18.0, 18.0, 0.0))?;
        ripple.SetOpacity(0.0)?;
        root.Children()?.InsertAtTop(&ripple)?;

        Ok(Layer { rect, hwnd, _target: target, root, scale, top, bottom, pill_root, pill, capsule, ripple, ripple_shape, pill_key: String::new(), shimmer_props: None })
    }

    fn set_accent(&self, compositor: &Compositor, accent: &str) -> windows::core::Result<()> {
        for (v, top) in [(&self.top, true), (&self.bottom, false)] {
            let g = compositor.CreateLinearGradientBrush()?;
            g.SetStartPoint(v2(0.0, if top { 0.0 } else { 1.0 }))?;
            g.SetEndPoint(v2(0.0, if top { 1.0 } else { 0.0 }))?;
            let stops = g.ColorStops()?;
            stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(0.0, color(accent, 210))?)?;
            stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(0.45, color(accent, 70))?)?;
            stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(1.0, color(accent, 0))?)?;
            v.SetBrush(&g)?;
        }
        self.ripple_shape.SetStrokeBrush(&compositor.CreateColorBrushWithColor(color(accent, 235))?)?;
        Ok(())
    }

    /// Rebuild the pill: dot, title (with shimmer when `shimmer`), status, "Esc to cancel".
    fn set_pill(&mut self, gfx: &Gfx, title: &str, status: Option<&str>, esc: Option<&str>, accent: &str, shimmer: bool) -> windows::core::Result<()> {
        let key = format!("{title}|{status:?}|{esc:?}|{accent}|{shimmer}");
        if self.pill_key == key {
            return Ok(());
        }
        self.pill_key = key;
        let c = &gfx.compositor;
        let children = self.pill.Children()?;
        children.RemoveAll()?;
        self.shimmer_props = None;
        let bold = gfx.format(true, TEXT_PX)?;
        let dim = gfx.format(false, TEXT_PX)?;
        let (pad, gap, h) = (20.0f32, 11.0f32, (TEXT_PX * 2.6).round());
        let mut parts: Vec<(String, &IDWriteTextFormat, Color)> = vec![(title.to_string(), &bold, Color { A: if shimmer { 175 } else { 255 }, R: 255, G: 255, B: 255 })];
        if let Some(s) = status {
            parts.push((s.to_string(), &dim, Color { A: 235, R: 255, G: 255, B: 255 }));
        }
        if let Some(e) = esc {
            parts.push((e.to_string(), &dim, Color { A: 185, R: 255, G: 255, B: 255 }));
        }
        let widths: Vec<f32> = parts.iter().map(|(t, f, _)| gfx.measure(t, f).ceil()).collect();
        let dot = 4.0;
        let w = 2.0 * pad + widths.iter().sum::<f32>() + (parts.len() as f32 - 1.0) * (2.0 * gap + dot);
        // capsule
        let fill = d2d(color(accent, 255));
        self.capsule.SetSize(v2(w, h))?;
        self.capsule.SetBrush(&gfx.surface(w, h, self.scale, move |dc| unsafe {
            if let Ok(b) = dc.CreateSolidColorBrush(&fill, None) {
                let rr = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h }, radiusX: h / 2.0, radiusY: h / 2.0 };
                dc.FillRoundedRectangle(&rr, &b);
            }
        })?)?;
        // text parts
        let mut x = pad;
        for (i, (text, format, col)) in parts.iter().enumerate() {
            if i > 0 {
                x += gap;
                let dg = c.CreateEllipseGeometry()?;
                dg.SetRadius(v2(dot / 2.0, dot / 2.0))?;
                dg.SetCenter(v2(dot / 2.0, dot / 2.0))?;
                let ds = c.CreateSpriteShapeWithGeometry(&dg)?;
                ds.SetFillBrush(&c.CreateColorBrushWithColor(Color { A: 150, R: 255, G: 255, B: 255 })?)?;
                let dv = c.CreateShapeVisual()?;
                dv.SetSize(v2(dot, dot))?;
                dv.Shapes()?.Append(&ds)?;
                dv.SetOffset(v3(x, h / 2.0 - dot / 2.0, 0.0))?;
                children.InsertAtTop(&dv)?;
                x += dot + gap;
            }
            let tw = widths[i] + 2.0;
            let tv = c.CreateSpriteVisual()?;
            tv.SetSize(v2(tw, h))?;
            tv.SetOffset(v3(x, 0.0, 0.0))?;
            tv.SetBrush(&gfx.text_surface(text, format, tw, h, self.scale, *col)?)?;
            children.InsertAtTop(&tv)?;
            if i == 0 && shimmer {
                // a pure-white copy of the title seen through a gradient band that sweeps across
                let glow = gfx.text_surface(text, format, tw, h, self.scale, Color { A: 255, R: 255, G: 255, B: 255 })?;
                let mask = c.CreateLinearGradientBrush()?;
                mask.SetMappingMode(CompositionMappingMode::Absolute)?;
                let stops = mask.ColorStops()?;
                for (o, a) in [(0.0, 0u8), (0.5, 255u8), (1.0, 0u8)] {
                    stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(o, Color { A: a, R: 255, G: 255, B: 255 })?)?;
                }
                let props = c.CreatePropertySet()?;
                props.InsertScalar(h!("Progress"), 0.0)?;
                props.InsertScalar(h!("StartX"), -0.3 * tw)?;
                props.InsertScalar(h!("TravelX"), 1.6 * tw)?;
                props.InsertScalar(h!("Band"), 0.44 * tw)?;
                let start = c.CreateExpressionAnimationWithExpression(h!("Vector2(shimmer.StartX + shimmer.TravelX * shimmer.Progress - shimmer.Band / 2, 0)"))?;
                start.SetReferenceParameter(h!("shimmer"), &props)?;
                mask.StartAnimation(h!("StartPoint"), &start)?;
                let endp = c.CreateExpressionAnimationWithExpression(h!("Vector2(shimmer.StartX + shimmer.TravelX * shimmer.Progress + shimmer.Band / 2, 0)"))?;
                endp.SetReferenceParameter(h!("shimmer"), &props)?;
                mask.StartAnimation(h!("EndPoint"), &endp)?;
                let prog = c.CreateScalarKeyFrameAnimation()?;
                prog.InsertKeyFrame(0.0, 0.0)?;
                prog.InsertKeyFrame(1.0, 1.0)?;
                prog.SetDuration(span(SHIMMER_PERIOD_MS))?;
                prog.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                props.StartAnimation(h!("Progress"), &prog)?;
                let mb = c.CreateMaskBrush()?;
                mb.SetSource(&glow)?;
                mb.SetMask(&mask)?;
                let gv = c.CreateSpriteVisual()?;
                gv.SetSize(v2(tw, h))?;
                gv.SetOffset(v3(x, 0.0, 0.0))?;
                gv.SetBrush(&mb)?;
                children.InsertAtTop(&gv)?;
                self.shimmer_props = Some(props);
            }
            x += widths[i];
        }
        self.pill.SetSize(v2(w, h))?;
        self.pill_root.SetSize(v2(w, h))?;
        let lw = self.rect.width as f32 / self.scale;
        self.pill_root.SetOffset(v3(((lw - w) / 2.0).round(), 8.0, 0.0))?;
        Ok(())
    }

    fn fade(&self, compositor: &Compositor, to: f32) -> windows::core::Result<()> {
        let a = compositor.CreateScalarKeyFrameAnimation()?;
        a.InsertKeyFrame(1.0, to)?;
        a.SetDuration(span(FADE_MS))?;
        self.root.StartAnimation(h!("Opacity"), &a)
    }

    fn play_ripple(&self, compositor: &Compositor, x: i32, y: i32) -> windows::core::Result<()> {
        let lx = (x - self.rect.left) as f32 / self.scale;
        let ly = (y - self.rect.top) as f32 / self.scale;
        self.ripple.SetOffset(v3(lx - 18.0, ly - 18.0, 0.0))?;
        let scale = compositor.CreateVector3KeyFrameAnimation()?;
        scale.InsertKeyFrame(0.0, v3(0.35, 0.35, 1.0))?;
        scale.InsertKeyFrameWithEasingFunction(1.0, v3(2.2, 2.2, 1.0), &compositor.CreateCubicBezierEasingFunction(v2(0.2, 0.8), v2(0.3, 1.0))?)?;
        scale.SetDuration(span(450))?;
        self.ripple.StartAnimation(h!("Scale"), &scale)?;
        let op = compositor.CreateScalarKeyFrameAnimation()?;
        op.InsertKeyFrame(0.0, 0.95)?;
        op.InsertKeyFrame(1.0, 0.0)?;
        op.SetDuration(span(450))?;
        self.ripple.StartAnimation(h!("Opacity"), &op)
    }
}

// ---------- the overlay thread ----------

#[derive(PartialEq)]
enum Mode {
    Hidden,
    Working,
    Ending,
}

fn run(rx: Receiver<Msg>) -> windows::core::Result<()> {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }
    let opts = DispatcherQueueOptions { dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32, threadType: DQTYPE_THREAD_CURRENT, apartmentType: DQTAT_COM_NONE };
    // keep the controller alive for the thread's lifetime: dropping it tears the queue down
    let _controller = unsafe { CreateDispatcherQueueController(opts)? };
    let capturable = std::env::var("DESKTOP_CONTROL_CAPTURABLE").map(|v| v == "1").unwrap_or(false);
    let compositor = Compositor::new().map_err(|e| { eprintln!("overlay: Compositor::new failed: {e}"); e })?;
    let gfx = Gfx::new(&compositor).map_err(|e| { eprintln!("overlay: Gfx::new failed: {e}"); e })?;
    let mut accent = accent();
    let mut title_text = crate::client::using_text();
    let esc_text = string("escToCancel", "Esc to cancel");
    let mut layers: Vec<Layer> = Vec::new();
    for m in win::monitors() {
        let mut l = Layer::new(&compositor, &gfx, m.rect, capturable)?;
        l.set_accent(&compositor, &accent)?;
        l.set_pill(&gfx, &title_text, Some("thinking…"), Some(&esc_text), &accent, true)?;
        layers.push(l);
    }
    READY.store(true, std::sync::atomic::Ordering::Relaxed);

    let mut mode = Mode::Hidden;
    let mut current: usize = 0; // layer holding the pill
    let mut used: Vec<usize> = Vec::new();
    let mut status_text = "thinking…".to_string();
    let mut until = Instant::now();
    let mut msg = MSG::default();
    let mut last_poll = Instant::now();
    let set_visible = |layers: &[Layer], used: &[usize], current: usize, on: bool| {
        for (i, l) in layers.iter().enumerate() {
            let show = on && used.contains(&i);
            let _ = l.fade(&compositor, if show { 1.0 } else { 0.0 });
            let _ = l.pill_root.SetOpacity(if i == current { 1.0 } else { 0.0 });
        }
    };
    loop {
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(m) => match m {
                Msg::Show => {
                    used.clear();
                    mode = Mode::Working;
                    for l in layers.iter_mut() {
                        l.set_pill(&gfx, &title_text, Some(&status_text), Some(&esc_text), &accent, true)?;
                    }
                    for l in &layers {
                        unsafe { let _ = SetWindowPos(l.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE); }
                    }
                }
                Msg::Status(s) => {
                    status_text = s;
                    if mode == Mode::Working {
                        layers[current].set_pill(&gfx, &title_text, Some(&status_text), Some(&esc_text), &accent, true)?;
                    }
                }
                Msg::Place(r) => {
                    if let Some(i) = layers.iter().position(|l| l.rect == r) {
                        current = i;
                        if !used.contains(&i) {
                            used.push(i);
                        }
                        if mode == Mode::Working {
                            layers[i].set_pill(&gfx, &title_text, Some(&status_text), Some(&esc_text), &accent, true)?;
                            set_visible(&layers, &used, current, true);
                        }
                    }
                }
                Msg::Style(a, t) => {
                    accent = a;
                    title_text = t;
                    for l in &layers {
                        l.set_accent(&compositor, &accent)?;
                    }
                    if mode == Mode::Working {
                        let st = status_text.clone();
                        for l in layers.iter_mut() {
                            l.set_pill(&gfx, &title_text, Some(&st), Some(&esc_text), &accent, true)?;
                        }
                    }
                }
                Msg::Ripple(x, y) => {
                    let mons = win::monitors();
                    let i = win::monitor_at(x, y, &mons).min(layers.len() - 1);
                    let _ = layers[i].play_ripple(&compositor, x, y);
                }
                Msg::End(text, col, ms) => {
                    let text: String = text.chars().take(90).collect();
                    layers[current].set_pill(&gfx, &text, None, None, &col, false)?;
                    if !used.contains(&current) {
                        used.push(current);
                    }
                    set_visible(&layers, &used, current, true);
                    mode = Mode::Ending;
                    until = Instant::now() + Duration::from_millis(ms);
                }
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // idle / turn-ended bookkeeping (every 100 ms), like the Python indicator
        if last_poll.elapsed() >= Duration::from_millis(100) {
            last_poll = Instant::now();
            crate::hooks::poll_stop();
            if mode == Mode::Working {
                // the task is over when the turn ended (like finish) or after 30 s without a call
                if let Some(ended) = session::poll_end() {
                    if ended.reason == session::Reason::Idle {
                        mode = Mode::Hidden;
                        set_visible(&layers, &used, current, false);
                        }
                    // the switch back to VS Code can take a moment: off the overlay thread
                    let _ = std::thread::Builder::new().name("complete".into()).spawn(move || {
                        session::complete(ended, None);
                    });
                } else if !session::TASK.lock().unwrap().active {
                    // ended some other way (the user's stop): just hide
                    mode = Mode::Hidden;
                    set_visible(&layers, &used, current, false);
                    crate::capture::release();
                }
            } else if mode == Mode::Ending && Instant::now() > until {
                mode = Mode::Hidden;
                set_visible(&layers, &used, current, false);
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn _unused(_: HWND, _: RECT) {
    unsafe {
        let _ = SetFocus(None);
    }
}
