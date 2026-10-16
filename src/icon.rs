//! The application icon, drawn in code instead of shipped as an asset.
//!
//! A blue rounded tile with a white sheet of paper on it (its top-right corner
//! folded over) and a blue check mark on the sheet: "PDF, handled". The twin of
//! MediaFileValid's icon (a film frame with a check mark): one accent colour,
//! flat shapes, legible at 16 px. Chosen by the user from three candidates
//! (check mark, a stack of sheets, a shield).
//!
//! One definition serves both places the icon appears: the FLTK window icon
//! (32 px, via [`render_rgba`]) and the `.ico` compiled into the executable by
//! `build.rs` (via [`ico_bytes`]). Keeping them in one file means the taskbar
//! and Explorer can never show two different pictures.
//!
//! Shapes are described in normalized 0..1 coordinates so they render at any
//! size, and every pixel is supersampled — an aliased icon looks obviously
//! homemade at 16 px. Nothing here depends on `fltk`; `build.rs` pulls this
//! module in directly with `#[path]`.

/// Icon sizes stored in the `.ico`. Windows picks the closest one and scales
/// from there, so this covers the list view (16), the taskbar and desktop
/// (32/48), and Explorer's large-icon views (64/128). 256 is deliberately
/// omitted: as an uncompressed bitmap it alone would add ~264 KB, and a flat
/// geometric shape upscales from 128 without artifacts.
pub const ICO_SIZES: &[u32] = &[16, 32, 48, 64, 128];

/// The app's accent colour: the icon tile, and the progress bar fill in the UI,
/// which reads it from here so the two cannot drift apart.
pub const BLUE: [u8; 3] = [0x2F, 0x7D, 0xD1];
const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];
/// The folded-over corner shows the sheet's back: white tinted with the tile.
const FOLD: [u8; 3] = [0xB9, 0xD4, 0xF1];

/// Corner radius of the tile, which fills the whole square: with a margin
/// around it, the taskbar showed it 26 px tall beside 30 px icons (measured).
const TILE_RADIUS: f32 = 5.0 / 28.0;

/// The sheet: left, top, right, bottom edges and the size of the folded corner.
const SHEET: Sheet = Sheet { left: 0.237, top: 0.123, right: 0.763, bottom: 0.877, fold: 0.171 };

/// Samples per axis inside one pixel; 4x4 is enough to hide the staircase on
/// rounded corners and diagonals.
const SUB: u32 = 4;

/// A sheet of paper with its top-right corner folded down.
struct Sheet {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    fold: f32,
}

impl Sheet {
    /// Inside the paper (the cut-off corner excluded, the fold included).
    fn contains(&self, u: f32, v: f32) -> bool {
        (self.left..=self.right).contains(&u)
            && (self.top..=self.bottom).contains(&v)
            // Above the diagonal from (right - fold, top) to (right, top + fold)
            // the corner is folded away.
            && (u - (self.right - self.fold)) <= (v - self.top)
    }

    /// Inside the folded-over triangle in the top-right corner.
    fn in_fold(&self, u: f32, v: f32) -> bool {
        u >= self.right - self.fold
            && v <= self.top + self.fold
            && (u - (self.right - self.fold)) <= (v - self.top)
    }
}

/// Check mark: short stroke down to the corner, long stroke up to the right.
const CHECK: [(f32, f32); 3] = [(0.34, 0.569), (0.454, 0.683), (0.66, 0.431)];
/// Half the stroke width of the check mark.
const CHECK_HALF: f32 = 0.057;

/// Render the icon as straight-alpha RGBA bytes, row-major, top row first.
///
/// Returns `size * size * 4` bytes.
pub fn render_rgba(size: u32) -> Vec<u8> {
    let mut px = vec![0u8; (size as usize).pow(2) * 4];
    let samples = (SUB * SUB) as f32;
    for y in 0..size {
        for x in 0..size {
            // Sum the colour of every covered subsample; uncovered ones are
            // transparent and only lower the alpha.
            let (mut covered, mut sum) = (0u32, [0f32; 3]);
            for sy in 0..SUB {
                for sx in 0..SUB {
                    if let Some(colour) = shade(sample(x, sx, size), sample(y, sy, size)) {
                        covered += 1;
                        for (total, channel) in sum.iter_mut().zip(colour) {
                            *total += f32::from(channel);
                        }
                    }
                }
            }
            if covered == 0 {
                continue; // Fully outside the tile: leave it transparent.
            }
            let i = ((y * size + x) * 4) as usize;
            for (channel, total) in sum.iter().enumerate() {
                px[i + channel] = (total / covered as f32).round() as u8;
            }
            px[i + 3] = (255.0 * covered as f32 / samples).round() as u8;
        }
    }
    px
}

