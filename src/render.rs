//! Software renderer: draws the menu into an ARGB8888 (BGRA byte order) shm buffer.
//! Pure CPU, no Wayland dependency, so it is unit-testable headless.

use ab_glyph::{Font, Glyph, GlyphId, PxScale, ScaleFont, point};

pub type Bgra = [u8; 4];

pub const fn bgra(r: u8, g: u8, b: u8, a: u8) -> Bgra {
    [b, g, r, a]
}

/// Spotlight panel corner radius (logical px), clamped to half the panel size.
pub const CORNER_RADIUS: u32 = 12;

// Eye-comfort rules, checked at compile time (clippy: assertions on constants
// belong here): the content inset must clear the corner radius so text never
// grazes the curve, and the panel must float off the screen edge rather than
// hug it.
const _: () = assert!(crate::PAD >= CORNER_RADIUS);
const _: () = assert!(crate::TOP_MARGIN >= 32);

/// Fully transparent pixel: everything outside the rounded panel outline.
const CLEAR: Bgra = [0, 0, 0, 0];

/// Fixed gap between the prompt label and the typed input (8px grid).
const PROMPT_GAP: f32 = 8.0;
/// Prompt label rail: vertical accent strip on the bar's left edge.
/// Vertical padding defines its height (text line + vpad both sides) —
/// the same rhythm the capsule used.
const PILL_VPAD: u32 = 2;
/// Width of the label rail.
const LABEL_RAIL_W: u32 = 4;
/// A logical-pixel divider at the input/list boundary.
const DIVIDER_H: u32 = 1;
const DIVIDER_ALPHA: u32 = 64;

pub const BG_NORMAL: Bgra = bgra(0x22, 0x22, 0x22, 0xff);
pub const FG_NORMAL: Bgra = bgra(0xbb, 0xbb, 0xbb, 0xff);
/// Input bar: a subtly lighter tone than the list, so bar and list read as
/// separate pieces of one panel.
pub const BG_PROMPT: Bgra = bgra(0x2e, 0x2e, 0x2e, 0xff);
pub const FG_PROMPT: Bgra = bgra(0xee, 0xee, 0xee, 0xff);
/// Prompt label accent: a 4px rail on the input bar's left edge (option F); a
/// deep slate blue that carries the emphasis while staying harmonious with the
/// gray bar and apart from the selection blue.
pub const LABEL_ACCENT: Bgra = bgra(0x2e, 0x4a, 0x5c, 0xff);
/// Selected row. The background is darker than the old `0f7ca6` so white text
/// clears WCAG AA: `#ffffff` on `#0b6285` is 6.77:1 (the previous pair was
/// 4.06:1 — the weakest contrast in the whole UI, on the row that matters most).
pub const BG_SEL: Bgra = bgra(0x0b, 0x62, 0x85, 0xff);
pub const FG_SEL: Bgra = bgra(0xff, 0xff, 0xff, 0xff);

/// Menu color scheme, wmenu-style (defaults match the old hardcoded constants).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colors {
    /// Normal row background (`-N`).
    pub bg_normal: Bgra,
    /// Normal row foreground (`-n`).
    pub fg_normal: Bgra,
    /// Prompt/input row background (`-M`).
    pub bg_prompt: Bgra,
    /// Prompt label accent (left rail; input bar stays `bg_prompt`).
    pub label_accent: Bgra,
    /// Prompt/input row foreground (`-m`).
    pub fg_prompt: Bgra,
    /// Selected row background (`-S`).
    pub bg_sel: Bgra,
    /// Selected row foreground (`-s`).
    pub fg_sel: Bgra,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            bg_normal: BG_NORMAL,
            fg_normal: FG_NORMAL,
            bg_prompt: BG_PROMPT,
            label_accent: LABEL_ACCENT,
            fg_prompt: FG_PROMPT,
            bg_sel: BG_SEL,
            fg_sel: FG_SEL,
        }
    }
}

/// Parse a wmenu `RRGGBB` or `RRGGBBAA` color into BGRA `[b, g, r, a]`.
pub fn parse_color(s: &str) -> Option<Bgra> {
    let s = s.strip_prefix('#').unwrap_or(s);
    let v = u32::from_str_radix(s, 16).ok()?;
    match s.len() {
        6 => Some([
            (v & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            ((v >> 16) & 0xff) as u8,
            0xff,
        ]),
        8 => Some([
            ((v >> 8) & 0xff) as u8,
            ((v >> 16) & 0xff) as u8,
            ((v >> 24) & 0xff) as u8,
            (v & 0xff) as u8,
        ]),
        _ => None,
    }
}

/// One visible item row (text already resolved).
pub struct Row<'a> {
    pub text: &'a str,
    pub selected: bool,
}

/// Horizontal pixel span `[x0, x1)` of the rounded panel at row `y`
/// (x0 inclusive, x1 exclusive). Corner pixels fall outside the span;
/// middle rows span the full width. Radius is clamped to half a dimension.
pub fn rounded_span(w: u32, h: u32, r: u32, y: u32) -> (u32, u32) {
    let r = r.min(w / 2).min(h / 2);
    if r == 0 {
        return if y < h { (0, w) } else { (0, 0) };
    }
    let rr = (r * r) as f64;
    let py = y as f64 + 0.5;
    let top = r as f64;
    let bottom = h as f64 - r as f64;
    if py >= top && py <= bottom {
        return (0, w);
    }
    let dy = if py < top { top - py } else { py - bottom };
    let inset = (r as f64 - (rr - dy * dy).max(0.0).sqrt()).ceil() as u32;
    (inset.min(w), w.saturating_sub(inset))
}

