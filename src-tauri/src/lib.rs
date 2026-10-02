//! Flick: a Flameshot-style screenshot tool for macOS.
//!
//! Flow: global hotkey -> CoreGraphics grabs the display under the cursor in-process (a few ms) ->
//! the image is shown natively by a layer *under* a transparent webview, so the overlay opens
//! pixel-identical to the live screen with no flash and no decode -> the webview draws only the
//! selection and annotations -> on commit the crop is cut from the native image, the annotation
//! layer (raw RGBA) is blended on top, and the result goes to the clipboard (and Desktop on ⌘S).

use std::{
    borrow::Cow,
    fs::OpenOptions,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{
    ipc::{InvokeBody, Request},
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    window::Color,
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// ⌘⇧2: unused by macOS and next to its own ⌘⇧3/4/5 screenshot keys. (⌘⇧A would be taken away
/// from every other app while Flick runs: Finder's Go to Applications, Chrome's tab search...)
const DEFAULT_SHORTCUT: &str = "Super+Shift+2";
/// Menu bar icon: black on transparent, drawn by `scripts/gen_tray_icon.php`. Used as a template
/// image so macOS tints it for light and dark menu bars.
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray-template.png");
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bounds for data coming back from the webview (defence in depth; it is our own page).
const MAX_DIMENSION: usize = 32_768;
const MAX_BYTES: usize = 512 * 1024 * 1024;
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The frozen screen while the overlay is open, and a guard against overlapping captures.
#[derive(Default)]
struct Shot {
    image: Mutex<Option<sys::Image>>,
    busy: AtomicBool,
}

impl Shot {
    fn image(&self) -> std::sync::MutexGuard<'_, Option<sys::Image>> {
        self.image.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A display in global points (origin top-left of the primary display).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

// ---------------------------------------------------------------- macOS FFI

#[cfg(target_os = "macos")]
mod sys {
    use super::Rect;
    use objc2::{
        class,
        encode::{Encode, Encoding},
        msg_send,
        runtime::AnyObject,
    };
    use std::{
        ffi::c_void,
        ptr::null_mut,
        sync::atomic::{AtomicPtr, Ordering},
    };
    use tauri::WebviewWindow;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }
    // SAFETY: the layouts above match CoreGraphics' CGPoint/CGSize/CGRect (two f64 each).
    unsafe impl Encode for CGPoint {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for CGSize {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for CGRect {
        const ENCODING: Encoding =
            Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
    }

    type CFType = *mut c_void;
    const ALPHA_PREMULTIPLIED_LAST: u32 = 1;
    const BYTE_ORDER_32_BIG: u32 = 4 << 12;
    const BLEND_COPY: i32 = 17;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
        fn CGEventCreate(source: *const c_void) -> CFType;
        fn CGEventGetLocation(event: *const c_void) -> CGPoint;
        fn CGGetDisplaysWithPoint(p: CGPoint, max: u32, ids: *mut u32, count: *mut u32) -> i32;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayBounds(display: u32) -> CGRect;
        // Marked obsolete in the macOS 15 SDK headers in favour of ScreenCaptureKit, but still
        // exported and working (macOS 26 included): ~5 ms in-process versus a `screencapture`
        // subprocess plus a PNG encode and decode. `screencapture` remains the fallback.
        fn CGDisplayCreateImage(display: u32) -> CFType;
        fn CGImageGetWidth(image: *const c_void) -> usize;
        fn CGImageGetHeight(image: *const c_void) -> usize;
        fn CGImageCreateWithImageInRect(image: *const c_void, rect: CGRect) -> CFType;
        fn CGImageRelease(image: *const c_void);
        static kCGColorSpaceSRGB: *const c_void;
        fn CGColorSpaceCreateWithName(name: *const c_void) -> CFType;
        fn CGColorSpaceRelease(space: *const c_void);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *const c_void,
            info: u32,
        ) -> CFType;
        fn CGContextSetBlendMode(ctx: *mut c_void, mode: i32);
        fn CGContextDrawImage(ctx: *mut c_void, rect: CGRect, image: *const c_void);
        fn CGContextRelease(ctx: *const c_void);
    }
    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        fn CGImageSourceCreateWithData(data: *const c_void, options: *const c_void) -> CFType;
        fn CGImageSourceCreateImageAtIndex(
            src: *const c_void,
            index: usize,
            options: *const c_void,
        ) -> CFType;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
        fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> CFType;
    }
    // CALayer / CATransaction are looked up by name at runtime; make sure QuartzCore is loaded.
    #[link(name = "QuartzCore", kind = "framework")]
    extern "C" {}

    /// An owned, immutable CGImage. CGImages are thread-safe, so it may cross threads.
    pub struct Image(CFType);
    // SAFETY: CGImageRef is immutable and documented as safe to use from any thread.
    unsafe impl Send for Image {}

    impl Drop for Image {
        fn drop(&mut self) {
            // SAFETY: we own one reference, created by a *Create* function.
            unsafe { CGImageRelease(self.0) }
        }
    }

    impl Image {
        fn from_owned(ptr: CFType) -> Option<Self> {
            (!ptr.is_null()).then_some(Self(ptr))
        }

        /// Size in pixels.
        pub fn size(&self) -> (usize, usize) {
            // SAFETY: self.0 is a live CGImage.
            unsafe { (CGImageGetWidth(self.0), CGImageGetHeight(self.0)) }
        }

        /// Decodes a PNG (the `screencapture` fallback path).
        pub fn from_png(png: &[u8]) -> Option<Self> {
            // SAFETY: every CF object created here is null-checked and released exactly once.
            unsafe {
                let data = CFDataCreate(std::ptr::null(), png.as_ptr(), png.len() as isize);
                if data.is_null() {
                    return None;
                }
                let src = CGImageSourceCreateWithData(data, std::ptr::null());
                CFRelease(data);
                if src.is_null() {
                    return None;
                }
                let image = CGImageSourceCreateImageAtIndex(src, 0, std::ptr::null());
                CFRelease(src);
                Self::from_owned(image)
            }
        }

        /// Pixels of `x, y, w, h` (image pixels) as straight sRGB RGBA, converted from the
        /// display's colour space exactly like the macOS screenshot tools do.
        pub fn rgba(&self, x: usize, y: usize, w: usize, h: usize) -> Option<Vec<u8>> {
            let mut buf = vec![0u8; w.checked_mul(h)?.checked_mul(4)?];
            let rect = CGRect {
                origin: CGPoint { x: x as f64, y: y as f64 },
                size: CGSize { width: w as f64, height: h as f64 },
            };
            let full = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: rect.size,
            };
            // SAFETY: `buf` outlives the bitmap context that writes into it and is exactly
            // w * h * 4 bytes with a w * 4 stride; all CF objects are null-checked and released.
            unsafe {
                let crop = CGImageCreateWithImageInRect(self.0, rect);
                if crop.is_null() {
                    return None;
                }
                let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
                let ctx = CGBitmapContextCreate(
                    buf.as_mut_ptr().cast(),
                    w,
                    h,
                    8,
                    w * 4,
                    space,
                    ALPHA_PREMULTIPLIED_LAST | BYTE_ORDER_32_BIG,
                );
                CGColorSpaceRelease(space);
                if !ctx.is_null() {
                    CGContextSetBlendMode(ctx, BLEND_COPY);
                    CGContextDrawImage(ctx, full, crop);
                    CGContextRelease(ctx);
                }
                CGImageRelease(crop);
                (!ctx.is_null()).then_some(buf)
            }
        }
    }

    pub fn has_screen_access() -> bool {
        // SAFETY: no arguments, no pointers; plain query.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    /// Triggers the system permission prompt (once) if access was never decided.
    pub fn request_screen_access() {
        // SAFETY: no arguments, no pointers.
        unsafe {
            CGRequestScreenCaptureAccess();
        }
    }

    /// The display under the mouse (falling back to the main one) and its bounds in global points.
    pub fn display_under_cursor() -> (u32, Rect) {
        // SAFETY: CGEventCreate(NULL) returns an owned event or NULL; we null-check and release it.
        // CGGetDisplaysWithPoint writes at most `max` (1) ids into our one-element buffer.
        unsafe {
            let mut id = CGMainDisplayID();
            let event = CGEventCreate(std::ptr::null());
            if !event.is_null() {
                let p = CGEventGetLocation(event);
                CFRelease(event);
                let (mut found, mut count) = (0u32, 0u32);
                if CGGetDisplaysWithPoint(p, 1, &mut found, &mut count) == 0 && count == 1 {
                    id = found;
                }
            }
            let b = CGDisplayBounds(id);
            (
                id,
                Rect { x: b.origin.x, y: b.origin.y, w: b.size.width, h: b.size.height },
            )
        }
    }

    pub fn grab(display: u32) -> Option<Image> {
        // SAFETY: returns an owned CGImage or NULL.
        Image::from_owned(unsafe { CGDisplayCreateImage(display) })
    }

    /// The first in-process capture pays a one-off setup cost (~0.5 s); pay it at launch instead
    /// of on the first hotkey press.
    pub fn warm_up() {
        if has_screen_access() {
            // SAFETY: plain query; the image is dropped (released) right away.
            drop(grab(unsafe { CGMainDisplayID() }));
        }
    }

    /// Layer-hosting view under the webview that shows the frozen screen. Created once at startup
    /// on the main thread and owned by the window's view hierarchy afterwards.
    static FROZEN_LAYER: AtomicPtr<AnyObject> = AtomicPtr::new(null_mut());

    /// Must run on the main thread.
    pub fn install_frozen_view(win: &WebviewWindow) {
        let Ok(ptr) = win.ns_window() else { return };
        if ptr.is_null() {
            return;
        }
        // SAFETY: `ptr` is the live NSWindow; its content view is wry's container holding the
        // WKWebView. We add a plain layer-hosting NSView (we own its layer, so setting the layer's
        // contents directly is supported) below every other subview. The superview retains it.
        unsafe {
            let w = &*(ptr as *const AnyObject);
            let parent: *mut AnyObject = msg_send![w, contentView];
            if parent.is_null() {
                return;
            }
            let bounds: CGRect = msg_send![parent, bounds];
            let view: *mut AnyObject = msg_send![class!(NSView), alloc];
            let view: *mut AnyObject = msg_send![view, initWithFrame: bounds];
            let layer: *mut AnyObject = msg_send![class!(CALayer), new];
            let _: () = msg_send![view, setLayer: layer];
            let _: () = msg_send![view, setWantsLayer: true];
            // NSViewWidthSizable | NSViewHeightSizable
            let _: () = msg_send![view, setAutoresizingMask: 2usize | 16usize];
            let nil: *mut AnyObject = null_mut();
            // NSWindowBelow = -1: under the webview.
            let _: () = msg_send![parent, addSubview: view, positioned: -1isize, relativeTo: nil];
            let _: () = msg_send![view, release];
            FROZEN_LAYER.store(layer, Ordering::Release);
            let _: () = msg_send![layer, release];
        }
    }

    /// Sets (or clears) the frozen screen without the implicit cross-fade animation.
    /// Must run on the main thread.
    pub fn set_frozen(image: Option<&Image>) {
        let layer = FROZEN_LAYER.load(Ordering::Acquire);
        if layer.is_null() {
            return;
        }
        let contents: *mut AnyObject = image.map_or(null_mut(), |i| i.0.cast());
        // SAFETY: `layer` is kept alive by the window's view hierarchy for the app's lifetime;
        // a CGImageRef is a valid `contents` object (CF types are toll-free bridged to id).
        unsafe {
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setDisableActions: true];
            let _: () = msg_send![layer, setContents: contents];
            let _: () = msg_send![class!(CATransaction), commit];
        }
    }

    /// Covers `rect` (global points), above the menu bar and Dock, on every Space and over
    /// full-screen apps. Must run on the main thread.
    pub fn place_over(win: &WebviewWindow, rect: Rect) {
        let Ok(ptr) = win.ns_window() else { return };
        if ptr.is_null() {
            return;
        }
        // Cocoa's global coordinates have their origin at the bottom-left of the main display.
        // SAFETY: plain query.
        let main_h = unsafe { CGDisplayBounds(CGMainDisplayID()).size.height };
        let frame = CGRect {
            origin: CGPoint { x: rect.x, y: main_h - (rect.y + rect.h) },
            size: CGSize { width: rect.w, height: rect.h },
        };
        // SAFETY: `ptr` is the live NSWindow backing `win`; all selectors exist on NSWindow.
        unsafe {
            let w = &*(ptr as *const AnyObject);
            let _: () = msg_send![w, setLevel: 1001isize];
            let _: () = msg_send![w, setCollectionBehavior: (1usize << 0) | (1usize << 8)];
            let _: () = msg_send![w, setFrame: frame, display: false];
        }
    }

    /// Shows the crosshair right away, like ⌘⇧4, instead of waiting for the first mouse move.
    pub fn crosshair() {
        // SAFETY: class method returning a shared cursor; `set` takes no arguments.
        unsafe {
            let c: *mut AnyObject = msg_send![class!(NSCursor), crosshairCursor];
            if !c.is_null() {
                let _: () = msg_send![c, set];
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    use super::Rect;
    use tauri::WebviewWindow;
    pub struct Image;
    impl Image {
        pub fn size(&self) -> (usize, usize) {
            (0, 0)
        }
        pub fn from_png(_: &[u8]) -> Option<Self> {
            None
        }
        pub fn rgba(&self, _: usize, _: usize, _: usize, _: usize) -> Option<Vec<u8>> {
            None
        }
    }
    pub fn has_screen_access() -> bool {
        true
    }
    pub fn request_screen_access() {}
    pub fn display_under_cursor() -> (u32, Rect) {
        (0, Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 })
    }
    pub fn grab(_: u32) -> Option<Image> {
        None
    }
    pub fn warm_up() {}
    pub fn install_frozen_view(_: &WebviewWindow) {}
    pub fn set_frozen(_: Option<&Image>) {}
    pub fn place_over(_: &WebviewWindow, _: Rect) {}
    pub fn crosshair() {}
}

// ---------------------------------------------------------------- config

fn config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_default()
        .join("Flick")
        .join("config.json")
}

/// Hotkey from `~/Library/Application Support/Flick/config.json`, e.g. `{"shortcut":"Super+Shift+X"}`.
fn configured_shortcut() -> String {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["shortcut"].as_str().map(str::to_owned))
        .unwrap_or_else(|| DEFAULT_SHORTCUT.to_owned())
}

