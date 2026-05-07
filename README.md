# shotpaste

> **One screenshot, three pastes.**

shotpaste copies your screenshots to the clipboard as **image**, **file**, *and* **text-path** — all at once, in a single clipboard write. Then one `Ctrl+V` (or `Cmd+V`) does the right thing in any app:

- **Image-aware apps** (Slack, WhatsApp, Discord, image editors) → paste the image.
- **File-aware contexts** (file upload zones, file managers, JIRA attachments) → paste the file.
- **Text fields** (terminals, editors, markdown previews) → paste the path.

No more screenshotting twice, no more drag-from-Explorer, no more switching tools depending on where you're pasting.

---

## Install

> Project is in active development — install instructions will land here once the first release ships.

```sh
# macOS / Linux
curl -fsSL https://github.com/ebrahim-sameh/shotpaste/releases/latest/download/shotpaste-installer.sh | sh
shotpaste install
```

```powershell
# Windows
irm https://github.com/ebrahim-sameh/shotpaste/releases/latest/download/shotpaste-installer.ps1 | iex
shotpaste install
```

The first line drops a `shotpaste` binary into `~/.cargo/bin` (Linux/macOS) or `%LOCALAPPDATA%\Programs\shotpaste\` (Windows). The second registers it to start at login. Verify with `shotpaste status`.

## Quickstart

1. Press your normal screenshot shortcut (Win+PrtScn, Cmd+Shift+3, PrtScn).
2. Paste anywhere.
3. That's it.

## How it works

shotpaste runs as a tiny background daemon that watches your screenshots folder. When a new PNG appears, it builds a single clipboard entry that advertises three formats simultaneously — image bytes, a file-drop list, and the file path as text — using each OS's native multi-format clipboard API:

- **Windows**: `IDataObject` with `CF_BITMAP` + `CF_HDROP` + `CF_UNICODETEXT`
- **macOS**: `NSPasteboard.writeObjects:` with `NSPasteboardTypePNG` + `NSPasteboardTypeFileURL` + `NSPasteboardTypeString`
- **Linux**: Wayland `wl_data_source` with `image/png` + `text/uri-list` + `text/plain;charset=utf-8`, or X11 selection ownership advertising the same MIMEs

It's about 1 MB of compiled Rust, with no runtime dependencies on any platform.

## Why not just use ShareX / Greenshot / CopyCut?

Existing screenshot tools give you EITHER an image OR a path on the clipboard. shotpaste is the first to give you all three formats from a single capture.

| Tool | Image paste | File-drop paste | Path-text paste | All three at once | Cross-platform |
|---|---|---|---|---|---|
| **shotpaste** | ✓ | ✓ | ✓ | **✓** | Win + macOS + Linux |
| ShareX | ✓ | ✓ (separate action) | ✓ (separate action) | ✗ | Windows |
| Greenshot / Flameshot / Lightshot | ✓ | ✗ | ✗ | ✗ | varies |
| CopyCut / winclipshot | ✗ | ✗ | ✓ | ✗ | Windows |
| Snagit | ✓ | ✗ | ✗ | ✗ | Win + macOS (paid) |
| Snipping Tool / macOS Screenshot | ✓ | ✗ | ✗ | ✗ | native |

## Configuration

Config file at `~/.config/shotpaste/config.toml` (created on first install). You can:

- Change the watched folder (default: your OS screenshot folder)
- Toggle individual clipboard formats
- Set custom log level

## Roadmap

Considering for future releases:

- Custom watch folders (multiple dirs, OneDrive, Dropbox)
- Per-format toggles
- Optional auto-upload (imgur / 0x0.st) as a 4th clipboard format
- OCR text format (paste OCR'd text into editors)
- Filename templates

## Uninstall

```sh
# macOS / Linux
shotpaste uninstall
curl -fsSL https://github.com/ebrahim-sameh/shotpaste/releases/latest/download/shotpaste-installer.sh | sh -s -- --uninstall
```

```powershell
# Windows
shotpaste uninstall
# To remove the binary too, delete %LOCALAPPDATA%\Programs\shotpaste\
```

## Contributing

Issues and PRs welcome. Run `cargo test` and `cargo clippy --all-targets -- -D warnings` before opening a PR.

## License

MIT — see [LICENSE](./LICENSE).

## Acknowledgements

- Inspired by [`Higangssh/winclipshot`](https://github.com/Higangssh/winclipshot), which solves the path-paste half of this problem on Windows.
- Built on excellent crates: [`clipboard-rs`](https://github.com/ChurchTao/clipboard-rs), [`wl-clipboard-rs`](https://github.com/YaLTeR/wl-clipboard-rs), [`notify`](https://github.com/notify-rs/notify).
