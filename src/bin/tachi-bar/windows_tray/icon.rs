//! Pixel work for the tray: decode Tachi's face, shrink it, and overlay the
//! status dots. Everything here is plain RGBA8 math (no Win32), so it is
//! unit-testable anywhere.

use std::io::Cursor;
use tachi_noti::state::Status;

/// Edge length of the tray icon. Windows scales it down to the DPI-dependent
/// notification-area size; 32 is the largest size it ever asks for at 200%.
pub const TRAY_SIZE: u32 = 32;
/// Edge length of the per-row dot shown in the menu.
pub const MENU_DOT_SIZE: u32 = 16;

const OUTLINE: [u8; 4] = [0x1C, 0x1C, 0x1E, 0xFF];
const ALERT_RED: [u8; 3] = [0xFF, 0x3B, 0x30];

/// Straight-alpha RGBA8 image, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    /// Fully transparent canvas (also the fallback face if the PNG cannot be decoded).
    pub fn transparent(width: u32, height: u32) -> Self {
        Rgba { width, height, data: vec![0; (width * height * 4) as usize] }
    }

    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }
}

/// Same colours as the AppKit renderer: waiting yellow, running green, idle gray.
pub fn status_color(status: Status) -> [u8; 3] {
    match status {
        Status::Waiting => [0xF5, 0xC5, 0x18],
        Status::Running => [0x34, 0xC7, 0x59],
        Status::Idle => [0x8E, 0x8E, 0x93],
    }
}

/// Decode a PNG into RGBA8, whatever its colour type or bit depth.
pub fn decode_png(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    // EXPAND turns palette / low-bit-depth / tRNS into plain samples, STRIP_16
    // brings 16-bit down to 8, so only the four 8-bit layouts remain.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| format!("png header: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("png too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("png data: {e}"))?;
    let px = &buf[..info.buffer_size()];

    let mut data = Vec::with_capacity((info.width * info.height * 4) as usize);
    match info.color_type {
        png::ColorType::Rgba => data.extend_from_slice(px),
        png::ColorType::Rgb => px.chunks_exact(3).for_each(|p| data.extend_from_slice(&[p[0], p[1], p[2], 255])),
        png::ColorType::Grayscale => px.iter().for_each(|&g| data.extend_from_slice(&[g, g, g, 255])),
        png::ColorType::GrayscaleAlpha => {
            px.chunks_exact(2).for_each(|p| data.extend_from_slice(&[p[0], p[0], p[0], p[1]]))
        }
        png::ColorType::Indexed => return Err("png: palette was not expanded".into()),
    }
    if data.len() != (info.width * info.height * 4) as usize {
        return Err("png: unexpected pixel count".into());
    }
    Ok(Rgba { width: info.width, height: info.height, data })
}

/// Box-filter resize: every destination pixel is the alpha-weighted mean of the
/// source rectangle it covers (so transparent pixels do not bleed dark fringes).
pub fn box_downscale(src: &Rgba, width: u32, height: u32) -> Rgba {
    if width == 0 || height == 0 || src.width == 0 || src.height == 0 {
        return Rgba::transparent(width, height);
    }
    let mut out = Rgba::transparent(width, height);
    for y in 0..height {
        let sy0 = (y as u64 * src.height as u64 / height as u64) as u32;
        let sy1 = (((y as u64 + 1) * src.height as u64 / height as u64) as u32).max(sy0 + 1).min(src.height);
        for x in 0..width {
            let sx0 = (x as u64 * src.width as u64 / width as u64) as u32;
            let sx1 = (((x as u64 + 1) * src.width as u64 / width as u64) as u32).max(sx0 + 1).min(src.width);
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let p = src.at(sx, sy);
                    let pa = p[3] as u64;
                    r += p[0] as u64 * pa;
                    g += p[1] as u64 * pa;
                    b += p[2] as u64 * pa;
                    a += pa;
                    n += 1;
                }
            }
            let i = ((y * width + x) * 4) as usize;
            if a > 0 {
                out.data[i] = ((r + a / 2) / a) as u8;
                out.data[i + 1] = ((g + a / 2) / a) as u8;
                out.data[i + 2] = ((b + a / 2) / a) as u8;
                out.data[i + 3] = ((a + n / 2) / n) as u8;
            }
        }
    }
    out
}