// ---------------------------------------------------------------- capture

/// Fallback when the in-process grab is unavailable: `screencapture` into a private temp file.
fn run_screencapture(rect: Rect) -> Result<sys::Image, String> {
    // Private (0600) unique temp file, removed on drop even on error paths.
    let tmp = tempfile::Builder::new()
        .prefix("flick-")
        .suffix(".png")
        .tempfile()
        .map_err(|e| e.to_string())?;
    let mut child = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-t", "png"])
        .arg(format!(
            "-R{},{},{},{}",
            rect.x.round() as i64,
            rect.y.round() as i64,
            rect.w.round() as i64,
            rect.h.round() as i64
        ))
        .arg(tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run screencapture: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => break,
            Some(status) => return Err(format!("screencapture failed: {status}")),
            None if started.elapsed() > CAPTURE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("screencapture timed out".into());
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let png = std::fs::read(tmp.path()).map_err(|e| e.to_string())?;
    if png.len() > MAX_BYTES || !png.starts_with(&PNG_MAGIC) {
        return Err("screencapture produced no valid PNG".into());
    }
    sys::Image::from_png(&png).ok_or_else(|| "cannot decode screenshot".into())
}

fn capture_inner(app: &AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("overlay")
        .ok_or("overlay window missing")?;
    if win.is_visible().unwrap_or(false) {
        return Err("overlay already open".into());
    }
    if !sys::has_screen_access() {
        sys::request_screen_access();
        return Err("Screen Recording permission missing".into());
    }
    let (display, rect) = sys::display_under_cursor();
    if rect.w < 1.0 || rect.h < 1.0 {
        return Err("no display found".into());
    }
    let image = match sys::grab(display) {
        Some(image) => image,
        None => run_screencapture(rect)?,
    };
    let size = image.size();
    *app.state::<Shot>().image() = Some(image);

    let shown = win.clone();
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        // The frozen screen is in place before the window is ordered in, so its very first frame
        // already matches the screen: nothing flashes and nothing jumps.
        sys::set_frozen(app2.state::<Shot>().image().as_ref());
        sys::place_over(&shown, rect);
        let _ = shown.emit("shot", size);
        let _ = shown.show();
        let _ = shown.set_focus();
        sys::crosshair();
    })
    .map_err(|e| e.to_string())
}

