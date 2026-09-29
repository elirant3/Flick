# Flick

A Flameshot-style screenshot tool for macOS. Built with Rust + Tauri.

Press **⌘⇧A**, drag a region, and it's on your clipboard. Annotate if you want — the clipboard updates after every change.

## Tools

Pen (P) · Line (L) · Arrow (A) · Rectangle (R) · Circle (E) · Marker (M) · Text (T) — 7 colors, 3 sizes.

| Key | Action |
| --- | --- |
| Enter / ⌘C | Copy and close |
| ⌘S | Save PNG to Desktop and close |
| ⌘Z | Undo |
| Esc / right-click | Cancel |
| Shift (while drawing) | Snap lines and arrows to 45° |

## Install

```sh
npm install
./scripts/install.sh
```

The script builds, code-signs, installs to `/Applications` and launches. Set `FLICK_SIGN_ID` to your own signing identity (`security find-identity -v -p codesigning`); a stable signature is what lets macOS remember the Screen Recording permission across rebuilds.

On first capture, allow Flick under System Settings → Privacy & Security → Screen Recording, then relaunch.

## Config

Change the hotkey in `~/Library/Application Support/Flick/config.json`:

```json
{ "shortcut": "Super+Shift+X" }
```

## Limitations

Main display only, no launch-at-login yet.

## License

GPL-3.0
