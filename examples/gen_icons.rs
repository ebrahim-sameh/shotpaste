//! One-off icon generator for shotpaste. Run with:
//!
//!     cargo run --example gen_icons
//!
//! Writes `assets/icon.png` (256×256 color) and `assets/icon-template.png`
//! (32×32 monochrome white-on-transparent, for the macOS menu bar).
//!
//! The glyph is a clipboard outline with a small camera-shutter disc — chosen
//! to read at 16×16 in the tray. Drawn by hand with simple rasterization so
//! we don't add a font/SVG dep just for the placeholder. Replace with a real
//! designed icon when the project grows one.

use image::{ImageBuffer, Rgba};
use std::path::Path;

const COLOR_SIZE: u32 = 256;
const TEMPLATE_SIZE: u32 = 32;

fn main() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    std::fs::create_dir_all(&assets).expect("create assets dir");

    let color = render_icon(
        COLOR_SIZE,
        Rgba([245, 245, 250, 255]),
        Rgba([30, 30, 35, 255]),
    );
    color.save(assets.join("icon.png")).expect("write icon.png");

    let template = render_icon(
        TEMPLATE_SIZE,
        Rgba([255, 255, 255, 255]),
        Rgba([255, 255, 255, 255]),
    );
    template
        .save(assets.join("icon-template.png"))
        .expect("write icon-template.png");

    println!(
        "wrote {}/icon.png and {}/icon-template.png",
        assets.display(),
        assets.display()
    );
}

/// Render the shotpaste glyph at `size` px square. The clipboard body uses
/// `clip_color`; the shutter disc uses `dot_color`. For the macOS template
/// both are pure white — the OS auto-inverts.
fn render_icon(
    size: u32,
    clip_color: Rgba<u8>,
    dot_color: Rgba<u8>,
) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let transparent = Rgba([0, 0, 0, 0]);
    let mut img = ImageBuffer::from_pixel(size, size, transparent);
    let s = size as f32;

    // Geometry, all in normalized units (0..1) then scaled by `s`.
    // Clipboard outline: a rounded rect from (.20, .18) to (.80, .92).
    // Clip head: a smaller rounded rect from (.36, .10) to (.64, .24).
    // Shutter disc: centered at (.50, .58), radius .18.
    let clip_outer = Rect {
        x0: 0.20 * s,
        y0: 0.18 * s,
        x1: 0.80 * s,
        y1: 0.92 * s,
        radius: 0.06 * s,
    };
    let clip_inner = Rect {
        x0: 0.26 * s,
        y0: 0.24 * s,
        x1: 0.74 * s,
        y1: 0.86 * s,
        radius: 0.04 * s,
    };
    let head_outer = Rect {
        x0: 0.36 * s,
        y0: 0.10 * s,
        x1: 0.64 * s,
        y1: 0.24 * s,
        radius: 0.03 * s,
    };
    let head_inner = Rect {
        x0: 0.40 * s,
        y0: 0.14 * s,
        x1: 0.60 * s,
        y1: 0.22 * s,
        radius: 0.02 * s,
    };
    let dot_cx = 0.50 * s;
    let dot_cy = 0.58 * s;
    let dot_r = 0.18 * s;
    let dot_inner_r = 0.10 * s;

    for y in 0..size {
        for x in 0..size {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;

            // Clipboard outline (outer minus inner) and head.
            let in_clip = clip_outer.contains(fx, fy) && !clip_inner.contains(fx, fy);
            let in_head = head_outer.contains(fx, fy) && !head_inner.contains(fx, fy);
            let in_outline = in_clip || in_head;

            // Shutter disc — outer ring filled with dot_color, inner hole for
            // the "lens" detail.
            let dx = fx - dot_cx;
            let dy = fy - dot_cy;
            let dist2 = dx * dx + dy * dy;
            let in_disc = dist2 <= dot_r * dot_r && dist2 >= dot_inner_r * dot_inner_r;

            if in_outline {
                img.put_pixel(x, y, clip_color);
            } else if in_disc {
                img.put_pixel(x, y, dot_color);
            }
        }
    }

    img
}

struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radius: f32,
}

impl Rect {
    /// Rounded-rect inclusion test with squared-distance corner check —
    /// avoids a sqrt per pixel and is plenty crisp at icon resolutions.
    fn contains(&self, x: f32, y: f32) -> bool {
        if x < self.x0 || x > self.x1 || y < self.y0 || y > self.y1 {
            return false;
        }
        let r = self.radius;
        // Determine which corner region we're in, if any.
        let (cx, cy) = match (
            x < self.x0 + r,
            x > self.x1 - r,
            y < self.y0 + r,
            y > self.y1 - r,
        ) {
            (true, _, true, _) => (self.x0 + r, self.y0 + r),
            (_, true, true, _) => (self.x1 - r, self.y0 + r),
            (true, _, _, true) => (self.x0 + r, self.y1 - r),
            (_, true, _, true) => (self.x1 - r, self.y1 - r),
            _ => return true, // straight-edge interior
        };
        let dx = x - cx;
        let dy = y - cy;
        dx * dx + dy * dy <= r * r
    }
}
