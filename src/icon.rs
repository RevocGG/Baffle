//! Runtime-generated app icon (dark disc + teal spectrum bars).
//! Avoids an image-decoding dependency entirely.

const S: usize = 128;
const BG: (u8, u8, u8) = (0x0F, 0x11, 0x15);
const ACCENT: (u8, u8, u8) = (0x2D, 0xD4, 0xBF);

/// RGBA (unpremultiplied) pixels, width 128, height 128.
pub fn icon_rgba() -> Vec<u8> {
    let mut px = vec![0u8; S * S * 4];
    let (cx, cy) = (S as f32 / 2.0, S as f32 / 2.0);
    let r = S as f32 / 2.0 - 2.0;

    for y in 0..S {
        for x in 0..S {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if d <= r {
                let edge = ((r - d) / 1.5).min(1.0);
                let i = (y * S + x) * 4;
                px[i] = BG.0;
                px[i + 1] = BG.1;
                px[i + 2] = BG.2;
                px[i + 3] = (edge * 255.0) as u8;
            }
        }
    }

    // Spectrum bars
    let heights = [34usize, 58, 82, 58, 34];
    let bw = 12usize;
    let gap = 8usize;
    let total = heights.len() * bw + (heights.len() - 1) * gap;
    let x0 = (S - total) / 2;
    for (i, &h) in heights.iter().enumerate() {
        let bx = x0 + i * (bw + gap);
        let by = (S - h) / 2;
        for y in by..by + h {
            for x in bx..bx + bw {
                let dx = (x - bx).min(bx + bw - 1 - x);
                let dy = (y - by).min(by + h - 1 - y);
                if dx + dy >= 3 {
                    let i = (y * S + x) * 4;
                    px[i] = ACCENT.0;
                    px[i + 1] = ACCENT.1;
                    px[i + 2] = ACCENT.2;
                    px[i + 3] = 255;
                }
            }
        }
    }
    px
}

pub const ICON_W: u32 = S as u32;
pub const ICON_H: u32 = S as u32;

