//! Flick: a Flameshot-style screenshot tool for macOS.
//!
//! Flow: global hotkey -> `screencapture` grabs the display under the cursor into a private temp
//! file -> the PNG is held in memory and handed to the overlay window as a raw binary IPC
//! response (no base64) -> the overlay crops and annotates -> the result comes back as raw
//! RGBA (clipboard) or PNG bytes (save).

use std::{
    borrow::Cow,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{
    ipc::{InvokeBody, Request, Response},
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
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

/// The screenshot waiting to be picked up by the overlay, and a guard against overlapping captures.
#[derive(Default)]
struct Shot {
    png: Mutex<Option<Vec<u8>>>,
    busy: AtomicBool,
}

impl Shot {
    fn png(&self) -> std::sync::MutexGuard<'_, Option<Vec<u8>>> {
        self.png.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// ---------------------------------------------------------------- macOS FFI

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::c_void;
    use tauri::WebviewWindow;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        fn CGEventGetLocation(event: *const c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
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

    /// Cursor position in global display points (origin top-left of the primary display), the
    /// same space `screencapture -R` and `CGDisplayBounds` use.
    pub fn cursor() -> Option<(f64, f64)> {
        // SAFETY: CGEventCreate(NULL) returns an owned event or NULL; we null-check and release it.
        unsafe {
            let event = CGEventCreate(std::ptr::null());
            if event.is_null() {
                return None;
            }
            let p = CGEventGetLocation(event);
            CFRelease(event);
            Some((p.x, p.y))
        }
    }

    /// Put the overlay above the menu bar and Dock, on every Space and over full-screen apps.
    /// Must run on the main thread.
    pub fn float_above_everything(win: &WebviewWindow) {
        use objc2::{msg_send, runtime::AnyObject};
        let Ok(ptr) = win.ns_window() else { return };
        if ptr.is_null() {
            return;
        }
        // SAFETY: `ptr` is the live NSWindow backing `win`; both selectors exist on NSWindow and
        // take NSInteger / NSUInteger. Called on the main thread by the callers.
        unsafe {
            let w = &*(ptr as *const AnyObject);
            let _: () = msg_send![w, setLevel: 1001isize];
            let _: () = msg_send![w, setCollectionBehavior: (1usize << 0) | (1usize << 8)];
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    use tauri::WebviewWindow;
    pub fn has_screen_access() -> bool {
        true
    }
    pub fn request_screen_access() {}
    pub fn cursor() -> Option<(f64, f64)> {
        None
    }
    pub fn float_above_everything(_: &WebviewWindow) {}
}

// ---------------------------------------------------------------- config

fn config_path() -> PathBuf {
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

/// Rectangle (in global points) of the monitor under the cursor, falling back to the primary one.
fn target_rect(win: &WebviewWindow) -> Result<(f64, f64, f64, f64), String> {
    let rects: Vec<_> = win
        .available_monitors()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|m| {
            let s = m.scale_factor();
            let p = m.position().to_logical::<f64>(s);
            let z = m.size().to_logical::<f64>(s);
            (p.x, p.y, z.width, z.height)
        })
        .collect();
    let under_cursor = sys::cursor().and_then(|(cx, cy)| {
        rects
            .iter()
            .copied()
            .find(|&(x, y, w, h)| cx >= x && cx < x + w && cy >= y && cy < y + h)
    });
    if let Some(r) = under_cursor {
        return Ok(r);
    }
    let primary = win.primary_monitor().map_err(|e| e.to_string())?;
    primary
        .map(|m| {
            let s = m.scale_factor();
            let p = m.position().to_logical::<f64>(s);
            let z = m.size().to_logical::<f64>(s);
            (p.x, p.y, z.width, z.height)
        })
        .or_else(|| rects.first().copied())
        .ok_or_else(|| "no monitor found".to_owned())
}

fn run_screencapture(rect: (f64, f64, f64, f64), out: &Path) -> Result<(), String> {
    let (x, y, w, h) = rect;
    let mut child = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-t", "png"])
        .arg(format!(
            "-R{},{},{},{}",
            x.round() as i64,
            y.round() as i64,
            w.round() as i64,
            h.round() as i64
        ))
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run screencapture: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => return Err(format!("screencapture failed: {status}")),
            None if started.elapsed() > CAPTURE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("screencapture timed out".into());
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
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
    let rect = target_rect(&win)?;

    // Private (0600) unique temp file, removed on drop even on error paths.
    let tmp = tempfile::Builder::new()
        .prefix("flick-")
        .suffix(".png")
        .tempfile()
        .map_err(|e| e.to_string())?;
    run_screencapture(rect, tmp.path())?;
    let png = std::fs::read(tmp.path()).map_err(|e| e.to_string())?;
    drop(tmp);
    if png.len() > MAX_BYTES || !png.starts_with(&PNG_MAGIC) {
        return Err("screencapture produced no valid PNG".into());
    }
    *app.state::<Shot>().png() = Some(png);

    let (x, y, w, h) = rect;
    let _ = win.set_position(LogicalPosition::new(x, y));
    let _ = win.set_size(LogicalSize::new(w, h));
    win.emit("shot", ()).map_err(|e| e.to_string())?;
    let shown = win.clone();
    app.run_on_main_thread(move || {
        sys::float_above_everything(&shown);
        let _ = shown.show();
        let _ = shown.set_focus();
    })
    .map_err(|e| e.to_string())
}

fn capture(app: &AppHandle) {
    if app.state::<Shot>().busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    // Never block the event loop on a subprocess.
    std::thread::spawn(move || {
        if let Err(e) = capture_inner(&app) {
            eprintln!("flick: {e}");
            app.state::<Shot>().busy.store(false, Ordering::Release);
        }
    });
}

// ---------------------------------------------------------------- IPC commands

fn raw_body<'a>(request: &'a Request<'_>) -> Result<&'a [u8], String> {
    match request.body() {
        InvokeBody::Raw(bytes) => Ok(bytes),
        InvokeBody::Json(_) => Err("expected a raw binary body".into()),
    }
}

fn header_usize(request: &Request<'_>, name: &str) -> Result<usize, String> {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("missing or invalid header {name}"))
}

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

/// Hands the pending screenshot (raw PNG bytes) to the overlay, freeing our copy.
#[tauri::command]
async fn take_shot(state: State<'_, Shot>) -> Result<Response, String> {
    state
        .png()
        .take()
        .map(Response::new)
        .ok_or_else(|| "no screenshot pending".to_owned())
}

/// Body: raw RGBA pixels. Headers: `x-width`, `x-height`.
#[tauri::command]
async fn copy_image(request: Request<'_>) -> Result<(), String> {
    let (w, h) = (
        header_usize(&request, "x-width")?,
        header_usize(&request, "x-height")?,
    );
    let rgba = raw_body(&request)?;
    validate_rgba(w, h, rgba.len())?;
    arboard::Clipboard::new()
        .and_then(|mut c| {
            c.set_image(arboard::ImageData {
                width: w,
                height: h,
                bytes: Cow::Borrowed(rgba),
            })
        })
        .map_err(|e| e.to_string())
}

/// Body: raw PNG bytes. Saved to the Desktop without ever overwriting an existing file.
#[tauri::command]
async fn save_image(request: Request<'_>) -> Result<String, String> {
    let png = raw_body(&request)?;
    if png.len() > MAX_BYTES || !png.starts_with(&PNG_MAGIC) {
        return Err("not a PNG".into());
    }
    let stamp = Command::new("/bin/date")
        .arg("+%Y-%m-%d at %H.%M.%S")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "shot".into());
    let dir = dirs::desktop_dir().ok_or("no Desktop folder")?;
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

#[tauri::command]
async fn hide_overlay(app: AppHandle, state: State<'_, Shot>) -> Result<(), String> {
    *state.png() = None;
    state.busy.store(false, Ordering::Release);
    match app.get_webview_window("overlay") {
        Some(w) => w.hide().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------- app

pub fn run() {
    tauri::Builder::default()
        // Must be first. Launching Flick again just triggers a capture.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| capture(app)))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(Shot::default())
        .invoke_handler(tauri::generate_handler![
            take_shot,
            copy_image,
            save_image,
            hide_overlay
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("index.html".into()))
                .decorations(false)
                .resizable(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .shadow(false)
                .visible(false)
                .build()?;

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

    #[test]
    fn png_magic_is_checked() {
        assert!(PNG_MAGIC.starts_with(&[0x89, b'P']));
        assert!(!b"GIF89a".starts_with(&PNG_MAGIC));
    }
}