/// Scrollbar thumb `(y, h)` for a `visible`-row viewport over `total` items at
/// scroll offset `top`, inside a track starting at `track_y` with height
/// `track_h`. `None` when everything fits (or there is no track). Length tracks
/// the visible/total ratio so the indicator reads as "list continues".
pub fn scroll_thumb(
    track_y: u32,
    track_h: u32,
    visible: usize,
    total: usize,
    top: usize,
) -> Option<(u32, u32)> {
    if visible == 0 || total <= visible || track_h == 0 {
        return None;
    }
    let th = ((track_h as u64 * visible as u64) / total as u64).clamp(1, track_h as u64) as u32;
    let max_top = (total - visible) as u64;
    let span = track_h.saturating_sub(th) as u64;
    let ty = track_y + ((span * top.min(max_top as usize) as u64) / max_top) as u32;
    Some((ty, th))
}

fn set_span(buf: &mut [u8], w: u32, y: u32, x0: u32, x1: u32, color: Bgra) {
    let row = &mut buf[(y * w + x0) as usize * 4..(y * w + x1) as usize * 4];
    for px in row.as_chunks_mut::<4>().0 {
        *px = color;
    }
}

/// Draws the full frame. `buf` is `w*h*4` bytes in BGRA order.
/// The first row is the prompt/input row, the rest are `rows`.
/// `scale` is the buffer-to-logical density (HiDPI): `padding` and this
/// module's geometry constants are logical and are multiplied by it here.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    buf: &mut [u8],
    w: u32,
    h: u32,
    font: &crate::font::MenuFont,
    prompt: &str,
    query: &str,
    password: bool,
    rows: &[Row],
    padding: u32,
    colors: &Colors,
    scale: u32,
    caret_visible: bool,
    subpixel: Subpixel,
) {
    let s = scale.max(1);
    let row_h = font.row_h;
    // Buffer-pixel geometry derived from the logical constants.
    let p = padding * s;
    let corner = CORNER_RADIUS * s;
    let rail_w = LABEL_RAIL_W * s;
    let vpad = PILL_VPAD * s;
    let gap = PROMPT_GAP * s as f32;
    // `-P` masks the typed text; filtering still sees the real query.
    let query = masked_input(query, password);

    // Panel: rounded corners (transparent outside), input bar and list sharing
    // one outline, separated by a subtle divider and background difference.
    // One pass, one write per pixel. The buffer can be a different slot each
    // frame, so pixels outside the rounded outline must be written too — but the
    // panel itself must not be painted twice (that second full-buffer write was
    // the bulk of a HiDPI frame: 4.8 ms at scale 2 with 64 rows).
    let r = corner.min(h / 2);
    for y in 0..h {
        let (x0, x1) = rounded_span(w, h, r, y);
        let bg = if y < row_h {
            colors.bg_prompt
        } else {
            colors.bg_normal
        };
        if x0 > 0 {
            set_span(buf, w, y, 0, x0, CLEAR);
        }
        set_span(buf, w, y, x0, x1, bg);
        if x1 < w {
            set_span(buf, w, y, x1, w, CLEAR);
        }
    }

    if h > row_h {
        let divider_h = DIVIDER_H * s;
        let divider_y = row_h - divider_h;
        let (x0, x1) = rounded_span(w, h, r, divider_y);
        let mut divider = colors.bg_prompt;
        blend_px(
            &mut divider,
            [
                colors.label_accent[0],
                colors.label_accent[1],
                colors.label_accent[2],
            ],
            DIVIDER_ALPHA,
        );
        rect(buf, w, h, x0, divider_y, x1 - x0, divider_h, divider);
    }

    // Prompt / input row text.
    let baseline = row_baseline(font, 0);
    // The prompt label ("address") sits on a compact capsule, rounded on both
    // sides, so the bar background stays uniformly bg_prompt (identical to the
    // normal, prompt-less input bar).
    let mut x = p as f32;
    if !prompt.is_empty() {
        // Option F: a 4px accent rail on the bar's left edge, the label text
        // 8px past it — no enclosing shape, so the bar stays uniformly
        // bg_prompt and the visual mass is minimal.
        let rail_h = font.line_h.ceil() as u32 + 2 * vpad;
        let rail_y = (font.row_h - rail_h) / 2;
        rect(buf, w, h, p, rail_y, rail_w, rail_h, colors.label_accent);
        x = draw_text(
            buf,
            w,
            h,
            font,
            p as f32 + rail_w as f32 + gap,
            baseline,
            prompt,
            colors.fg_prompt,
            (w - 2 * p) as f32,
            subpixel,
        );
        // Eye-comfort: fixed on-grid gap between the label text and the first
        // typed character — missing inter-element spacing increases regressive
        // fixations (Rayner et al.).
        x += gap;
    }
    if !query.is_empty() {
        x = draw_text(
            buf,
            w,
            h,
            font,
            x,
            baseline,
            &query,
            colors.fg_prompt,
            w as f32 - x - p as f32,
            subpixel,
        );
    }
    // caret: 2px bar after the input text, spanning the text's ascent→descent
    // block (descent_px is negative in ab_glyph, so text bottom = baseline - descent).
    let caret_w = 2 * s;
    let caret_x = x as u32 + s;
    let caret_bottom = (baseline - font.descent_px).round() as u32;
    let caret_h = font.line_h.ceil() as u32;
    if caret_visible && caret_bottom > 0 {
        let caret_top = caret_bottom.saturating_sub(caret_h);
        rect(
            buf,
            w,
            h,
            caret_x.min(w.saturating_sub(caret_w)),
            caret_top,
            caret_w,
            caret_bottom - caret_top,
            colors.fg_prompt,
        );
    }

    // Item rows (background already bg_normal from the panel pass; only the
    // selected row needs an override, clipped to the rounded outline).
    for (i, row) in rows.iter().enumerate() {
        let y = row_h * (i as u32 + 1);
        if row.selected {
            for yy in y..(y + row_h).min(h) {
                let (x0, x1) = rounded_span(w, h, r, yy);
                set_span(buf, w, yy, x0, x1, colors.bg_sel);
            }
        }
        let fg = if row.selected {
            colors.fg_sel
        } else {
            colors.fg_normal
        };
        draw_text(
            buf,
            w,
            h,
            font,
            p as f32,
            row_baseline(font, y),
            row.text,
            fg,
            w as f32 - 2.0 * p as f32,
            subpixel,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub fn rect(buf: &mut [u8], w: u32, h: u32, x: u32, y: u32, rw: u32, rh: u32, color: Bgra) {
    let x0 = (x as i64).clamp(0, w as i64) as u32;
    let y0 = (y as i64).clamp(0, h as i64) as u32;
    let x1 = ((x + rw) as i64).clamp(0, w as i64) as u32;
    let y1 = ((y + rh) as i64).clamp(0, h as i64) as u32;
    for py in y0..y1 {
        let row = &mut buf[(py * w + x0) as usize * 4..(py * w + x1) as usize * 4];
        for px in row.as_chunks_mut::<4>().0 {
            *px = color;
        }
    }
}

/// Antialiasing mode for text. LCD subpixel modes sample coverage in three
/// horizontal sub-columns and feed them to R, G and B separately, which is
/// noticeably sharper on 1× displays; `Gray` is neutral (no colour fringing)
/// and is what `Bgr` exists for — panels wired blue-green-red need the mirror.
/// `Gray`/`Bgr` are only reachable by changing `main::TEXT_AA`, but both are
/// covered by tests (and are the documented escape hatches), so they are not
/// dead code.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subpixel {
    Gray,
    Rgb,
    Bgr,
}

/// Subpixel sample offsets, in pixels, inside one pixel column.
const SUBPIXEL_SAMPLES: f32 = 3.0;
/// BGRA byte index for the red, green and blue channels.
const RGB_CHAN: [usize; 3] = [2, 1, 0];
const BGR_CHAN: [usize; 3] = [0, 1, 2];

/// Subpixel buckets per pixel: glyphs are rasterized per bucket, so pen
/// positioning stays subpixel-accurate and still cacheable.
const BUCKETS_PER_PX: f32 = 4.0;
/// Glyph cache cap; a script with thousands of distinct glyphs resets it
/// (costing one frame) instead of growing without bound.
const GLYPH_CACHE_CAP: usize = 8192;

/// Draw `s` starting at (x, baseline), clipped to `max_w` px wide.
/// Returns the x position just past the last drawn advance.
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    buf: &mut [u8],
    w: u32,
    h: u32,
    font: &crate::font::MenuFont,
    x: f32,
    baseline: f32,
    s: &str,
    color: Bgra,
    max_w: f32,
    subpixel: Subpixel,
) -> f32 {
    let scale = || PxScale::from(font.size);
    let mut cx = x;
    let end = x + max_w;
    let mode = match subpixel {
        Subpixel::Gray => 0,
        Subpixel::Rgb => 1,
        Subpixel::Bgr => 2,
    };
    let bucket = |v: f32| ((v.fract() * BUCKETS_PER_PX) as u8).min(BUCKETS_PER_PX as u8 - 1);
    let canonical = |v: f32| (v.fract() * BUCKETS_PER_PX).floor() / BUCKETS_PER_PX;
    'ch: for ch in s.chars() {
        // Walk the face chain (primary first); draw with the first face that
        // has this glyph so CJK etc. fall back to a system font. A face beyond
        // the primary is read on first use here.
        for face_idx in 0..font.face_count() {
            let Some(face) = font.face(face_idx) else {
                continue; // unreadable face, try the next
            };
            let scaled = face.as_scaled(scale());
            let gid = scaled.glyph_id(ch);
            if gid == GlyphId(0) {
                continue; // no glyph in this face, try the next
            }
            let advance = scaled.h_advance(gid);
            if cx + advance > end {
                break 'ch;
            }
            let key: crate::font::GlyphKey =
                (face_idx as u8, gid.0, bucket(cx), bucket(baseline), mode);
            if !font.glyphs.borrow().contains_key(&key) {
                if font.glyphs.borrow().len() >= GLYPH_CACHE_CAP {
                    font.glyphs.borrow_mut().clear();
                }
                // Rasterize at the canonical pen (subpixel part only), then blit
                // at the whole-pixel part — the two differ by an integer shift.
                let planes = match subpixel {
                    // One neutral plane, used for every channel.
                    Subpixel::Gray => {
                        vec![raster(
                            &face,
                            gid,
                            scale(),
                            point(canonical(cx), canonical(baseline)),
                        )]
                    }
                    // One plane per subpixel column: each carries one channel.
                    _ => (0..3)
                        .map(|k| {
                            let off = k as f32 / SUBPIXEL_SAMPLES;
                            raster(
                                &face,
                                gid,
                                scale(),
                                point(canonical(cx) + off, canonical(baseline)),
                            )
                        })
                        .collect(),
                };
                font.glyphs
                    .borrow_mut()
                    .insert(key, crate::font::GlyphBitmap { advance, planes });
            }
            let cache = font.glyphs.borrow();
            let bmp = cache.get(&key).expect("inserted above");
            let dx = cx.floor() as i32;
            let dy = baseline.floor() as i32;
            match subpixel {
                Subpixel::Gray => blit_plane(buf, w, h, &bmp.planes[0], dx, dy, color, None),
                Subpixel::Rgb => {
                    for (k, plane) in bmp.planes.iter().enumerate() {
                        blit_plane(buf, w, h, plane, dx, dy, color, Some(RGB_CHAN[k]));
                    }
                }
                Subpixel::Bgr => {
                    for (k, plane) in bmp.planes.iter().enumerate() {
                        blit_plane(buf, w, h, plane, dx, dy, color, Some(BGR_CHAN[k]));
                    }
                }
            }
            cx += bmp.advance;
            continue 'ch;
        }
        // No face in the chain has this glyph; skip it silently.
    }
    cx
}