fn capture(app: &AppHandle) {
    if app.state::<Shot>().busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    // Never block the event loop on the capture.
    std::thread::spawn(move || {
        if let Err(e) = capture_inner(&app) {
            eprintln!("flick: {e}");
            *app.state::<Shot>().image() = None;
            app.state::<Shot>().busy.store(false, Ordering::Release);
        }
    });
}

// ---------------------------------------------------------------- output

/// Checks that `len` is exactly a `width` x `height` RGBA buffer within sane limits.
fn validate_rgba(width: usize, height: usize, len: usize) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(format!("bad image size {width}x{height}"));
    }
    match width.checked_mul(height).and_then(|p| p.checked_mul(4)) {
        Some(expected) if expected == len && len <= MAX_BYTES => Ok(()),
        _ => Err("pixel buffer does not match dimensions".into()),
    }
}

/// Checks that the crop `x, y, w, h` lies inside an image of `size`.
fn validate_crop(size: (usize, usize), x: usize, y: usize, w: usize, h: usize) -> Result<(), String> {
    let inside = |o: usize, l: usize, max: usize| l > 0 && o.checked_add(l).is_some_and(|e| e <= max);
    if inside(x, w, size.0) && inside(y, h, size.1) {
        Ok(())
    } else {
        Err(format!("crop {x},{y} {w}x{h} outside {}x{}", size.0, size.1))
    }
}

