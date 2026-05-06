//! Atomic multi-format clipboard write.
//!
//! Each platform exposes a different mechanism for putting more than one
//! format on the clipboard in a single transaction:
//!
//! - **Windows**: a single `OpenClipboard` → `EmptyClipboard` → many
//!   `SetClipboardData` → `CloseClipboard` cycle. We drive this directly
//!   via `clipboard-win` because the higher-level `clipboard-rs` calls
//!   `EmptyClipboard` mid-sequence when an image is present, which wipes
//!   the file-drop and text formats.
//! - **macOS**: `NSPasteboard.writeObjects:` accepts an array of objects
//!   with multiple registered types — atomic by construction.
//! - **Linux**: Wayland's `wl_data_source` advertises multiple MIMEs in
//!   one offer; X11 selection ownership advertises multiple targets.
//!   Both are exposed atomically by `clipboard-rs`.

use anyhow::Result;
use std::path::Path;

#[cfg(target_os = "windows")]
pub fn write_png(path: &Path) -> Result<()> {
    use anyhow::{Context, anyhow};
    use clipboard_win::{Clipboard, formats, options, raw};
    use image::{ImageFormat, ImageReader};
    use std::io::Cursor;
    use tracing::info;

    // Standard BITMAPFILEHEADER size — the bytes that precede the DIB content
    // in a .bmp file. CF_DIB wants the rest (info header + pixels), not those.
    const BMP_FILE_HEADER_LEN: usize = 14;

    let path_str = path
        .to_str()
        .context("screenshot path is not valid UTF-8")?
        .to_string();

    let png_bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    // Decode the PNG and re-encode as a BMP file in memory, then strip the
    // 14-byte BITMAPFILEHEADER prefix. What's left is exactly the CF_DIB
    // payload Win32 expects (BITMAPINFOHEADER + optional palette + pixels).
    let img = ImageReader::new(Cursor::new(&png_bytes))
        .with_guessed_format()
        .context("failed to guess image format")?
        .decode()
        .context("failed to decode PNG")?;
    let mut bmp_buf = Vec::with_capacity(png_bytes.len() * 2);
    img.write_to(&mut Cursor::new(&mut bmp_buf), ImageFormat::Bmp)
        .context("failed to encode BMP for clipboard")?;
    if bmp_buf.len() <= BMP_FILE_HEADER_LEN {
        anyhow::bail!("encoded BMP is too small ({} bytes)", bmp_buf.len());
    }
    let dib = &bmp_buf[BMP_FILE_HEADER_LEN..];

    // Hold the clipboard open for the entire write. One Empty, three Sets,
    // then close — that's the atomic transaction the OS exposes.
    let _clip =
        Clipboard::new_attempts(10).map_err(|e| anyhow!("failed to open clipboard (code {e})"))?;
    clipboard_win::empty().map_err(|e| anyhow!("failed to empty clipboard (code {e})"))?;

    raw::set_without_clear(formats::CF_DIB, dib)
        .map_err(|e| anyhow!("failed to set CF_DIB (code {e})"))?;

    raw::set_file_list_with(&[path_str.as_str()], options::NoClear)
        .map_err(|e| anyhow!("failed to set CF_HDROP (code {e})"))?;

    raw::set_string_with(&path_str, options::NoClear)
        .map_err(|e| anyhow!("failed to set CF_UNICODETEXT (code {e})"))?;

    info!(path = %path.display(), "wrote screenshot to clipboard (image + file + path)");
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn write_png(path: &Path) -> Result<()> {
    use anyhow::{Context, anyhow};
    use clipboard_rs::common::RustImage;
    use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext, common::RustImageData};
    use tracing::info;

    let path_str = path
        .to_str()
        .context("screenshot path is not valid UTF-8")?
        .to_string();

    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    let image =
        RustImageData::from_bytes(&bytes).map_err(|e| anyhow!("failed to decode PNG: {e}"))?;

    let ctx = ClipboardContext::new().map_err(|e| anyhow!("failed to open clipboard: {e}"))?;

    let payload = vec![
        ClipboardContent::Image(image),
        ClipboardContent::Files(vec![path_str.clone()]),
        ClipboardContent::Text(path_str.clone()),
    ];

    ctx.set(payload)
        .map_err(|e| anyhow!("failed to set clipboard: {e}"))?;

    info!(path = %path.display(), "wrote screenshot to clipboard (image + file + path)");
    Ok(())
}
