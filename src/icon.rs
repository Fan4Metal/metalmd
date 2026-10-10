//! Procedurally drawn application icon: a sheet of paper with a folded
//! corner, a few lines of text and a blue Markdown mark (M↓) on it.
//! Dependency-free so that `build.rs` can include this file to produce the
//! `.ico` embedded in the executable and the raw RGBA the app shows, so that
//! nothing is rasterised at start-up.

// build.rs and the app use different parts.
#![allow(dead_code)]

/// Sizes in the `.ico` of the executable.
pub const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// Straight RGBA, channels 0..1.
type Rgba = [f32; 4];

fn hex(c: u32) -> Rgba {
    let ch = |s: u32| ((c >> s) & 0xff) as f32 / 255.0;
    [ch(16), ch(8), ch(0), 1.0]
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

fn with_alpha(c: Rgba, a: f32) -> Rgba {
    [c[0], c[1], c[2], c[3] * a.clamp(0.0, 1.0)]
}

/// Paint `top` over `under`.
fn over(under: Rgba, top: Rgba) -> Rgba {
    let a = top[3] + under[3] * (1.0 - top[3]);
    if a <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0, 0.0, 0.0, a];
    for i in 0..3 {
        out[i] = (top[i] * top[3] + under[i] * under[3] * (1.0 - top[3])) / a;
    }
    out
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Signed distance from `(x, y)` to a box from `min` to `max` with corners
/// rounded by `r` (negative inside).
fn rounded_box(x: f32, y: f32, min: (f32, f32), max: (f32, f32), r: f32) -> f32 {
    let (cx, cy) = ((min.0 + max.0) / 2.0, (min.1 + max.1) / 2.0);
    let (hx, hy) = ((max.0 - min.0) / 2.0 - r, (max.1 - min.1) / 2.0 - r);
    let qx = (x - cx).abs() - hx;
    let qy = (y - cy).abs() - hy;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Distance from `(x, y)` to the segment `a`–`b`.
fn segment(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (px, py) = (x - a.0, y - a.1);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = ((px * dx + py * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((px - dx * t).powi(2) + (py - dy * t).powi(2)).sqrt()
}

/// Whether `(x, y)` is inside the triangle `a`, `b`, `c`.
fn in_triangle(x: f32, y: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let side = |p: (f32, f32), q: (f32, f32)| (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0);
    let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
    let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(negative && positive)
}

// The sheet, in units of the icon's side.
const LEFT: f32 = 0.16;
const RIGHT: f32 = 0.84;
const TOP: f32 = 0.05;
const BOTTOM: f32 = 0.93;
const RADIUS: f32 = 0.06;
/// Size of the folded corner.
const FOLD: f32 = 0.22;

/// The colour of one point of the icon; `x`, `y` in 0..1 from the top left.
fn sample(x: f32, y: f32) -> Rgba {
    let mut c: Rgba = [0.0; 4];

    // Soft shadow under the sheet.
    let shadow = rounded_box(x, y - 0.025, (LEFT, TOP), (RIGHT, BOTTOM), RADIUS);
    c = over(
        c,
        with_alpha(
            hex(0x1b2433),
            0.32 * (1.0 - smoothstep(-0.01, 0.045, shadow)),
        ),
    );

    // The sheet without its top right corner, cut along the fold.
    let fold_line = (x - (RIGHT - FOLD)) - (y - TOP);
    let sheet = rounded_box(x, y, (LEFT, TOP), (RIGHT, BOTTOM), RADIUS) < 0.0 && fold_line < 0.0;
    if sheet {
        let paper = mix(hex(0xffffff), hex(0xe9edf3), (y - TOP) / (BOTTOM - TOP));
        // A thin darker rim.
        let edge = -rounded_box(x, y, (LEFT, TOP), (RIGHT, BOTTOM), RADIUS);
        let rim = 1.0 - smoothstep(0.0, 0.012, edge.min(-fold_line / std::f32::consts::SQRT_2));
        c = over(c, mix(paper, hex(0xb7c0cc), rim * 0.8));

        // Lines of text.
        for (row, (start, end)) in [(0.25, 0.62), (0.25, 0.70), (0.25, 0.56)]
            .iter()
            .enumerate()
        {
            let line_y = 0.25 + row as f32 * 0.085;
            if segment(x, y, (*start, line_y), (*end, line_y)) < 0.02 {
                c = over(c, hex(0xc3cad5));
            }
        }
    }

    // The folded flap, lying on the sheet below the cut, with a shadow.
    let a = (RIGHT - FOLD, TOP);
    let b = (RIGHT, TOP + FOLD);
    let corner = (RIGHT - FOLD, TOP + FOLD);
    if in_triangle(x - 0.012, y - 0.012, a, b, corner) && !in_triangle(x, y, a, b, corner) && sheet
    {
        c = over(c, with_alpha(hex(0x1b2433), 0.18));
    }
    if in_triangle(x, y, a, b, corner) {
        let t = ((x - a.0) + (y - a.1)) / (2.0 * FOLD);
        c = over(c, mix(hex(0xdfe4eb), hex(0xc4ccd7), t));
    }

    // The Markdown mark: a blue badge with a white M and a down arrow.
    let badge = rounded_box(x, y, (0.23, 0.53), (0.77, 0.84), 0.07);
    if badge < 0.0 {
        c = over(c, mix(hex(0x3d8bfd), hex(0x0a58ca), (y - 0.53) / 0.31));
        let stroke = 0.026;
        let m = [
            (0.31, 0.77),
            (0.31, 0.61),
            (0.395, 0.70),
            (0.48, 0.61),
            (0.48, 0.77),
        ];
        let on_m = m.windows(2).any(|w| segment(x, y, w[0], w[1]) < stroke);
        let shaft = segment(x, y, (0.625, 0.60), (0.625, 0.70)) < stroke;
        let head = in_triangle(x, y, (0.545, 0.68), (0.705, 0.68), (0.625, 0.785));
        if on_m || shaft || head {
            c = over(c, hex(0xffffff));
        }
    }
    c
}

/// Straight (non-premultiplied) RGBA pixels, `size * size * 4` bytes, row
/// major from the top. 4x4 supersampling gives smooth edges.
pub fn rgba(size: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let n = size as usize;
    let mut out = vec![0u8; n * n * 4];
    for py in 0..size {
        for px in 0..size {
            // Averaged premultiplied, so transparent samples add no colour.
            let mut acc = [0.0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    let c = sample(x, y);
                    for i in 0..3 {
                        acc[i] += c[i] * c[3];
                    }
                    acc[3] += c[3];
                }
            }
            let count = (SS * SS) as f32;
            let alpha = acc[3] / count;
            let i = (py as usize * n + px as usize) * 4;
            if alpha > 0.0 {
                for ch in 0..3 {
                    out[i + ch] = (acc[ch] / acc[3] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                out[i + 3] = (alpha * 255.0).round() as u8;
            }
        }
    }
    out
}

/// Encode the icon as a multi-resolution `.ico` (32-bit BMP entries).
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bmp_entry(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]); // reserved, type = icon
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        let dim = if s >= 256 { 0 } else { s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

/// BITMAPINFOHEADER + bottom-up BGRA pixels + 1-bpp AND mask.
fn bmp_entry(size: u32) -> Vec<u8> {
    let px = rgba(size);
    let n = size as usize;
    let mask_stride = n.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + n * n * 4 + mask_stride * n);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&(2 * size as i32).to_le_bytes()); // XOR + AND
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]); // compression .. colours important
    for y in (0..n).rev() {
        for x in 0..n {
            let i = (y * n + x) * 4;
            out.extend_from_slice(&[px[i + 2], px[i + 1], px[i], px[i + 3]]);
        }
    }
    // Alpha channel carries transparency; an all-zero AND mask is correct.
    out.resize(out.len() + mask_stride * n, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_shape() {
        let px = rgba(64);
        assert_eq!(px.len(), 64 * 64 * 4);
        // Corners are transparent; the badge centre is opaque blue.
        assert_eq!(px[3], 0);
        let c = ((51 * 64) + 25) * 4;
        assert_eq!(px[c + 3], 255);
        assert!(px[c + 2] > px[c], "badge is blue: {:?}", &px[c..c + 4]);
        let ico = ico(&[16, 32]);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 2, 0]);
    }
}
