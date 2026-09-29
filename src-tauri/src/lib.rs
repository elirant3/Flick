use base64::{engine::general_purpose::STANDARD, Engine};
use std::path::PathBuf;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

const DEFAULT_SHORTCUT: &str = "Super+Shift+A";

fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("Flick").join("config.json")
}

/// Shortcut string from ~/Library/Application Support/Flick/config.json, e.g. {"shortcut":"Super+Shift+X"}.
fn configured_shortcut() -> String {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["shortcut"].as_str().map(str::to_owned))
        .unwrap_or_else(|| DEFAULT_SHORTCUT.to_owned())
}

#[cfg(target_os = "macos")]
fn float_above_everything(win: &WebviewWindow) {
    use objc2::{msg_send, runtime::AnyObject};
    if let Ok(ptr) = win.ns_window() {
        // Level above the menu bar and Dock; join every Space and full-screen apps.
        unsafe {
            let w = &*(ptr as *const AnyObject);
            let _: () = msg_send![w, setLevel: 1001isize];
            let _: () = msg_send![w, setCollectionBehavior: (1usize << 0) | (1usize << 8)];
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn float_above_everything(_: &WebviewWindow) {}

fn capture(app: &AppHandle) {
    let Some(win) = app.get_webview_window("overlay") else { return };
    if win.is_visible().unwrap_or(false) {
        return;
    }
    let tmp = std::env::temp_dir().join("flick-capture.png");
    let ok = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-t", "png"])
        .arg(&tmp)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let Ok(bytes) = std::fs::read(&tmp) else {
        eprintln!("capture failed (ok={ok})");
        return;
    };
    let _ = std::fs::remove_file(&tmp);

    if let Ok(Some(mon)) = win.primary_monitor() {
        let scale = mon.scale_factor();
        let pos = mon.position().to_logical::<f64>(scale);
        let size = mon.size().to_logical::<f64>(scale);
        let _ = win.set_position(pos);
        let _ = win.set_size(size);
    }
    let _ = win.emit("shot", format!("data:image/png;base64,{}", STANDARD.encode(bytes)));
    float_above_everything(&win);
    let _ = win.show();
    let _ = win.set_focus();
}

#[tauri::command]
fn copy_image(data: String) -> Result<(), String> {
    let png = STANDARD.decode(data).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&png).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = img.dimensions();
    arboard::Clipboard::new()
        .and_then(|mut c| {
            c.set_image(arboard::ImageData {
                width: w as usize,
                height: h as usize,
                bytes: img.into_raw().into(),
            })
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn save_image(data: String) -> Result<String, String> {
    let png = STANDARD.decode(data).map_err(|e| e.to_string())?;
    let stamp = std::process::Command::new("date")
        .arg("+%Y-%m-%d at %H.%M.%S")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| "shot".into());
    let path = dirs::desktop_dir().unwrap_or_default().join(format!("Flick {stamp}.png"));
    std::fs::write(&path, png).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

#[tauri::command]
fn hide_overlay(app: AppHandle) {
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.hide();
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![copy_image, save_image, hide_overlay])
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

            let shortcut_str = configured_shortcut();
            let shortcut: Shortcut = shortcut_str.parse().unwrap_or_else(|_| DEFAULT_SHORTCUT.parse().unwrap());
            app.global_shortcut().on_shortcut(shortcut, |app, _, ev| {
                if ev.state() == ShortcutState::Pressed {
                    capture(app);
                }
            })?;

            let capture_item = MenuItem::with_id(app, "capture", format!("Capture  ({shortcut_str})"), true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Flick", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&capture_item, &quit_item])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "capture" => {
                        let app = app.clone();
                        // Let the tray menu close before grabbing the screen.
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(250));
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