/// Advance width of `s` under the same face-chain + fit rules as `draw_text`
/// (returns the extent starting at 0, capped at `max_w`). Test-only: the F-rail
/// layout no longer needs it in `draw`, but the geometry tests do.
#[cfg(test)]
fn measure(font: &crate::font::MenuFont, s: &str, max_w: f32) -> f32 {
    let scale = || PxScale::from(font.size);
    let mut cx = 0.0;
    'ch: for ch in s.chars() {
        for face_idx in 0..font.face_count() {
            let Some(face) = font.face(face_idx) else {
                continue;
            };
            let scaled = face.as_scaled(scale());
            let gid = scaled.glyph_id(ch);
            if gid == GlyphId(0) {
                continue;
            }
            let adv = scaled.h_advance(gid);
            if cx + adv > max_w {
                break 'ch;
            }
            cx += adv;
            continue 'ch;
        }
    }
    cx
}

fn row_baseline(font: &crate::font::MenuFont, row_y: u32) -> f32 {
    let top = row_y as f32 + (font.row_h as f32 - font.line_h) / 2.0;
    top + font.ascent_px
}

/// Rasterize one coverage plane of `gid` at `pos`.
fn raster(
    face: &ab_glyph::FontVec,
    gid: GlyphId,
    scale: PxScale,
    pos: ab_glyph::Point,
) -> crate::font::GlyphPlane {
    let Some(outline) = face.outline_glyph(Glyph {
        id: gid,
        scale,
        position: pos,
    }) else {
        return crate::font::GlyphPlane::default();
    };
    let b = outline.px_bounds();
    // px_bounds is an f32 rect and `draw` iterates width()×height() from `min`,
    // so both must stay in the same units.
    let (w, h) = (b.width() as u32, b.height() as u32);
    let mut coverage = Vec::with_capacity((w * h) as usize);
    outline.draw(|_, _, cov| coverage.push((cov * 255.0).round() as u8));
    crate::font::GlyphPlane {
        min_x: b.min.x as i32,
        min_y: b.min.y as i32,
        w,
        h,
        coverage,
    }
}