/// Centre of subsample `sub` inside pixel `pixel`, in 0..1 across the icon.
fn sample(pixel: u32, sub: u32, size: u32) -> f32 {
    (pixel as f32 + (sub as f32 + 0.5) / SUB as f32) / size as f32
}

/// Colour of the point `(u, v)`, or `None` outside the tile.
fn shade(u: f32, v: f32) -> Option<[u8; 3]> {
    if !in_tile(u, v) {
        return None;
    }
    let [a, b, c] = CHECK;
    let on_check = distance_to_segment((u, v), a, b) <= CHECK_HALF
        || distance_to_segment((u, v), b, c) <= CHECK_HALF;
    Some(if !SHEET.contains(u, v) {
        BLUE
    } else if SHEET.in_fold(u, v) {
        FOLD
    } else if on_check {
        BLUE
    } else {
        WHITE
    })
}

/// Rounded square: the whole icon, its corners clipped to a circle.
fn in_tile(u: f32, v: f32) -> bool {
    const LO: f32 = 0.0;
    const HI: f32 = 1.0;
    if !(LO..=HI).contains(&u) || !(LO..=HI).contains(&v) {
        return false;
    }
    // Distance past the straight edges; zero anywhere but in a corner.
    let corner = |t: f32| (LO + TILE_RADIUS - t).max(t - (HI - TILE_RADIUS)).max(0.0);
    let (dx, dy) = (corner(u), corner(v));
    dx * dx + dy * dy <= TILE_RADIUS * TILE_RADIUS
}

/// Euclidean distance from `p` to the segment `a`–`b`.
fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    // Position of the closest point along the segment, clamped to its ends.
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// Build a complete Windows `.ico` holding every size in [`ICO_SIZES`].
///
/// Images are stored in the classic BMP form (32-bit BGRA, bottom-up, followed
/// by an all-zero AND mask) rather than PNG: PNG entries would need a deflate
/// encoder, and compressing five flat-colour tiles is not worth a dependency.
pub fn ico_bytes() -> Vec<u8> {
    let images: Vec<Vec<u8>> = ICO_SIZES.iter().map(|&size| bmp_image(size)).collect();

    // ICONDIR: reserved, type 1 = icon, image count.
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());

    // One ICONDIRENTRY per image, each pointing past the whole directory.
    let mut offset = 6 + 16 * images.len() as u32;
    for (&size, image) in ICO_SIZES.iter().zip(&images) {
        // The format stores a side of 256 as 0, which is exactly what the
        // truncating cast produces; nothing larger is representable anyway.
        let side = size as u8;
        out.extend_from_slice(&[side, side, 0, 0]); // width, height, palette, reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }

    for image in &images {
        out.extend_from_slice(image);
    }
    out
}

