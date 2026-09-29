fn main() {
    // Only the listed app commands exist for the webview; each needs an explicit capability grant.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "take_shot",
            "copy_image",
            "save_image",
            "hide_overlay",
        ]),
    ))
    .expect("failed to run tauri-build");
}