/// Source-over blend of straight-alpha RGBA `top` onto opaque RGBA `base`, in place.
fn blend_over(base: &mut [u8], top: &[u8]) {
    for (b, t) in base.chunks_exact_mut(4).zip(top.chunks_exact(4)) {
        let a = t[3] as u32;
        if a == 0 {
            continue;
        }
        for i in 0..3 {
            b[i] = ((t[i] as u32 * a + b[i] as u32 * (255 - a) + 127) / 255) as u8;
        }
        b[3] = 255;
    }
}

fn encode_png(rgba: &[u8], w: usize, h: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Saves to the Desktop without ever overwriting an existing file.
fn save_to_desktop(png: &[u8]) -> Result<String, String> {
    let stamp = Command::new("/bin/date")
        .arg("+%Y-%m-%d at %H.%M.%S")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "shot".into());
    let dir = dirs::desktop_dir().ok_or("no Desktop folder")?;
    save_unique(&dir, &stamp, png)
}

fn save_unique(dir: &Path, stamp: &str, png: &[u8]) -> Result<String, String> {
    for n in 0..1000 {
        let name = if n == 0 {
            format!("Flick {stamp}.png")
        } else {
            format!("Flick {stamp} ({n}).png")
        };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                f.write_all(png).map_err(|e| e.to_string())?;
                return Ok(path.display().to_string());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("could not find a free file name".into())
}

// ---------------------------------------------------------------- IPC commands

fn header_usize(request: &Request<'_>, name: &str) -> Result<usize, String> {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("missing or invalid header {name}"))
}

/// Pixel size of the frozen screen, for a page that loaded after the `shot` event was sent.
#[tauri::command]
async fn shot_info(state: State<'_, Shot>) -> Result<(usize, usize), String> {
    state
        .image()
        .as_ref()
        .map(sys::Image::size)
        .ok_or_else(|| "no screenshot pending".to_owned())
}

/// Commits the selection. Headers: `x-x`, `x-y`, `x-width`, `x-height` (crop in screenshot
/// pixels), `x-save` (1 = also save to the Desktop). Body: the annotation layer as straight RGBA of
/// the crop size, or empty when there are no annotations. Returns the saved path, if any.
#[tauri::command]
async fn finish(request: Request<'_>, state: State<'_, Shot>) -> Result<Option<String>, String> {
    let (x, y, w, h) = (
        header_usize(&request, "x-x")?,
        header_usize(&request, "x-y")?,
        header_usize(&request, "x-width")?,
        header_usize(&request, "x-height")?,
    );
    let save = header_usize(&request, "x-save")? == 1;
    let annotations: &[u8] = match request.body() {
        InvokeBody::Raw(bytes) => bytes,
        InvokeBody::Json(_) => &[],
    };

    let mut pixels = {
        let guard = state.image();
        let image = guard.as_ref().ok_or("no screenshot pending")?;
        validate_crop(image.size(), x, y, w, h)?;
        validate_rgba(w, h, w * h * 4)?;
        image.rgba(x, y, w, h).ok_or("cannot read screenshot pixels")?
    };
    if !annotations.is_empty() {
        validate_rgba(w, h, annotations.len())?;
        blend_over(&mut pixels, annotations);
    }

    arboard::Clipboard::new()
        .and_then(|mut c| {
            c.set_image(arboard::ImageData {
                width: w,
                height: h,
                bytes: Cow::Borrowed(&pixels),
            })
        })
        .map_err(|e| format!("copy failed: {e}"))?;
    if save {
        let png = encode_png(&pixels, w, h)?;
        return save_to_desktop(&png).map(Some).map_err(|e| format!("save failed: {e}"));
    }
    Ok(None)
}

#[tauri::command]
async fn hide_overlay(app: AppHandle) -> Result<(), String> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if let Some(w) = handle.get_webview_window("overlay") {
            let _ = w.hide();
        }
        // Only after the window is gone, so the frozen screen never visibly disappears.
        sys::set_frozen(None);
        let state = handle.state::<Shot>();
        *state.image() = None;
        state.busy.store(false, Ordering::Release);
    })
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- app