/// Blend one coverage plane: across all channels (grayscale AA, `chan = None`)
/// or into a single channel (LCD subpixel AA). Subpixel planes touch disjoint
/// channels, so blending them in sequence is exact.
#[allow(clippy::too_many_arguments)]
fn blit_plane(
    buf: &mut [u8],
    w: u32,
    h: u32,
    plane: &crate::font::GlyphPlane,
    dx: i32,
    dy: i32,
    color: Bgra,
    chan: Option<usize>,
) {
    for gy in 0..plane.h {
        for gx in 0..plane.w {
            let cov = plane.coverage[(gy * plane.w + gx) as usize];
            if cov == 0 {
                continue;
            }
            let px = dx + plane.min_x + gx as i32;
            let py = dy + plane.min_y + gy as i32;
            if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                continue;
            }
            let a = (cov as u32 * color[3] as u32) / 255;
            if a == 0 {
                continue;
            }
            let dst = &mut buf[((py as u32 * w + px as u32) * 4) as usize..][..4];
            match chan {
                None => blend_px(dst, [color[0], color[1], color[2]], a),
                Some(c) => dst[c] = ((color[c] as u32 * a + dst[c] as u32 * (255 - a)) / 255) as u8,
            }
        }
    }
}

/// `-P` password mode: one asterisk per char; otherwise return the query unchanged.
fn masked_input(query: &str, password: bool) -> String {
    if password {
        "*".repeat(query.chars().count())
    } else {
        query.to_string()
    }
}