/// One icon image: `BITMAPINFOHEADER` + pixels + AND mask.
fn bmp_image(size: u32) -> Vec<u8> {
    let rgba = render_rgba(size);
    // The mask is 1 bit per pixel with every row padded to 4 bytes.
    let mask_row = size.div_ceil(32) * 4;

    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes()); // header size
    out.extend_from_slice(&(size as i32).to_le_bytes());
    // Doubled height: the header describes the pixels AND the mask together.
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB, uncompressed
    // Image size and the resolution/palette fields may all stay zero for BI_RGB.
    out.extend_from_slice(&[0u8; 20]);

    // Pixels: bottom-up rows, BGRA order.
    for row in rgba.chunks_exact(size as usize * 4).rev() {
        for &[r, g, b, a] in row.as_chunks::<4>().0 {
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    // AND mask: all zero, because the alpha channel already carries the shape.
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read one pixel as `(r, g, b, a)`.
    fn pixel(px: &[u8], size: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * size + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3])
    }

    const S: u32 = 64;
    const OPAQUE_BLUE: (u8, u8, u8, u8) = (BLUE[0], BLUE[1], BLUE[2], 255);
    const OPAQUE_WHITE: (u8, u8, u8, u8) = (255, 255, 255, 255);

    /// The pixel containing the normalized point `(u, v)` of a 64 px icon.
    fn at(px: &[u8], u: f32, v: f32) -> (u8, u8, u8, u8) {
        pixel(px, S, (u * S as f32) as u32, (v * S as f32) as u32)
    }

    #[test]
    fn corners_are_transparent() {
        let px = render_rgba(S);
        for (x, y) in [(0, 0), (S - 1, 0), (0, S - 1), (S - 1, S - 1)] {
            assert_eq!(pixel(&px, S, x, y).3, 0, "corner ({x},{y}) must be cut away");
        }
    }

    #[test]
    fn the_tile_fills_the_square_to_its_edges() {
        let px = render_rgba(S);
        for (x, y) in [(0, S / 2), (S - 1, S / 2), (S / 2, 0), (S / 2, S - 1)] {
            assert_eq!(pixel(&px, S, x, y), OPAQUE_BLUE, "edge ({x},{y})");
        }
    }

    #[test]
    fn white_paper_with_a_folded_corner_on_a_blue_tile() {
        let px = render_rgba(S);
        assert_eq!(at(&px, 0.15, 0.5), OPAQUE_BLUE, "tile left of the paper");
        assert_eq!(at(&px, 0.33, 0.30), OPAQUE_WHITE, "paper");
        // The fold triangle sits below the diagonal from (0.592, 0.123) to
        // (0.763, 0.294); above it the corner is gone and the tile shows through.
        assert_eq!(at(&px, 0.62, 0.28), (FOLD[0], FOLD[1], FOLD[2], 255), "the fold");
        assert_eq!(at(&px, 0.71, 0.20), OPAQUE_BLUE, "tile where the corner was folded away");
    }

    #[test]
    fn the_check_mark_is_blue_on_the_paper() {
        let px = render_rgba(S);
        assert_eq!(at(&px, CHECK[1].0, CHECK[1].1), OPAQUE_BLUE, "the check mark's corner");
        assert_eq!(at(&px, 0.55, 0.70), OPAQUE_WHITE, "paper below the long stroke");
    }

    #[test]
    fn edges_are_antialiased_rather_than_hard() {
        const S: u32 = 64;
        let px = render_rgba(S);
        // A rounded corner must produce at least one partially covered pixel;
        // without supersampling every alpha would be 0 or 255.
        let partial = (0..S)
            .flat_map(|y| (0..S).map(move |x| (x, y)))
            .any(|(x, y)| matches!(pixel(&px, S, x, y).3, 1..=254));
        assert!(partial, "expected soft edges");
    }

    #[test]
    fn ico_directory_matches_the_images_that_follow_it() {
        let ico = ico_bytes();
        assert_eq!(&ico[0..2], &0u16.to_le_bytes(), "reserved");
        assert_eq!(&ico[2..4], &1u16.to_le_bytes(), "type 1 = icon");
        assert_eq!(&ico[4..6], &(ICO_SIZES.len() as u16).to_le_bytes());

        // Images must start right after the directory, sit back to back, and
        // end exactly at the end of the file.
        let mut expected_at = 6 + 16 * ICO_SIZES.len() as u32;
        for (i, &size) in ICO_SIZES.iter().enumerate() {
            let entry = &ico[6 + i * 16..6 + (i + 1) * 16];
            let word =
                |at: usize| u32::from_le_bytes(entry[at..at + 4].try_into().expect("4 bytes"));
            assert_eq!(entry[0..2], [size as u8, size as u8], "width/height for {size}");
            // BITMAPINFOHEADER + pixels + mask, exactly.
            let expected_len = 40 + size * size * 4 + size.div_ceil(32) * 4 * size;
            assert_eq!(word(8), expected_len, "image length for {size}");
            assert_eq!(word(12), expected_at, "offset of the {size} px image");
            expected_at += expected_len;
        }
        assert_eq!(expected_at as usize, ico.len(), "trailing bytes after the last image");
    }
}
