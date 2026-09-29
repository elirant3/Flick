# Flick

A Flameshot-style screenshot tool for macOS. Built with Rust + Tauri.

Press **⌘⇧2**, drag a region, annotate if you want, and press **Enter**: it's on your clipboard. Cancelling never touches the clipboard.

## Tools

Pen (P) · Line (L) · Arrow (A) · Rectangle (R) · Circle (E) · Marker (M) · Text (T), with 7 colors and 3 sizes.

| Key | Action |
| --- | --- |
| Enter / ⌘C | Copy and close |
| ⌘S | Save PNG to Desktop, copy, and close |
| ⌘Z | Undo |
| Esc / right-click / switching apps | Cancel (the clipboard is left as it was) |
| Shift (while drawing) | Snap lines and arrows to 45° |

Captures the display your cursor is on. Launching Flick a second time also triggers a capture.

## Install

```sh
npm install
./scripts/install.sh
```

The script builds, code-signs, installs to `/Applications` and launches. A stable signature is what lets macOS remember the Screen Recording permission across rebuilds, so put your identity in `scripts/.signing` (git-ignored) or the environment:

```sh
export FLICK_SIGN_ID="Apple Development: you@example.com (TEAMID)"   # see: security find-identity -v -p codesigning
```

On first capture, allow Flick under System Settings → Privacy & Security → Screen Recording, then relaunch.

## Config

The default hotkey is ⌘⇧2: macOS doesn't use it, and it sits next to the system's own ⌘⇧3/4/5. (A global hotkey is taken away from every other app while Flick runs, which is why it isn't ⌘⇧A: that belongs to Finder's Go to Applications and Chrome's tab search.) Change it in `~/Library/Application Support/Flick/config.json`:

```json
{ "shortcut": "Super+Shift+X" }
```

If the hotkey is taken by another app Flick still starts; use the menu bar item to capture.

## Design notes

- **Memory:** the screenshot moves to the overlay as a raw binary IPC response (no base64/JSON), the full-screen bitmap is freed on close, and the pixel buffer for the clipboard is released immediately after sending. Nothing is cached on disk.
- **Clipboard on commit only:** drawing never exports anything; the crop is rendered once when you press Enter, ⌘C or ⌘S (⌘S reuses that one render for the file and the clipboard).
- **Menu bar icon:** a black-on-transparent template image (`scripts/gen_tray_icon.php`), so macOS tints it for light and dark menu bars.
- **Temp files:** the capture goes to a private (0600) unique temp file that is deleted right away, including on error paths.
- **Least privilege:** strict CSP (no inline script/style, no remote origins), frozen prototypes, and the overlay window may only call four app commands plus event listening. The webview can't touch the filesystem or shell.
- **Input validation:** clipboard/save payloads are size-checked (`width × height × 4` must match exactly, 32 768 px and 512 MB caps); saves never overwrite an existing file.
- **Robustness:** no work on the UI thread (capture runs in a worker with a 5 s timeout), overlapping captures are ignored, AppKit calls run on the main thread, the overlay closes if it loses focus, and a missing Screen Recording permission is detected up front instead of capturing a blank wallpaper.

## License

GPL-3.0