#[inline]
fn blend_px(dst: &mut [u8], src: [u8; 3], a: u32) {
    let inv = 255 - a;
    dst[0] = ((src[0] as u32 * a + dst[0] as u32 * inv) / 255) as u8;
    dst[1] = ((src[1] as u32 * a + dst[1] as u32 * inv) / 255) as u8;
    dst[2] = ((src[2] as u32 * a + dst[2] as u32 * inv) / 255) as u8;
    // dst alpha stays 255 (fully opaque menu)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::MenuFont;

    /// Headless render check: draws a real frame (text incl. CJK) to a raw BGRA dump.
    #[test]
    fn renders_frame_with_cjk_text() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (320u32, font.row_h * 4);
        let mut buf = vec![0u8; (w * h * 4) as usize];

        let colors = Colors::default();
        let rows = [
            Row {
                text: "Firefox 火狐浏览器",
                selected: true,
            },
            Row {
                text: "Terminal",
                selected: false,
            },
            Row {
                text: "Settings",
                selected: false,
            },
        ];
        draw(
            &mut buf,
            w,
            h,
            &font,
            "run: ",
            "fir",
            false,
            &rows,
            crate::PAD,
            &colors,
            1,
            true,
            Subpixel::Gray,
        );

        // Selected row (rows[0]) has the selection background.
        let sel_px = &buf[(font.row_h * w + w / 2) as usize * 4..][..4];
        assert_eq!(sel_px, &colors.bg_sel);
        // Normal row (rows[1]) has the normal background.
        let norm_px = &buf[(font.row_h * 2 * w + w / 2) as usize * 4..][..4];
        assert_eq!(norm_px, &colors.bg_normal);
        // Prompt row has the prompt background.
        let prompt_px = &buf[(w / 2) as usize * 4..][..4];
        assert_eq!(prompt_px, &colors.bg_prompt);
        // First text row must contain non-background pixels (glyph ink).
        let mut text_has_ink = false;
        'outer: for y in 2..24u32 {
            for x in crate::PAD..200u32 {
                let i = ((font.row_h + y) * w + x) as usize * 4;
                if [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]] != colors.bg_sel {
                    text_has_ink = true;
                    break 'outer;
                }
            }
        }
        assert!(text_has_ink, "text should be drawn over the selection row");
    }

    /// LCD subpixel AA: three planes feed R, G and B separately, so channel
    /// differences appear; `Bgr` is the mirror of `Rgb`; `Gray` stays neutral.
    #[test]
    fn subpixel_aa_is_per_channel_and_order_symmetric() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (240u32, font.row_h);
        let render = |aa: Subpixel| {
            let mut buf = vec![0u8; (w * h * 4) as usize];
            draw(
                &mut buf,
                w,
                h,
                &font,
                "",
                "firim",
                false,
                &[],
                crate::PAD,
                &Colors::default(),
                1,
                true,
                aa,
            );
            buf
        };
        let gray = render(Subpixel::Gray);
        let rgb = render(Subpixel::Rgb);
        let bgr = render(Subpixel::Bgr);

        assert_ne!(gray, rgb, "subpixel AA must change the rendering");
        assert!(
            gray.as_chunks::<4>().0.iter().all(|p| p[0] == p[2]),
            "grayscale AA must stay colour-neutral"
        );
        assert!(
            rgb.as_chunks::<4>().0.iter().any(|p| p[0] != p[2]),
            "RGB subpixel AA must produce channel differences"
        );
        // BGR is RGB with red and blue swapped, green untouched.
        for (a, b) in rgb.as_chunks::<4>().0.iter().zip(bgr.as_chunks::<4>().0) {
            assert_eq!(a[0], b[2], "blue and red are mirrored");
            assert_eq!(a[2], b[0]);
            assert_eq!(a[1], b[1], "green is shared");
        }
    }

    /// The glyph cache must render identically on the second frame and add no
    /// entries: it is reused, not just populated.
    #[test]
    fn glyph_cache_is_reused_across_frames() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (320u32, font.row_h * 2);
        let colors = Colors::default();
        let rows = [Row {
            text: "Firefox 火狐",
            selected: true,
        }];
        let mut a = vec![0u8; (w * h * 4) as usize];
        draw(
            &mut a,
            w,
            h,
            &font,
            "run:",
            "fir",
            false,
            &rows,
            crate::PAD,
            &colors,
            1,
            true,
            Subpixel::Gray,
        );
        let cached = font.glyphs.borrow().len();
        assert!(cached > 0, "the first frame must populate the cache");

        let mut b = vec![0u8; (w * h * 4) as usize];
        draw(
            &mut b,
            w,
            h,
            &font,
            "run:",
            "fir",
            false,
            &rows,
            crate::PAD,
            &colors,
            1,
            true,
            Subpixel::Gray,
        );
        assert_eq!(a, b, "cached glyphs must render exactly as the first frame");
        assert_eq!(font.glyphs.borrow().len(), cached, "reuse adds no entries");
    }

    /// Render check: latin-only primary face, CJK must still draw via fallback.
    #[test]
    fn renders_cjk_via_fallback_when_primary_is_latin() {
        let font = MenuFont::load(Some("Noto Sans Mono"), 16.0).expect("system font available");
        let (w, h) = (320u32, font.row_h);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        draw(
            &mut buf,
            w,
            h,
            &font,
            "",
            "火狐",
            false,
            &[],
            crate::PAD,
            &Colors::default(),
            1,
            true,
            Subpixel::Gray,
        );
        // Prompt row bg is BG_PROMPT; any pixel differing from it is glyph ink.
        let bg = Colors::default().bg_prompt;
        let mut ink = false;
        'o: for y in 2..h - 2 {
            for x in 2..w - 2 {
                let i = (y * w + x) as usize * 4;
                if [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]] != bg {
                    ink = true;
                    break 'o;
                }
            }
        }
        assert!(ink, "CJK glyphs must render via the fallback chain");
    }

    #[test]
    fn rounded_corner_pixels_are_transparent_inside_is_opaque() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (200u32, font.row_h * 3);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let colors = Colors::default();
        draw(
            &mut buf,
            w,
            h,
            &font,
            "",
            "fir",
            false,
            &[],
            crate::PAD,
            &colors,
            1,
            true,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[((y * w + x) * 4) as usize..][..4];

        // All four corner pixels are fully transparent.
        assert_eq!(px(0, 0), &[0, 0, 0, 0]);
        assert_eq!(px(w - 1, 0), &[0, 0, 0, 0]);
        assert_eq!(px(0, h - 1), &[0, 0, 0, 0]);
        assert_eq!(px(w - 1, h - 1), &[0, 0, 0, 0]);
        // The interior near a corner is opaque and opaque panel-colored.
        assert_eq!(px(6, 2)[3], 0xff);
        assert_eq!(px(w / 2, h / 2), &colors.bg_normal);
    }

    #[test]
    fn rounded_span_outline_geometry() {
        let (w, h, r) = (200u32, 40u32, 10u32);
        // Top corner row starts inset from the left edge (curve), shrinks to full width.
        let (x0, _) = rounded_span(w, h, r, 0);
        assert!(x0 > 0 && x0 < r);
        // Middle rows are full width.
        assert_eq!(rounded_span(w, h, r, 20), (0, w));
        // Bottom corner row ends before the right edge.
        let (_, x1) = rounded_span(w, h, r, h - 1);
        assert!(x1 > w - r && x1 < w);
        // Zero radius = full rect.
        assert_eq!(rounded_span(w, h, 0, 0), (0, w));
        // Radius clamped to half the height: nearly full-width spans, not deep corners.
        let (x0, x1) = rounded_span(w, 2, 10, 0);
        assert!(x0 <= 1 && x1 >= w - 1);
    }

    #[test]
    fn prompt_bar_and_list_have_distinct_backgrounds() {
        let def = Colors::default();
        assert_ne!(
            def.bg_prompt, def.bg_normal,
            "input bar color must differ from list color"
        );

        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (200u32, font.row_h * 2);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        draw(
            &mut buf,
            w,
            h,
            &font,
            "",
            "x",
            false,
            &[Row {
                text: "app",
                selected: false,
            }],
            crate::PAD,
            &def,
            1,
            true,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[((y * w + x) * 4) as usize..][..4];
        // Input bar row uses bg_prompt, the list row below it bg_normal.
        assert_eq!(px(w / 2, 2), &def.bg_prompt);
        assert_eq!(px(w / 2, font.row_h + 2), &def.bg_normal);
    }

    #[test]
    fn selected_row_covers_full_width_and_list_has_divider() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (240u32, font.row_h * 2);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let colors = Colors::default();
        draw(
            &mut buf,
            w,
            h,
            &font,
            "",
            "",
            false,
            &[Row {
                text: "",
                selected: true,
            }],
            crate::PAD,
            &colors,
            1,
            false,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[((y * w + x) * 4) as usize..][..4];

        let selection_y = font.row_h + 1;
        assert_eq!(px(0, selection_y), &colors.bg_sel);
        assert_eq!(px(w - 1, selection_y), &colors.bg_sel);
        assert_eq!(px(0, h - 1), &CLEAR, "outer corner stays transparent");
        assert_eq!(px(w - 1, h - 1), &CLEAR);

        let divider = px(w / 2, font.row_h - 1);
        assert_ne!(divider, &colors.bg_prompt);
        assert_ne!(divider, &colors.bg_normal);
        for (c, value) in divider.iter().copied().take(3).enumerate() {
            assert!(value >= colors.bg_prompt[c].min(colors.label_accent[c]));
            assert!(value <= colors.bg_prompt[c].max(colors.label_accent[c]));
        }
    }

    #[test]
    fn prompt_label_is_a_left_rail_over_an_uniform_bar() {
        let def = Colors::default();
        assert_ne!(
            def.label_accent, def.bg_prompt,
            "label accent must differ from the bar"
        );

        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (240u32, font.row_h);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        draw(
            &mut buf,
            w,
            h,
            &font,
            "address",
            "",
            false,
            &[],
            crate::PAD,
            &def,
            1,
            true,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[((y * w + x) * 4) as usize..][..4];

        let rail_h = font.line_h.ceil() as u32 + 2 * PILL_VPAD;
        let rail_y = (font.row_h - rail_h) / 2;
        let mid = font.row_h / 2;

        // A 4px rail sits flush on the bar's left padding; everything else —
        // just past the rail, between rail and label text, and the far right —
        // is bg_prompt, so the bar reads like the normal, prompt-less input bar.
        assert_eq!(px(crate::PAD, mid), &def.label_accent, "rail left edge");
        assert_eq!(
            px(crate::PAD + LABEL_RAIL_W - 1, mid),
            &def.label_accent,
            "rail right edge"
        );
        assert_eq!(
            px(crate::PAD + LABEL_RAIL_W, mid),
            &def.bg_prompt,
            "just past the rail"
        );
        assert_eq!(
            px(crate::PAD + 8, mid),
            &def.bg_prompt,
            "between rail and label text"
        );
        assert_eq!(px(w - 10, mid), &def.bg_prompt);
        // Rail is vertically centered at the same height the capsule had.
        assert_eq!(px(crate::PAD, rail_y), &def.label_accent, "rail top");
        assert_eq!(px(crate::PAD, rail_y - 1), &def.bg_prompt, "above the rail");
        assert_eq!(
            px(crate::PAD, rail_y + rail_h),
            &def.bg_prompt,
            "below the rail"
        );

        // Empty prompt → no rail; the whole bar stays bg_prompt.
        let mut buf2 = vec![0u8; (w * h * 4) as usize];
        draw(
            &mut buf2,
            w,
            h,
            &font,
            "",
            "x",
            false,
            &[],
            crate::PAD,
            &def,
            1,
            true,
            Subpixel::Gray,
        );
        assert_eq!(
            &buf2[((mid * w + crate::PAD) * 4) as usize..][..4],
            &def.bg_prompt
        );
    }

    /// Caret hidden while blinked off: no solid fg_prompt column.
    #[test]
    fn caret_can_be_hidden_for_blinking() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (200u32, font.row_h);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let colors = Colors::default();
        draw(
            &mut buf,
            w,
            h,
            &font,
            "",
            "fir",
            false,
            &[],
            crate::PAD,
            &colors,
            1,
            false,
            Subpixel::Gray,
        );
        let mid = font.row_h / 2;
        let px = |x: u32, y: u32| &buf[((y * w + x) * 4) as usize..][..4];
        let solid = (1..w - 3)
            .any(|x| px(x, mid) == colors.fg_prompt && px(x + 1, mid) == colors.fg_prompt);
        assert!(!solid, "blinked-off caret must not be drawn");
    }

    /// Clipping: text longer than `max_w` stops at the limit and never writes
    /// past it. This is the boundary a future ellipsis / match highlight builds
    /// on, and it guards against overflow regressions.
    #[test]
    fn draw_text_clips_at_max_width_without_overflow() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (400u32, font.row_h);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let colors = Colors::default();
        let (start, max_w) = (crate::PAD as f32, 60.0f32);
        let limit = (start + max_w).ceil() as u32;
        let long = "W".repeat(200);

        let drawn = draw_text(
            &mut buf,
            w,
            h,
            &font,
            start,
            row_baseline(&font, 0),
            &long,
            colors.fg_prompt,
            max_w,
            Subpixel::Gray,
        );
        assert!(drawn <= start + max_w, "advance {drawn} ran past the limit");
        let ink = |px: &[u8]| px[..3] != [0, 0, 0];
        // Something was drawn inside the limit...
        let inside = (0..h)
            .flat_map(|y| (0..limit).map(move |x| (x, y)))
            .any(|(x, y)| ink(&buf[((y * w + x) * 4) as usize..][..4]));
        assert!(
            inside,
            "the clipped text should still draw inside the limit"
        );
        // ...and nothing at or past it.
        for y in 0..h {
            for x in limit..w {
                let px = &buf[((y * w + x) * 4) as usize..][..4];
                assert!(!ink(px), "ink past the clip limit at ({x},{y})");
            }
        }
    }

    #[test]
    fn scroll_thumb_only_when_overflowing_and_tracks_position() {
        assert_eq!(
            scroll_thumb(24, 240, 10, 10, 0),
            None,
            "no overflow, no indicator"
        );
        assert_eq!(scroll_thumb(24, 240, 10, 0, 0), None, "empty list");
        assert_eq!(
            scroll_thumb(24, 0, 10, 100, 0),
            None,
            "collapsed bar has no track"
        );
        let (y0, h0) = scroll_thumb(24, 240, 10, 100, 0).expect("overflows");
        assert_eq!(y0, 24, "at the top the thumb starts at the track top");
        assert_eq!(h0, 24, "length tracks the visible/total ratio");
        let (y1, h1) = scroll_thumb(24, 240, 10, 100, 90).expect("overflows");
        assert_eq!(
            y1 + h1,
            24 + 240,
            "at the end the thumb ends at the track bottom"
        );
        let (ym, hm) = scroll_thumb(24, 240, 10, 100, 45).expect("overflows");
        assert!(ym > y0 && ym < y1, "mid-scroll sits between the ends");
        assert_eq!(hm, h0);
    }

    /// WCAG relative luminance of an opaque BGRA pixel (index 0 is blue).
    fn luminance(px: Bgra) -> f64 {
        let chan = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * chan(px[2]) + 0.7152 * chan(px[1]) + 0.0722 * chan(px[0])
    }

    fn contrast(a: Bgra, b: Bgra) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// The palette is what users actually read. The selected row used to sit at
    /// 4.06:1 (below WCAG AA) while every other pair was above 8:1.
    #[test]
    fn default_palette_meets_wcag_aa_for_text() {
        let c = Colors::default();
        for (name, fg, bg) in [
            ("normal", c.fg_normal, c.bg_normal),
            ("prompt", c.fg_prompt, c.bg_prompt),
            ("selected", c.fg_sel, c.bg_sel),
        ] {
            let r = contrast(fg, bg);
            assert!(
                r >= 4.5,
                "{name} text contrast {r:.2}:1 is below WCAG AA (4.5:1)"
            );
        }
    }

    /// HiDPI: geometry constants are logical and scale with the buffer density.
    #[test]
    fn hidpi_scale_doubles_the_buffer_geometry() {
        let font = MenuFont::load(None, 32.0).expect("system font available");
        let (w, h) = (480u32, font.row_h * 2);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let def = Colors::default();
        draw(
            &mut buf,
            w,
            h,
            &font,
            "address",
            "",
            false,
            &[Row {
                text: "",
                selected: true,
            }],
            crate::PAD,
            &def,
            2,
            true,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[((y * w + x) * 4) as usize..][..4];
        let mid = font.row_h / 2;

        // Padding 16 -> 32, rail width 4 -> 8.
        let rail_x = crate::PAD * 2;
        assert_eq!(
            px(rail_x, mid),
            &def.label_accent,
            "rail starts at scaled padding"
        );
        assert_eq!(
            px(rail_x + 2 * LABEL_RAIL_W - 1, mid),
            &def.label_accent,
            "rail is 2x wide"
        );
        assert_eq!(
            px(rail_x + 2 * LABEL_RAIL_W, mid),
            &def.bg_prompt,
            "past the scaled rail"
        );

        let selection_y = font.row_h + 1;
        assert_eq!(px(0, selection_y), &def.bg_sel);
        assert_eq!(px(w - 1, selection_y), &def.bg_sel);
        assert_ne!(
            px(w / 2, font.row_h - 1),
            &def.bg_prompt,
            "logical divider scales to physical pixels"
        );

        // Panel corner radius 12 -> 24: the first buffer row is inset further.
        let (x0, _) = rounded_span(w, h, CORNER_RADIUS * 2, 0);
        assert!(x0 > CORNER_RADIUS, "corner inset scales with density: {x0}");
    }

    #[test]
    fn password_mode_masks_query_with_asterisks() {
        assert_eq!(masked_input("hunter2", true), "*******");
        assert_eq!(masked_input("中文", true), "**");
        assert_eq!(masked_input("hunter2", false), "hunter2");
    }

    #[test]
    fn spacing_follows_eye_comfort_rules() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        // Row height ≈1.5× the font: glyphs don't crowd, the selection band is a
        // comfortable target. (line_h == font size by PxScale definition.)
        assert!(
            font.row_h as f32 >= font.line_h * 1.4,
            "row {} should be >= 1.4x line {}",
            font.row_h,
            font.line_h
        );
        // Content inset vs corner radius and the top margin are compile-time
        // guards at the top of this file; the font check below is runtime.
        assert!(
            font.row_h as f32 >= font.line_h * 1.4,
            "row {} should be >= 1.4x line {}",
            font.row_h,
            font.line_h
        );
    }

    #[test]
    fn parses_wmenu_colors() {
        // RRGGBB → BGRA [b, g, r, 0xff]
        assert_eq!(parse_color("bb2222"), Some([0x22, 0x22, 0xbb, 0xff]));
        assert_eq!(parse_color("005577"), Some([0x77, 0x55, 0x00, 0xff]));
        // RRGGBBAA → [b, g, r, a]
        assert_eq!(parse_color("11223344"), Some([0x33, 0x22, 0x11, 0x44]));
        assert_eq!(parse_color("12345"), None);
        assert_eq!(parse_color("zzzzzz"), None);
        // wmenu-style '#' prefix is accepted; malformed inputs still rejected.
        assert_eq!(parse_color("#bb2222"), Some([0x22, 0x22, 0xbb, 0xff]));
        assert_eq!(parse_color("#11223344"), Some([0x33, 0x22, 0x11, 0x44]));
        assert_eq!(parse_color("##123456"), None);
        assert_eq!(parse_color("#12345"), None);
    }
    // caret geometry: solid 2px bar right after the query text, spanning the
    // text's ascent→descent block (not hidden by row height, not shifted by the
    // negative descent that ab_glyph reports).
    #[cfg(test)]
    mod caret_tests {
        use super::*;
        use crate::font::MenuFont;

        #[test]
        fn caret_is_solid_bar_just_past_query_spanning_text_block() {
            let font = MenuFont::load(None, 16.0).expect("system font available");
            let (w, h) = (200u32, font.row_h);
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let colors = Colors::default();
            draw(
                &mut buf,
                w,
                h,
                &font,
                "",
                "fir",
                false,
                &[],
                crate::PAD,
                &colors,
                1,
                true,
                Subpixel::Gray,
            );
            let px = |x: u32, y: u32| &buf[((y * w + x) * 4) as usize..][..4];

            // A solid (non-antialiased) 2px-wide fg_prompt column = the caret;
            // glyph ink is always anti-aliased, so it can't fake this.
            let mid = font.row_h / 2;
            let mut caret_x = None;
            for x in 1..w - 3 {
                if px(x, mid) == colors.fg_prompt
                    && px(x + 1, mid) == colors.fg_prompt
                    && px(x - 1, mid) != colors.fg_prompt
                    && px(x + 2, mid) != colors.fg_prompt
                {
                    caret_x = Some(x);
                    break;
                }
            }
            let x = caret_x.expect("caret must be drawn after the query text");
            // Caret sits just past the text (starts at the panel padding).
            assert!(x > crate::PAD + 4, "caret at x={x} should follow the text");
            // Vertical span == the line block centered in the (taller) row.
            let line_h = font.line_h.ceil() as u32;
            let top_expected = (font.row_h - line_h) / 2;
            let mut top = None;
            let mut bottom = None;
            for y in 0..font.row_h {
                let solid = px(x, y) == colors.fg_prompt && px(x + 1, y) == colors.fg_prompt;
                if solid && top.is_none() {
                    top = Some(y);
                }
                if solid {
                    bottom = Some(y);
                }
            }
            assert_eq!(
                top,
                Some(top_expected),
                "caret must start at the text block top"
            );
            assert_eq!(
                bottom,
                Some(top_expected + line_h - 1),
                "caret must span the full text block"
            );
        }

        #[test]
        fn prompt_gap_separates_pill_from_input() {
            let font = MenuFont::load(None, 16.0).expect("system font available");
            let (w, h) = (300u32, font.row_h);
            let colors = Colors::default();
            let mut buf = vec![0u8; (w * h * 4) as usize];
            // Locate the solid 2px caret (same detector as the test above) for a
            // given prompt; the query "fir" and caret stay identical in both frames.
            let mut caret_x = |prompt: &str| -> u32 {
                buf.fill(0);
                draw(
                    &mut buf,
                    w,
                    h,
                    &font,
                    prompt,
                    "fir",
                    false,
                    &[],
                    crate::PAD,
                    &colors,
                    1,
                    true,
                    Subpixel::Gray,
                );
                let mid = font.row_h / 2;
                let at = |x: u32| &buf[((mid * w + x) * 4) as usize..][..4];
                for x in 1..w - 3 {
                    if at(x) == colors.fg_prompt
                        && at(x + 1) == colors.fg_prompt
                        && at(x - 1) != colors.fg_prompt
                        && at(x + 2) != colors.fg_prompt
                    {
                        return x;
                    }
                }
                panic!("caret not found for prompt {prompt:?}");
            };
            // With a prompt, the caret shifts right by the prompt's advance plus
            // the pill's two horizontal paddings and the inter-element gap; without
            // one it starts straight at the padding. f32 sums can round the
            // measured delta up by one pixel.
            let advance = measure(&font, "run:", (w - 2 * crate::PAD) as f32) as u32;
            // Rail (4px) + label gap (8px) + inter-element gap (8px).
            let separators = (LABEL_RAIL_W as f32 + 2.0 * PROMPT_GAP) as u32;
            let delta = caret_x("run:") - caret_x("") - advance;
            assert!(
                (separators..=separators + 1).contains(&delta),
                "label and input must be separated by ~{separators}px, got {delta}"
            );
        }
    }
}