/// Source-over blend of an anti-aliased disc (coverage from the distance to
/// the circle edge) onto `img`.
fn blend_disc(img: &mut Rgba, cx: f32, cy: f32, radius: f32, color: [u8; 4]) {
    let x_lo = ((cx - radius - 1.0).floor().max(0.0)) as u32;
    let y_lo = ((cy - radius - 1.0).floor().max(0.0)) as u32;
    let x_hi = ((cx + radius + 1.0).ceil().max(0.0) as u32).min(img.width);
    let y_hi = ((cy + radius + 1.0).ceil().max(0.0) as u32).min(img.height);
    for y in y_lo..y_hi {
        for x in x_lo..x_hi {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let cov = (radius - d + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let sa = cov * color[3] as f32 / 255.0;
            let i = ((y * img.width + x) * 4) as usize;
            let da = img.data[i + 3] as f32 / 255.0;
            let oa = sa + da * (1.0 - sa);
            if oa <= 0.0 {
                continue;
            }
            for c in 0..3 {
                let s = color[c] as f32;
                let dst = img.data[i + c] as f32;
                img.data[i + c] = ((s * sa + dst * da * (1.0 - sa)) / oa).round().clamp(0.0, 255.0) as u8;
            }
            img.data[i + 3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// A coloured disc with a dark rim, so it reads on light and dark taskbars.
fn blend_badge(img: &mut Rgba, cx: f32, cy: f32, radius: f32, rgb: [u8; 3]) {
    blend_disc(img, cx, cy, radius + 1.0, OUTLINE);
    blend_disc(img, cx, cy, radius, [rgb[0], rgb[1], rgb[2], 0xFF]);
}

/// The tray icon: `base` (already `TRAY_SIZE` square) plus a status dot at the
/// bottom-right (`None` = no sessions, no dot) and a small red usage-alert dot
/// at the top-right.
pub fn compose_tray(base: &Rgba, dot: Option<Status>, alert: bool) -> Rgba {
    let mut img = base.clone();
    let s = img.width.min(img.height) as f32 / TRAY_SIZE as f32;
    let (w, h) = (img.width as f32, img.height as f32);
    if let Some(status) = dot {
        blend_badge(&mut img, w - 7.0 * s, h - 7.0 * s, 6.0 * s, status_color(status));
    }
    if alert {
        blend_badge(&mut img, w - 5.0 * s, 5.0 * s, 4.0 * s, ALERT_RED);
    }
    img
}

/// Face scaled to the tray size, ready for `compose_tray`.
pub fn tray_base(face: &Rgba) -> Rgba {
    box_downscale(face, TRAY_SIZE, TRAY_SIZE)
}

/// The 16x16 status dot shown beside a session row in the menu.
pub fn menu_dot(status: Status) -> Rgba {
    let mut img = Rgba::transparent(MENU_DOT_SIZE, MENU_DOT_SIZE);
    let c = MENU_DOT_SIZE as f32 / 2.0;
    blend_badge(&mut img, c, c, 4.5, status_color(status));
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Rgba {
        Rgba { width: w, height: h, data: px.iter().copied().cycle().take((w * h * 4) as usize).collect() }
    }

    #[test]
    fn box_filter_averages_each_block() {
        // 4x2 -> 2x1: each 2x2 block collapses to its per-channel mean.
        let mut src = solid(4, 2, [0, 0, 0, 255]);
        let set = |img: &mut Rgba, x: u32, y: u32, v: u8| {
            let i = ((y * img.width + x) * 4) as usize;
            img.data[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        };
        // left 2x2 block: 0, 100, 100, 200 -> mean 100
        set(&mut src, 0, 0, 0);
        set(&mut src, 1, 0, 100);
        set(&mut src, 0, 1, 100);
        set(&mut src, 1, 1, 200);
        // right 2x2 block all 40
        for (x, y) in [(2, 0), (3, 0), (2, 1), (3, 1)] {
            set(&mut src, x, y, 40);
        }
        let out = box_downscale(&src, 2, 1);
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.at(0, 0), [100, 100, 100, 255]);
        assert_eq!(out.at(1, 0), [40, 40, 40, 255]);
    }

    #[test]
    fn box_filter_weights_colour_by_alpha() {
        // One opaque white pixel next to one fully transparent black pixel:
        // the mean must stay white at half alpha, not turn gray.
        let src = Rgba { width: 2, height: 1, data: vec![255, 255, 255, 255, 0, 0, 0, 0] };
        let out = box_downscale(&src, 1, 1);
        assert_eq!(out.at(0, 0), [255, 255, 255, 128]);
    }

    #[test]
    fn box_filter_handles_non_integer_ratio() {
        let src = solid(5, 5, [10, 20, 30, 255]);
        let out = box_downscale(&src, 3, 3);
        assert!(out.data.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
    }

    #[test]
    fn no_dot_leaves_the_face_untouched() {
        let base = solid(TRAY_SIZE, TRAY_SIZE, [200, 100, 50, 255]);
        assert_eq!(compose_tray(&base, None, false), base);
    }

    #[test]
    fn status_dot_sits_bottom_right_and_alert_top_right() {
        let base = solid(TRAY_SIZE, TRAY_SIZE, [255, 255, 255, 255]);
        let img = compose_tray(&base, Some(Status::Running), true);
        let g = status_color(Status::Running);
        assert_eq!(&img.at(25, 25)[..3], &g, "dot centre");
        assert_eq!(&img.at(27, 5)[..3], &ALERT_RED, "alert centre");
        assert_eq!(img.at(2, 2), [255, 255, 255, 255], "far corner untouched");
        // Rim is dark so the dot reads on a light taskbar.
        assert!(img.at(25, 31)[..3].iter().all(|&c| c < 60), "rim {:?}", img.at(25, 31));
    }

    #[test]
    fn dot_colour_follows_status() {
        let base = solid(TRAY_SIZE, TRAY_SIZE, [0, 0, 0, 255]);
        for s in [Status::Waiting, Status::Running, Status::Idle] {
            let img = compose_tray(&base, Some(s), false);
            assert_eq!(&img.at(25, 25)[..3], &status_color(s));
        }
    }

    #[test]
    fn menu_dot_is_transparent_at_the_corners_and_coloured_in_the_middle() {
        let dot = menu_dot(Status::Waiting);
        assert_eq!(dot.at(0, 0)[3], 0);
        let mid = dot.at(8, 8);
        assert_eq!((&mid[..3], mid[3]), (&status_color(Status::Waiting)[..], 255));
    }

    #[test]
    fn decodes_the_shipped_face() {
        let face = decode_png(include_bytes!("../../../../assets/tachi-head.png")).expect("decode");
        assert!(face.width > 0 && face.height > 0);
        let base = tray_base(&face);
        assert_eq!((base.width, base.height), (TRAY_SIZE, TRAY_SIZE));
        assert_eq!(base.data.len(), (TRAY_SIZE * TRAY_SIZE * 4) as usize);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_png(b"not a png").is_err());
    }
}
