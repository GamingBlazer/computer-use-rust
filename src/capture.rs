//! Screenshots through Windows.Graphics.Capture: a window's own pixels even
//! when covered, GPU-rendered apps included; monitors the same way. The capture session for the
//! latest target stays open (a cached capture session), so a repeat screenshot only
//! takes the newest frame. Frames are copied to a staging texture, downscaled and JPEG-encoded.

use crate::sync::Mutex;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use image::{imageops, ImageBuffer, RgbaImage};
use windows::core::{Interface, Result};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use crate::win::Rect;

pub const JPEG_QUALITY: u8 = 70;

struct Devices {
    d3d: ID3D11Device,
    winrt: IDirect3DDevice,
}

// The D3D11 device is created without single-threaded flags, so it is free-threaded.
unsafe impl Send for Devices {}
unsafe impl Sync for Devices {}

static DEVICES: OnceLock<std::result::Result<Devices, String>> = OnceLock::new();

fn devices() -> std::result::Result<&'static Devices, String> {
    DEVICES
        .get_or_init(|| {
            let mut device = None;
            unsafe {
                D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, None)
                    .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
            }
            let d3d = device.ok_or("no D3D11 device")?;
            let dxgi: IDXGIDevice = d3d.cast().map_err(|e| e.to_string())?;
            let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }.map_err(|e| e.to_string())?;
            let winrt: IDirect3DDevice = inspectable.cast().map_err(|e| e.to_string())?;
            Ok(Devices { d3d, winrt })
        })
        .as_ref()
        .map_err(|e| e.clone())
}

pub fn d3d_device() -> std::result::Result<ID3D11Device, String> {
    devices().map(|d| d.d3d.clone())
}

fn interop() -> Result<IGraphicsCaptureItemInterop> {
    windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Target {
    Window(isize),
    Monitor(isize),
}

struct Session {
    target: Target,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    latest: Arc<Mutex<(u64, Option<Direct3D11CaptureFrame>)>>, // (frame counter, newest frame); older frames are dropped so the pool keeps producing
    seen: u64,
    last: Option<RgbaImage>,
}
unsafe impl Send for Session {}

static CACHE: Mutex<Option<Session>> = Mutex::new(None);

/// Target and moment at which settle() last saw the screen quiet: a screenshot right after it
/// can take the newest frame at once instead of waiting 60 ms for one that may never come.
static STABLE: Mutex<Option<(Target, std::time::Instant)>> = Mutex::new(None);

pub fn mark_stable(target: Target) {
    *STABLE.lock().unwrap() = Some((target, std::time::Instant::now()));
}

/// Whether a capture session for `target` is open, opening one when `open_if_missing` is set
/// (a different cached target is closed, as grab() would do).
pub fn session_for(target: Target, open_if_missing: bool) -> bool {
    let mut cache = CACHE.lock().unwrap();
    if !matches!(cache.as_ref(), Some(s) if s.target == target) {
        if !open_if_missing {
            return false;
        }
        if let Some(old) = cache.take() {
            let _ = old.session.Close();
            let _ = old.pool.Close();
        }
        *cache = open(target).ok();
    }
    cache.is_some()
}

/// Hash of the pixels of the newest frame of the open session for `target`: equal hashes mean
/// an identical picture. (The frame counter is no use for this: Windows delivers frames
/// continuously, changed or not.) None when there is no session or no frame yet.
pub fn picture_hash(target: Target) -> Option<u64> {
    let cache = CACHE.lock().unwrap();
    let s = cache.as_ref().filter(|s| s.target == target)?;
    let l = s.latest.lock().unwrap();
    let frame = l.1.as_ref()?;
    frame_hash(frame).ok()
}

fn frame_hash(frame: &Direct3D11CaptureFrame) -> std::result::Result<u64, String> {
    let dev = devices()?;
    let content = frame.ContentSize().map_err(|e| e.to_string())?;
    let surface = frame.Surface().map_err(|e| e.to_string())?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| e.to_string())?;
    let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(|e| e.to_string())?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    // a cap on what one capture may allocate (RGBA, 4 bytes a pixel): 64 megapixels is about 256 MB
    let (cw, ch) = ((content.Width.max(1) as u32).min(desc.Width) as u64, (content.Height.max(1) as u32).min(desc.Height) as u64);
    if cw * ch > MAX_CAPTURE_PIXELS {
        return Err(format!("the capture is {cw}x{ch} pixels, more than the {} megapixel limit; capture a smaller window or one monitor", MAX_CAPTURE_PIXELS / 1_000_000));
    }
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut staging = None;
    unsafe { dev.d3d.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(|e| e.to_string())?;
    let staging = staging.ok_or("no staging texture")?;
    let ctx = unsafe { dev.d3d.GetImmediateContext() }.map_err(|e| e.to_string())?;
    unsafe { ctx.CopyResource(&staging, &texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(|e| e.to_string())?;
    let w = (content.Width.max(1) as usize).min(desc.Width as usize);
    let h = (content.Height.max(1) as usize).min(desc.Height as usize);
    let mut hash = 0xcbf29ce484222325u64 ^ ((w as u64) << 32 | h as u64);
    unsafe {
        for y in 0..h {
            let row = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), w * 4);
            let mut chunks = row.chunks_exact(8);
            for c in &mut chunks {
                hash = (hash ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(0x100000001b3).rotate_left(23);
            }
            for &b in chunks.remainder() {
                hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
            }
        }
        ctx.Unmap(&staging, 0);
    }
    Ok(hash)
}

static BORDERLESS: OnceLock<bool> = OnceLock::new();

/// Ask Windows once for borderless capture; without it SetIsBorderRequired(false) is refused and
/// Windows' yellow capture border duplicates the indicator. Unpackaged apps are granted it when
/// Settings > Privacy & security > Screenshot borders lets desktop apps turn the border off.
/// Runs on its own thread with a timeout so a stuck request can never block a screenshot.
fn borderless_allowed() -> bool {
    *BORDERLESS.get_or_init(|| {
        use windows::Graphics::Capture::{GraphicsCaptureAccess, GraphicsCaptureAccessKind};
        use windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let status = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless).and_then(|op| op.get());
            let _ = tx.send(status);
        });
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(s)) => {
                eprintln!("capture: borderless access status {} (4 = allowed, 2 = denied by user)", s.0);
                s == AppCapabilityAccessStatus::Allowed
            }
            Ok(Err(e)) => {
                eprintln!("capture: borderless access request failed: {e}");
                false
            }
            Err(_) => {
                eprintln!("capture: borderless access request timed out");
                false
            }
        }
    })
}