pub fn run() {
    tauri::Builder::default()
        // Must be first. Launching Flick again just triggers a capture.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| capture(app)))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(Shot::default())
        .invoke_handler(tauri::generate_handler![shot_info, finish, hide_overlay])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let overlay =
                WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("index.html".into()))
                    .decorations(false)
                    .resizable(false)
                    .always_on_top(true)
                    .skip_taskbar(true)
                    .shadow(false)
                    // The webview draws no background: the frozen screen underneath shows through.
                    .background_color(Color(0, 0, 0, 0))
                    .visible(false)
                    .build()?;
            sys::install_frozen_view(&overlay);
            std::thread::spawn(sys::warm_up);

            let requested = configured_shortcut();
            let shortcut: Shortcut = requested.parse().unwrap_or_else(|_| {
                eprintln!("flick: invalid shortcut {requested:?}, using {DEFAULT_SHORTCUT}");
                DEFAULT_SHORTCUT.parse().expect("default shortcut is valid")
            });
            // A hotkey owned by another app must not stop Flick from starting; the tray still works.
            let hotkey_label = match app.global_shortcut().on_shortcut(shortcut, |app, _, ev| {
                if ev.state() == ShortcutState::Pressed {
                    capture(app);
                }
            }) {
                Ok(()) => shortcut.into_string(),
                Err(e) => {
                    eprintln!("flick: cannot register hotkey: {e}");
                    "hotkey unavailable".to_owned()
                }
            };

            let capture_item = MenuItem::with_id(
                app,
                "capture",
                format!("Capture  ({hotkey_label})"),
                true,
                None::<&str>,
            )?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Flick", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&capture_item, &quit_item])?;
            TrayIconBuilder::new()
                .icon(tauri::image::Image::from_bytes(TRAY_ICON)?)
                .icon_as_template(true)
                .tooltip("Flick")
                .menu(&menu)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "capture" => {
                        let app = app.clone();
                        // Let the tray menu close before grabbing the screen.
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(250));
                            capture(&app);
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Flick");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_validation_accepts_exact_buffers() {
        assert!(validate_rgba(10, 5, 10 * 5 * 4).is_ok());
    }

    #[test]
    fn rgba_validation_rejects_bad_input() {
        assert!(validate_rgba(0, 5, 0).is_err());
        assert!(validate_rgba(10, 5, 10 * 5 * 4 - 1).is_err());
        assert!(validate_rgba(MAX_DIMENSION + 1, 1, (MAX_DIMENSION + 1) * 4).is_err());
        assert!(validate_rgba(usize::MAX, usize::MAX, 0).is_err());
    }

    #[test]
    fn crop_must_lie_inside_the_screenshot() {
        assert!(validate_crop((100, 50), 0, 0, 100, 50).is_ok());
        assert!(validate_crop((100, 50), 90, 40, 10, 10).is_ok());
        assert!(validate_crop((100, 50), 91, 0, 10, 10).is_err());
        assert!(validate_crop((100, 50), 0, 0, 0, 10).is_err());
        assert!(validate_crop((100, 50), usize::MAX, 0, 2, 10).is_err());
    }

    #[test]
    fn blending_respects_alpha() {
        let mut base = [10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255];
        let top = [200, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 128];
        blend_over(&mut base, &top);
        assert_eq!(&base[0..4], &[10, 20, 30, 255], "transparent leaves the screen as is");
        assert_eq!(&base[4..8], &[255, 255, 255, 255], "opaque replaces it");
        assert_eq!(&base[8..12], &[5, 10, 15, 255], "half alpha mixes");
    }

    #[test]
    fn png_round_trips() {
        let rgba = [1, 2, 3, 255, 4, 5, 6, 128];
        let png = encode_png(&rgba, 2, 1).unwrap();
        assert!(png.starts_with(&PNG_MAGIC));
        let mut reader = png::Decoder::new(&png[..]).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(buf, rgba);
    }

    #[test]
    fn saving_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let a = save_unique(dir.path(), "t", b"one").unwrap();
        let b = save_unique(dir.path(), "t", b"two").unwrap();
        assert_ne!(a, b);
        assert!(b.ends_with("Flick t (1).png"));
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
    }

    #[test]
    fn default_shortcut_parses() {
        assert!(DEFAULT_SHORTCUT.parse::<Shortcut>().is_ok());
    }

    #[test]
    fn default_shortcut_is_not_cmd_shift_a() {
        use tauri_plugin_global_shortcut::{Code, Modifiers};
        let s: Shortcut = DEFAULT_SHORTCUT.parse().unwrap();
        assert_eq!(s, Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Digit2));
    }

    #[test]
    fn tray_icon_is_a_template_image() {
        let icon = tauri::image::Image::from_bytes(TRAY_ICON).expect("tray icon decodes");
        assert_eq!((icon.width(), icon.height()), (44, 44));
        // Template images are pure black with alpha; any colour would show untinted.
        assert!(icon
            .rgba()
            .chunks(4)
            .all(|p| p[3] == 0 || (p[0] == 0 && p[1] == 0 && p[2] == 0)));
        assert!(icon.rgba().chunks(4).any(|p| p[3] > 200), "icon is not empty");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn png_fallback_decodes_and_crops_to_srgb() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 9, 9, 9, 255];
        let png = encode_png(&rgba, 2, 2).unwrap();
        let image = sys::Image::from_png(&png).expect("decodes");
        assert_eq!(image.size(), (2, 2));
        assert_eq!(image.rgba(1, 1, 1, 1).unwrap(), vec![9, 9, 9, 255]);
        assert_eq!(image.rgba(0, 0, 2, 2).unwrap(), rgba);
    }
}