fn open(target: Target) -> std::result::Result<Session, String> {
    let dev = devices()?;
    let item = unsafe {
        match target {
            Target::Window(h) => interop().and_then(|i| i.CreateForWindow::<GraphicsCaptureItem>(HWND(h as *mut _))),
            Target::Monitor(h) => interop().and_then(|i| i.CreateForMonitor::<GraphicsCaptureItem>(HMONITOR(h as *mut _))),
        }
    }
    .map_err(|e| format!("capture item: {e}"))?;
    let size = item.Size().map_err(|e| e.to_string())?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&dev.winrt, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size).map_err(|e| format!("frame pool: {e}"))?;
    let session = pool.CreateCaptureSession(&item).map_err(|e| format!("capture session: {e}"))?;
    let _ = session.SetIsCursorCaptureEnabled(false);
    if borderless_allowed() {
        if let Err(e) = session.SetIsBorderRequired(false) {
            eprintln!("capture: Windows kept the capture border: {e}");
        }
    }
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("capture: border required = {:?}", session.IsBorderRequired());
    }
    let latest: Arc<Mutex<(u64, Option<Direct3D11CaptureFrame>)>> = Arc::new(Mutex::new((0, None)));
    let slot = latest.clone();
    pool.FrameArrived(&TypedEventHandler::new(move |pool: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
        if let Some(p) = pool.as_ref() {
            if let Ok(frame) = p.TryGetNextFrame() {
                let mut l = slot.lock().unwrap();
                l.0 += 1;
                l.1 = Some(frame); // drops the previous frame, releasing its buffer
            }
        }
        Ok(())
    }))
    .map_err(|e| e.to_string())?;
    session.StartCapture().map_err(|e| format!("StartCapture: {e}"))?;
    Ok(Session { target, pool, session, latest, seen: 0, last: None })
}

/// Close the cached capture session when a task ends. While a session is open Windows draws its
/// yellow capture border (SetIsBorderRequired(false) is refused without consent), so keeping the
/// session past the task left that border on screen until the server exited.
pub fn release() {
    if let Some(old) = CACHE.lock().unwrap().take() {
        let _ = old.session.Close();
        let _ = old.pool.Close();
        eprintln!("capture: closed the {:?} session (task ended)", old.target);
    }
}

const MAX_CAPTURE_PIXELS: u64 = 64_000_000;

fn frame_to_image(frame: &Direct3D11CaptureFrame) -> std::result::Result<RgbaImage, String> {
    let dev = devices()?;
    let content = frame.ContentSize().map_err(|e| e.to_string())?;
    let surface = frame.Surface().map_err(|e| e.to_string())?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| e.to_string())?;
    let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(|e| e.to_string())?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut staging = None;
    unsafe { dev.d3d.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(|e| e.to_string())?;
    let staging = staging.ok_or("no staging texture")?;
    let ctx = unsafe { dev.d3d.GetImmediateContext() }.map_err(|e| e.to_string())?;
    unsafe { ctx.CopyResource(&staging, &texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(|e| e.to_string())?;
    let w = (content.Width.max(1) as u32).min(desc.Width);
    let h = (content.Height.max(1) as u32).min(desc.Height);
    let mut img: RgbaImage = ImageBuffer::new(w, h);
    unsafe {
        let buf: &mut [u8] = img.as_mut();
        for y in 0..h as usize {
            let row = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), w as usize * 4);
            let out = &mut buf[y * w as usize * 4..(y + 1) * w as usize * 4];
            for x in 0..w as usize {
                out[x * 4] = row[x * 4 + 2];
                out[x * 4 + 1] = row[x * 4 + 1];
                out[x * 4 + 2] = row[x * 4];
                out[x * 4 + 3] = row[x * 4 + 3]; // keep alpha: popup shadows blend instead of turning black
            }
        }
        ctx.Unmap(&staging, 0);
    }
    Ok(img)
}

/// The newest frame of a target: reuses the open session for the same target, otherwise opens
/// one. Content that has not changed since the last frame returns the last image.
fn grab(target: Target) -> std::result::Result<RgbaImage, String> {
    let mut cache = CACHE.lock().unwrap();
    let fresh = !matches!(cache.as_ref(), Some(s) if s.target == target);
    if fresh {
        if let Some(old) = cache.take() {
            let _ = old.session.Close();
            let _ = old.pool.Close();
        }
        *cache = Some(open(target)?);
    }
    let s = cache.as_mut().unwrap();
    // wait briefly for a frame newer than the one already converted (3 s for the first ever)
    let quiet = matches!(*STABLE.lock().unwrap(), Some((t, at)) if t == target && at.elapsed() < Duration::from_millis(300));
    let wait = if s.last.is_some() { if quiet { Duration::ZERO } else { Duration::from_millis(60) } } else { Duration::from_secs(3) };
    let deadline = std::time::Instant::now() + wait;
    let mut newest: Option<Direct3D11CaptureFrame> = None;
    loop {
        {
            let mut l = s.latest.lock().unwrap();
            if l.0 > s.seen {
                s.seen = l.0;
                newest = l.1.take();
                break;
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    if newest.is_none() && s.last.is_none() {
        *cache = None;
        return Err("no frame arrived within 3 s".into());
    }
    if let Some(f) = newest {
        match frame_to_image(&f) {
            Ok(img) => s.last = Some(img),
            Err(e) => {
                *cache = None;
                return Err(e);
            }
        }
    }
    Ok(s.last.clone().unwrap())
}

pub fn capture_monitor(hmon: HMONITOR) -> std::result::Result<RgbaImage, String> {
    grab(Target::Monitor(hmon.0 as isize))
}

/// One frame of a target through a throw-away session (popups: rare, short-lived windows).
fn capture_once(target: Target) -> std::result::Result<RgbaImage, String> {
    let s = open(target)?;
    let deadline = std::time::Instant::now() + Duration::from_millis(800);
    let mut frame = None;
    while frame.is_none() && std::time::Instant::now() < deadline {
        frame = s.latest.lock().unwrap().1.take();
        if frame.is_none() {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
    let img = frame.ok_or_else(|| "no frame".to_string()).and_then(|f| frame_to_image(&f));
    let _ = s.session.Close();
    let _ = s.pool.Close();
    img
}

/// A window's own pixels, cropped to its visible frame (`frame` relative to `outer`, the
/// window rect WGC captures), with the app's popups above it (menus, drop-downs, tooltips:
/// separate, short-lived windows) composited on top.
pub fn capture_window(hwnd: HWND, outer: Rect, frame: Rect) -> std::result::Result<RgbaImage, String> {
    let target = Target::Window(hwnd.0 as isize);
    let img = match grab(target) {
        Ok(i) => i,
        Err(_) => {
            *CACHE.lock().unwrap() = None; // the window may have closed or changed; retry once fresh
            grab(target)?
        }
    };
    let x = (frame.left - outer.left).max(0) as u32;
    let y = (frame.top - outer.top).max(0) as u32;
    let w = (frame.width as u32).min(img.width().saturating_sub(x)).max(1);
    let h = (frame.height as u32).min(img.height().saturating_sub(y)).max(1);
    let mut img = imageops::crop_imm(&img, x, y, w, h).to_image();
    let z = crate::win::windows_z();
    let Some(pos) = z.iter().position(|&h| h == hwnd) else { return Ok(img) };
    let pid = crate::win::pid_of(hwnd);
    for &other in z[..pos].iter().rev() {
        // back to front
        if !crate::win::is_window(other) || crate::win::pid_of(other) != pid {
            continue;
        }
        let r = crate::win::window_rect(other);
        if r.width <= 0 || r.height <= 0 || r.left >= frame.left + frame.width || r.top >= frame.top + frame.height
            || r.left + r.width <= frame.left || r.top + r.height <= frame.top {
            continue;
        }
        if let Ok(part) = capture_once(Target::Window(other.0 as isize)) {
            imageops::overlay(&mut img, &part, (r.left - frame.left) as i64, (r.top - frame.top) as i64);
        }
    }
    Ok(img)
}

/// Downscale by `scale` (>= 1) to the RGB pixels that get encoded (and compared, to spot an
/// unchanged screen).
pub fn scaled(img: &RgbaImage, scale: f64) -> image::RgbImage {
    let rgb: image::RgbImage = image::buffer::ConvertBuffer::convert(img);
    if scale > 1.0 {
        imageops::resize(&rgb, (rgb.width() as f64 / scale).round() as u32, (rgb.height() as f64 / scale).round() as u32, imageops::FilterType::Triangle)
    } else {
        rgb
    }
}

/// Encode downscaled pixels as JPEG.
pub fn jpeg(rgb: &image::RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    let _ = enc.encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8);
    out
}
