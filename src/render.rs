//! Software renderer: draws the menu into an ARGB8888 (BGRA byte order) shm buffer.
//! Pure CPU, no Wayland dependency, so it is unit-testable headless.

use ab_glyph::{Font, Glyph, GlyphId, PxScale, ScaleFont, point};

pub type Bgra = [u8; 4];

pub const fn bgra(r: u8, g: u8, b: u8, a: u8) -> Bgra {
    [b, g, r, a]
}

/// Spotlight panel corner radius (logical px), clamped to half the panel size.
pub const CORNER_RADIUS: u32 = 12;

/// Space reserved around the panel for its drop shadow (logical px). The buffer
/// and the layer surface grow by it; the panel keeps its size and position.
pub const SHADOW: u32 = 24;

/// Shadow layers as (blur radius, alpha at the panel edge, vertical offset),
/// logical px and 0-255. A tight contact shadow under a wide ambient one; the
/// offset is positive because the light comes from above.
const SHADOW_TIGHT: (u32, u32, u32) = (7, 170, 2);
const SHADOW_WIDE: (u32, u32, u32) = (17, 130, 6);
// The margin must fit the widest layer, or its falloff is cut off at the buffer edge.
const _: () = assert!(SHADOW >= SHADOW_WIDE.0 + SHADOW_WIDE.2);

/// Hairline on the panel's outermost pixel — the seam between the translucent
/// body and the shadow. Lit along the top, dark underneath, like window chrome.
const BORDER_LIGHT_ALPHA: u32 = 34;
const BORDER_DARK_ALPHA: u32 = 60;

// Eye-comfort rules: the content inset must clear the corner radius so text never
// grazes the curve — `main::pad_for` floors it at `CORNER_RADIUS` — and the panel
// must float off the screen edge rather than hug it, which is compile-time.
const _: () = assert!(crate::TOP_MARGIN >= 32);

/// Fully transparent pixel: what the shadow margin holds every frame.
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

/// Translucent glass body: the alpha is what the compositor blends with the
/// wallpaper, so the buffer is premultiplied (`draw` does that).
pub const BG_NORMAL: Bgra = bgra(0x1e, 0x1e, 0x1e, 0xd4);
pub const FG_NORMAL: Bgra = bgra(0xbb, 0xbb, 0xbb, 0xff);
/// Input bar: a subtly lighter tone than the list, so bar and list read as
/// separate pieces of one panel.
pub const BG_PROMPT: Bgra = bgra(0x2c, 0x2c, 0x2c, 0xdc);
pub const FG_PROMPT: Bgra = bgra(0xee, 0xee, 0xee, 0xff);
/// Prompt label accent: a 4px rail on the input bar's left edge (option F); a
/// deep slate blue that carries the emphasis while staying harmonious with the
/// gray bar and apart from the selection blue.
pub const LABEL_ACCENT: Bgra = bgra(0x2e, 0x4a, 0x5c, 0xff);
/// Selected row. The background is darker than the old `0f7ca6` so white text
/// clears WCAG AA: `#ffffff` on `#0b6285` is 6.77:1 (the previous pair was
/// 4.06:1 — the weakest contrast in the whole UI, on the row that matters most).
pub const BG_SEL: Bgra = bgra(0x0b, 0x62, 0x85, 0xee);
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
    if y >= h {
        return (0, 0);
    }
    let inset = rounded_inset(w, h, r, y).ceil() as u32;
    (inset.min(w), w.saturating_sub(inset))
}

/// Fractional horizontal inset of the rounded outline at row `y`. Rounding it
/// up gives the whole-pixel span; the fraction left over is the coverage of the
/// pixel just outside that span, which is what antialiases the edge.
pub fn rounded_inset(w: u32, h: u32, r: u32, y: u32) -> f64 {
    let r = r.min(w / 2).min(h / 2);
    if r == 0 || y >= h {
        return 0.0;
    }
    let rr = (r * r) as f64;
    let py = y as f64 + 0.5;
    let top = r as f64;
    let bottom = h as f64 - r as f64;
    if py >= top && py <= bottom {
        return 0.0;
    }
    let dy = if py < top { top - py } else { py - bottom };
    r as f64 - (rr - dy * dy).max(0.0).sqrt()
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

/// Premultiply a straight colour: wl_shm ARGB8888 is premultiplied, so a
/// translucent body has to be written as `rgb * a`, not as `rgb` next to an
/// alpha byte (the compositor would blend a too-bright panel).
fn premul(c: Bgra) -> Bgra {
    let a = c[3] as u32;
    [
        (c[0] as u32 * a / 255) as u8,
        (c[1] as u32 * a / 255) as u8,
        (c[2] as u32 * a / 255) as u8,
        c[3],
    ]
}

/// Composite an already premultiplied source over a premultiplied destination
/// with `cov` (0-255) attenuating it — edge coverage, or a border layer.
#[inline]
fn over_pm(dst: &mut [u8], src: Bgra, cov: u32) {
    let sa = src[3] as u32 * cov / 255;
    let inv = 255 - sa;
    for c in 0..3 {
        dst[c] = ((src[c] as u32 * cov / 255) + dst[c] as u32 * inv / 255) as u8;
    }
    dst[3] = (sa + dst[3] as u32 * inv / 255) as u8;
}

/// The panel inside the shadow margin: origin and size in buffer px, plus the
/// rounded outline that the body, its antialiased edge, the hairline and the
/// shadow are all derived from. One outline for all four is what keeps the
/// border and the shadow from leaving a seam between them.
#[derive(Clone, Copy)]
struct Panel {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    r: u32,
}

impl Panel {
    /// Whole-pixel span `[x0, x1)` of panel-relative row `rel` in buffer coords.
    fn span(&self, rel: u32) -> (u32, u32) {
        let (x0, x1) = rounded_span(self.w, self.h, self.r, rel);
        (self.x + x0, self.x + x1)
    }
}

/// Both drop-shadow layers in one pass. They share the panel outline, so each
/// pixel gets its alpha from both LUTs and is written once: no read-modify-write
/// between layers, and the band needs no clear (which is why the frame does not
/// start with a full-buffer memset).
fn shadow(buf: &mut [u8], w: u32, h: u32, panel: &Panel, scale: u32) {
    // (blur, vertical offset, alpha by squared distance)
    let s = scale.max(1);
    let layers: [(u32, i64, Vec<u32>); 2] =
        [SHADOW_WIDE, SHADOW_TIGHT].map(|(blur, a0, offset)| {
            let b = (blur * s).max(1);
            let offset = (offset * s) as i64;
            let lut = (0..=b * b)
                .map(|d2| {
                    // A blurred edge starts at half the peak and eases out: a ramp
                    // from the full value leaves a visible cusp where it begins.
                    let t = (d2 as f64).sqrt() / b as f64;
                    (a0 as f64 * 0.5 * (1.0 + (std::f64::consts::PI * t).cos())).round() as u32
                })
                .collect();
            (b, offset, lut)
        });
    let reach = layers.iter().map(|l| l.0).max().unwrap_or(1);
    // Alpha by horizontal distance, rebuilt per row: the two LUTs are combined
    // once per column distance instead of once per shadow pixel.
    let mut band = vec![0u8; reach as usize + 1];
    // Rows inside the panel all share dv = 0, so the band is usually built once.
    let mut cached = [u32::MAX; 2];
    for by in 0..h {
        let y = by as i64;
        // Per layer: squared row distance from the shifted body, clamped to the
        // layer's blur (the offset is what hangs the shadow below the panel).
        let dv: [u32; 2] = [0, 1].map(|k| {
            let (b, offset, _) = &layers[k];
            let top = panel.y as i64 + *offset;
            let bottom = top + panel.h as i64;
            let d = if y < top {
                top - y
            } else if y >= bottom {
                y - bottom + 1
            } else {
                0
            };
            let d = d.min(*b as i64) as u32;
            d * d
        });
        // The outline row comes from the body itself, not from the offset shape:
        // both layers must see the same span, or their falloffs leave a stripe
        // where one starts further out than the other.
        let rel = (y - panel.y as i64).clamp(0, panel.h as i64 - 1) as u32;
        let (x0, x1) = panel.span(rel);
        let start = x0.saturating_sub(reach);
        let end = (x1 + reach).min(w);
        // Everything the band does not cover is emptied: a reused buffer slot
        // still holds the previous frame.
        if start > 0 {
            set_span(buf, w, by, 0, start, CLEAR);
        }
        if end < w {
            set_span(buf, w, by, end, w, CLEAR);
        }
        let visible = (0..2).any(|k| dv[k] < layers[k].0 * layers[k].0);
        if !visible {
            if start < end {
                set_span(buf, w, by, start, end, CLEAR);
            }
            continue;
        }
        if dv != cached {
            cached = dv;
            for (dx, out) in band.iter_mut().enumerate() {
                let dx2 = (dx * dx) as u32;
                let mut a = 0u32;
                for (k, (b, _, lut)) in layers.iter().enumerate() {
                    let la = lut[(dx2 + dv[k]).min(b * b) as usize];
                    a = la + div255(a * (255 - la));
                }
                *out = a as u8;
            }
        }
        // Two bands outside the outline; the body itself is painted later, so
        // it is stepped over rather than tested per pixel.
        if by >= panel.y && by < panel.y + panel.h {
            put_band(buf, w, by, start..x0.min(end), (x0, x1), Side::Left, &band);
            put_band(buf, w, by, x1.max(start)..end, (x0, x1), Side::Right, &band);
        } else {
            put_band(buf, w, by, start..end, (x0, x1), Side::Row, &band);
        }
    }
}

/// Which part of an outline row a shadow band covers.
#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
    /// The whole row: the body is not there, so the middle is shadow too.
    Row,
}

/// Writes one row's shadow band: premultiplied black, so only the alpha byte.
fn put_band(
    buf: &mut [u8],
    w: u32,
    by: u32,
    xs: std::ops::Range<u32>,
    (x0, x1): (u32, u32),
    side: Side,
    band: &[u8],
) {
    for x in xs {
        let dx = match side {
            Side::Left => x0 - 1 - x,
            Side::Right => x - x1,
            Side::Row if x < x0 => x0 - 1 - x,
            Side::Row if x >= x1 => x - x1,
            Side::Row => 0,
        };
        let a = band[(dx as usize).min(band.len() - 1)];
        let dst = &mut buf[((by * w + x) * 4) as usize..][..4];
        dst.copy_from_slice(&[0, 0, 0, a]);
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
    let mo = SHADOW * s;
    let rail_w = LABEL_RAIL_W * s;
    let vpad = PILL_VPAD * s;
    let gap = PROMPT_GAP * s as f32;
    // `-P` masks the typed text; filtering still sees the real query.
    let query = masked_input(query, password);

    // The buffer is the panel plus its shadow margin. The shadow pass also
    // empties everything it does not paint (the buffer can be a different slot
    // each frame), so there is no separate clear.
    if w <= 2 * mo || h <= 2 * mo {
        buf.fill(0);
        return;
    }
    let panel = Panel {
        x: mo,
        y: mo,
        w: w - 2 * mo,
        h: h - 2 * mo,
        r: CORNER_RADIUS * s,
    };
    shadow(buf, w, h, &panel, s);

    // Body: whole-pixel spans, premultiplied, one write per pixel. What is left
    // of the fractional corner inset is the coverage of the single pixel just
    // outside the span, which is what antialiases the rounded edge (no extra
    // pass, no per-pixel distance field).
    let (bg_normal, bg_prompt, bg_sel) = (
        premul(colors.bg_normal),
        premul(colors.bg_prompt),
        premul(colors.bg_sel),
    );
    for by in panel.y..panel.y + panel.h {
        let rel = by - panel.y;
        let inset = rounded_inset(panel.w, panel.h, panel.r, rel);
        let (x0, x1) = panel.span(rel);
        let bg = if rel < row_h { bg_prompt } else { bg_normal };
        set_span(buf, w, by, x0, x1, bg);
        let cov = ((inset.ceil() - inset) * 255.0).round() as u32;
        if cov > 0 && x0 > 0 {
            over_pm(&mut buf[(by * w + x0 - 1) as usize * 4..][..4], bg, cov);
        }
        if cov > 0 && x1 < w {
            over_pm(&mut buf[(by * w + x1) as usize * 4..][..4], bg, cov);
        }
        // Hairline on the panel's outermost pixel: the seam where the body meets
        // the shadow. Lit along the top (fading out over the first third), dark
        // underneath, so the shadow reads as growing out of the edge.
        let t = 1.0 - rel as f32 / panel.h as f32;
        let light = (BORDER_LIGHT_ALPHA as f32 * t * t).round() as u32;
        if x0 < x1 {
            for x in [x0, x1 - 1] {
                let dst = &mut buf[(by * w + x) as usize * 4..][..4];
                over_pm(dst, [0, 0, 0, 0xff], BORDER_DARK_ALPHA);
                over_pm(dst, [0xff, 0xff, 0xff, 0xff], light);
            }
        }
    }

    if panel.h > row_h {
        let divider_h = DIVIDER_H * s;
        let divider_y = row_h - divider_h;
        // Inset by the content padding: the hairline separates bar from list
        // without touching the border ring.
        let (x0, x1) = panel.span(divider_y);
        let mut divider = bg_prompt;
        over_pm(&mut divider, colors.label_accent, DIVIDER_ALPHA);
        let (dx0, dx1) = (x0 + p, x1.saturating_sub(p));
        for by in divider_y..divider_y + divider_h {
            if dx0 < dx1 {
                set_span(buf, w, panel.y + by, dx0, dx1, divider);
            }
        }
    }

    // Prompt / input row text.
    let left = panel.x as f32;
    let right = (panel.x + panel.w) as f32;
    let baseline = row_baseline(font, panel.y);
    // The prompt label ("address") sits on a compact capsule, rounded on both
    // sides, so the bar background stays uniformly bg_prompt (identical to the
    // normal, prompt-less input bar).
    let mut x = left + p as f32;
    if !prompt.is_empty() {
        // Option F: a 4px accent rail on the bar's left edge, the label text
        // 8px past it — no enclosing shape, so the bar stays uniformly
        // bg_prompt and the visual mass is minimal.
        let rail_h = font.line_h.ceil() as u32 + 2 * vpad;
        let rail_y = (font.row_h - rail_h) / 2;
        rect(
            buf,
            w,
            h,
            panel.x + p,
            panel.y + rail_y,
            rail_w,
            rail_h,
            colors.label_accent,
        );
        x = draw_text(
            buf,
            w,
            h,
            font,
            left + p as f32 + rail_w as f32 + gap,
            baseline,
            prompt,
            colors.fg_prompt,
            (panel.w - 2 * p) as f32,
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
            right - x - p as f32,
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
            caret_x.min((right as u32).saturating_sub(caret_w)),
            caret_top,
            caret_w,
            caret_bottom - caret_top,
            colors.fg_prompt,
        );
    }

    // Item rows (background already bg_normal from the panel pass; only the
    // selected row needs an override, clipped to the rounded outline).
    for (i, row) in rows.iter().enumerate() {
        let rel = row_h * (i as u32 + 1);
        if rel >= panel.h {
            break;
        }
        if row.selected {
            for yy in rel..(rel + row_h).min(panel.h) {
                // Inside the hairline: the ring frames the selection instead of
                // being painted over by it.
                let (x0, x1) = panel.span(yy);
                if x0 + 1 < x1 {
                    set_span(buf, w, panel.y + yy, x0 + 1, x1 - 1, bg_sel);
                }
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
            left + p as f32,
            row_baseline(font, panel.y + rel),
            row.text,
            fg,
            (panel.w - 2 * p) as f32,
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
            // Composite, not overwrite: the panel under it may be translucent.
            blend_px(px, [color[0], color[1], color[2]], color[3] as u32);
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
                Some(c) => {
                    dst[c] = ((color[c] as u32 * a + dst[c] as u32 * (255 - a)) / 255) as u8;
                    // Each subpixel plane covers part of the pixel; the planes
                    // together lerp the alpha towards opaque, as `blend_px` does.
                    dst[3] = (a + dst[3] as u32 * (255 - a) / 255) as u8;
                }
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

/// `x / 255` for `x <= 255 * 255`, without the hardware divide: this runs once
/// per shadow pixel per layer, where the compiler's `/ 255` costs more than the
/// rest of the pixel put together.
#[inline]
fn div255(x: u32) -> u32 {
    (x + 1 + (x >> 8)) >> 8
}

#[inline]
fn blend_px(dst: &mut [u8], src: [u8; 3], a: u32) {
    let inv = 255 - a;
    dst[0] = ((src[0] as u32 * a + dst[0] as u32 * inv) / 255) as u8;
    dst[1] = ((src[1] as u32 * a + dst[1] as u32 * inv) / 255) as u8;
    dst[2] = ((src[2] as u32 * a + dst[2] as u32 * inv) / 255) as u8;
    // src is opaque here (its alpha is already folded into `a`), so "src over"
    // in premultiplied space also moves the destination alpha towards opaque —
    // without this, ink over a translucent panel would stay translucent.
    dst[3] = (a + dst[3] as u32 * inv / 255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::MenuFont;

    /// Content inset the tests render at — the app derives it from the font size
    /// (`pad_for`), this is the value at the 20 px default.
    const PAD: u32 = 16;

    /// Buffer size for a panel of `w`×`h` buffer px at `scale`: the panel plus
    /// its shadow margin on both sides.
    fn frame_size(w: u32, h: u32, scale: u32) -> (u32, u32) {
        (w + 2 * SHADOW * scale, h + 2 * SHADOW * scale)
    }

    /// Byte index of panel-relative (x, y) in a buffer `bw` px wide.
    fn idx(bw: u32, scale: u32, x: u32, y: u32) -> usize {
        let mo = SHADOW * scale;
        (((y + mo) * bw + x + mo) * 4) as usize
    }

    /// A panel colour as the buffer holds it: premultiplied.
    fn pm(c: Bgra) -> Bgra {
        premul(c)
    }

    /// Headless render check: draws a real frame (text incl. CJK) to a raw BGRA dump.
    #[test]
    fn renders_frame_with_cjk_text() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (320u32, font.row_h * 4);
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];

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
            bw,
            bh,
            &font,
            "run: ",
            "fir",
            false,
            &rows,
            PAD,
            &colors,
            1,
            true,
            Subpixel::Gray,
        );

        let px = |x: u32, y: u32| &buf[idx(bw, 1, x, y)..][..4];
        // Selected row (rows[0]) has the selection background.
        assert_eq!(px(w / 2, font.row_h + 1), &pm(colors.bg_sel));
        // Normal row (rows[1]) has the normal background.
        assert_eq!(px(w / 2, font.row_h * 2 + 1), &pm(colors.bg_normal));
        // Prompt row has the prompt background.
        assert_eq!(px(w / 2, 1), &pm(colors.bg_prompt));
        // First text row must contain non-background pixels (glyph ink).
        let mut text_has_ink = false;
        'outer: for y in 2..24u32 {
            for x in PAD..200u32 {
                if px(x, font.row_h + y) != pm(colors.bg_sel) {
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
        let (bw, bh) = frame_size(w, h, 1);
        let render = |aa: Subpixel| {
            let mut buf = vec![0u8; (bw * bh * 4) as usize];
            draw(
                &mut buf,
                bw,
                bh,
                &font,
                "",
                "firim",
                false,
                &[],
                PAD,
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
        let (bw, bh) = frame_size(w, h, 1);
        let colors = Colors::default();
        let rows = [Row {
            text: "Firefox 火狐",
            selected: true,
        }];
        let mut a = vec![0u8; (bw * bh * 4) as usize];
        draw(
            &mut a,
            bw,
            bh,
            &font,
            "run:",
            "fir",
            false,
            &rows,
            PAD,
            &colors,
            1,
            true,
            Subpixel::Gray,
        );
        let cached = font.glyphs.borrow().len();
        assert!(cached > 0, "the first frame must populate the cache");

        let mut b = vec![0u8; (bw * bh * 4) as usize];
        draw(
            &mut b,
            bw,
            bh,
            &font,
            "run:",
            "fir",
            false,
            &rows,
            PAD,
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
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "",
            "火狐",
            false,
            &[],
            PAD,
            &Colors::default(),
            1,
            true,
            Subpixel::Gray,
        );
        // Prompt row bg is BG_PROMPT; any pixel differing from it is glyph ink.
        let bg = pm(Colors::default().bg_prompt);
        let mut ink = false;
        'o: for y in 2..h - 2 {
            for x in 2..w - 2 {
                let i = idx(bw, 1, x, y);
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
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        let colors = Colors::default();
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "",
            "fir",
            false,
            &[],
            PAD,
            &colors,
            1,
            true,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[idx(bw, 1, x, y)..][..4];

        // The rounded corner is outside the body, so the shadow behind the glass
        // shows through: small alpha, no colour of its own.
        for (x, y) in [(0, 0), (w - 1, h - 1)] {
            assert_eq!(px(x, y)[..3], [0, 0, 0], "shadow only at ({x},{y})");
            assert!(px(x, y)[3] < colors.bg_normal[3] / 2, "{x},{y}");
        }
        // The panel body is translucent glass, premultiplied.
        assert_eq!(px(w / 2, h / 2), &pm(colors.bg_normal));
        assert!(
            px(6, 2)[0] < px(6, 2)[3],
            "a premultiplied body cannot be brighter than its alpha"
        );
        // One pixel above the panel's first row is shadowed but not panel.
        let edge = &buf[idx(bw, 1, w / 2, 0) - bw as usize * 4..][..4];
        assert!(edge[3] > 0 && edge[3] < colors.bg_prompt[3], "{edge:?}");
    }

    /// The corner outline antialiases: the pixel just outside a corner row's
    /// span is partially covered, so its alpha sits between shadow and body.
    #[test]
    fn panel_corner_edge_is_antialiased() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (200u32, font.row_h * 3);
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        let colors = Colors::default();
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "",
            "",
            false,
            &[],
            PAD,
            &colors,
            1,
            false,
            Subpixel::Gray,
        );
        let inset = rounded_inset(w, h, CORNER_RADIUS, 0);
        let cov = inset.ceil() - inset;
        assert!(cov > 0.01, "first row should cut the corner: {inset}");
        let x = inset.ceil() as u32 - 1;
        let a = &buf[idx(bw, 1, x, 0)..][..4];
        assert!(
            a[3] > 0 && a[3] < colors.bg_prompt[3],
            "edge pixel alpha {} must be partial",
            a[3]
        );
        // Two pixels further in is the body itself (x + 1 is the hairline).
        assert_eq!(&buf[idx(bw, 1, x + 2, 0)..][..4], &pm(colors.bg_prompt));
    }

    /// The hairline sits on the panel's outermost pixel and is darker than the
    /// body, so the glass edge reads as one line with the shadow under it.
    #[test]
    fn border_ring_is_one_pixel_wide() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (200u32, font.row_h * 3);
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        let colors = Colors::default();
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "",
            "",
            false,
            &[],
            PAD,
            &colors,
            1,
            false,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[idx(bw, 1, x, y)..][..4];
        let mid = h / 2;
        assert_eq!(px(1, mid), &pm(colors.bg_normal), "body starts at x=1");
        assert_ne!(px(0, mid), &pm(colors.bg_normal), "ring at x=0");
        // Lit along the top, dark along the bottom: that is what makes the edge
        // read as a glass rim instead of a hard cut. Rows inside the corner
        // radius, where x=0 is outside the body.
        let top = CORNER_RADIUS + 4;
        let bottom = h - CORNER_RADIUS - 1;
        assert!(px(0, top)[0] > px(1, top)[0], "top rim is lit");
        assert!(px(0, bottom)[0] < px(1, bottom)[0], "bottom rim is dark");
    }

    /// The shadow decays with distance from the outline and stops at the margin,
    /// while the body itself is never shadowed.
    #[test]
    fn shadow_falls_off_outside_the_panel() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (200u32, font.row_h * 3);
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        let colors = Colors::default();
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "",
            "",
            false,
            &[],
            PAD,
            &colors,
            1,
            false,
            Subpixel::Gray,
        );
        // Straight above the panel's left side, in absolute buffer rows.
        let x = SHADOW + 40;
        let alpha = |back: u32| buf[((SHADOW - back) * bw + x) as usize * 4 + 3];
        assert!(alpha(1) > alpha(8), "shadow decays with distance");
        assert_eq!(alpha(SHADOW), 0, "nothing past the shadow margin");
        // The body is opaque to the shadow behind it.
        let (px, py) = (SHADOW + w / 2, SHADOW + h / 2);
        assert_eq!(buf[((py * bw + px) * 4) as usize + 3], colors.bg_normal[3]);
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
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "",
            "x",
            false,
            &[Row {
                text: "app",
                selected: false,
            }],
            PAD,
            &def,
            1,
            true,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[idx(bw, 1, x, y)..][..4];
        // Input bar row uses bg_prompt, the list row below it bg_normal.
        assert_eq!(px(w / 2, 2), &pm(def.bg_prompt));
        assert_eq!(px(w / 2, font.row_h + 2), &pm(def.bg_normal));
    }

    #[test]
    fn selected_row_covers_full_width_and_list_has_divider() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (240u32, font.row_h * 2);
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        let colors = Colors::default();
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "",
            "",
            false,
            &[Row {
                text: "",
                selected: true,
            }],
            PAD,
            &colors,
            1,
            false,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[idx(bw, 1, x, y)..][..4];

        let selection_y = font.row_h + 1;
        // Full width inside the hairline ring at x=0 / x=w-1.
        assert_eq!(px(1, selection_y), &pm(colors.bg_sel));
        assert_eq!(px(w - 2, selection_y), &pm(colors.bg_sel));
        assert_ne!(
            px(0, selection_y),
            &pm(colors.bg_sel),
            "ring, not selection"
        );
        assert_ne!(
            px(w - 1, selection_y),
            &pm(colors.bg_sel),
            "ring, not selection"
        );

        // The divider is its own tone, between the bar and the accent.
        let divider = px(w / 2, font.row_h - 1);
        assert_ne!(divider, &pm(colors.bg_prompt));
        assert_ne!(divider, &pm(colors.bg_normal));
        for (c, value) in divider.iter().copied().take(3).enumerate() {
            assert!(value >= pm(colors.bg_prompt)[c].min(colors.label_accent[c]));
            assert!(value <= pm(colors.bg_prompt)[c].max(colors.label_accent[c]));
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
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "address",
            "",
            false,
            &[],
            PAD,
            &def,
            1,
            true,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[idx(bw, 1, x, y)..][..4];

        let rail_h = font.line_h.ceil() as u32 + 2 * PILL_VPAD;
        let rail_y = (font.row_h - rail_h) / 2;
        let mid = font.row_h / 2;

        // A 4px rail sits flush on the bar's left padding; everything else —
        // just past the rail, between rail and label text, and the far right —
        // is bg_prompt, so the bar reads like the normal, prompt-less input bar.
        assert_eq!(px(PAD, mid), &def.label_accent, "rail left edge");
        assert_eq!(
            px(PAD + LABEL_RAIL_W - 1, mid),
            &def.label_accent,
            "rail right edge"
        );
        assert_eq!(
            px(PAD + LABEL_RAIL_W, mid),
            &pm(def.bg_prompt),
            "just past the rail"
        );
        assert_eq!(
            px(PAD + 8, mid),
            &pm(def.bg_prompt),
            "between rail and label text"
        );
        assert_eq!(px(w - 10, mid), &pm(def.bg_prompt));
        // Rail is vertically centered at the same height the capsule had.
        assert_eq!(px(PAD, rail_y), &def.label_accent, "rail top");
        assert_eq!(px(PAD, rail_y - 1), &pm(def.bg_prompt), "above the rail");
        assert_eq!(
            px(PAD, rail_y + rail_h),
            &pm(def.bg_prompt),
            "below the rail"
        );

        // Empty prompt → no rail; the whole bar stays bg_prompt.
        let mut buf2 = vec![0u8; (bw * bh * 4) as usize];
        draw(
            &mut buf2,
            bw,
            bh,
            &font,
            "",
            "x",
            false,
            &[],
            PAD,
            &def,
            1,
            true,
            Subpixel::Gray,
        );
        assert_eq!(&buf2[idx(bw, 1, PAD, mid)..][..4], &pm(def.bg_prompt));
    }

    /// Caret hidden while blinked off: no solid fg_prompt column.
    #[test]
    fn caret_can_be_hidden_for_blinking() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (200u32, font.row_h);
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        let colors = Colors::default();
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "",
            "fir",
            false,
            &[],
            PAD,
            &colors,
            1,
            false,
            Subpixel::Gray,
        );
        let mid = font.row_h / 2;
        let px = |x: u32, y: u32| &buf[idx(bw, 1, x, y)..][..4];
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
        let (start, max_w) = (PAD as f32, 60.0f32);
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
        let (bw, bh) = frame_size(w, h, 2);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        let def = Colors::default();
        draw(
            &mut buf,
            bw,
            bh,
            &font,
            "address",
            "",
            false,
            &[Row {
                text: "",
                selected: true,
            }],
            PAD,
            &def,
            2,
            true,
            Subpixel::Gray,
        );
        let px = |x: u32, y: u32| &buf[idx(bw, 2, x, y)..][..4];
        let mid = font.row_h / 2;

        // Padding 16 -> 32, rail width 4 -> 8.
        let rail_x = PAD * 2;
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
            &pm(def.bg_prompt),
            "past the scaled rail"
        );

        let selection_y = font.row_h + 1;
        assert_eq!(px(1, selection_y), &pm(def.bg_sel));
        assert_eq!(px(w - 2, selection_y), &pm(def.bg_sel));
        assert_ne!(
            px(w / 2, font.row_h - 1),
            &pm(def.bg_prompt),
            "logical divider scales to physical pixels"
        );

        // The shadow margin scales too: 2 * 24 logical px each side.
        assert_eq!(bw - w, 4 * SHADOW);
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
            let (bw, bh) = frame_size(w, h, 1);
            let mut buf = vec![0u8; (bw * bh * 4) as usize];
            let colors = Colors::default();
            draw(
                &mut buf,
                bw,
                bh,
                &font,
                "",
                "fir",
                false,
                &[],
                PAD,
                &colors,
                1,
                true,
                Subpixel::Gray,
            );
            let px = |x: u32, y: u32| &buf[idx(bw, 1, x, y)..][..4];

            // A solid (non-antialiased) 2px-wide fg_prompt column = the caret;
            // glyph ink is always anti-aliased, so it can't fake this. It also
            // proves ink over the translucent body raises alpha to opaque.
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
            assert!(x > PAD + 4, "caret at x={x} should follow the text");
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
            let (bw, bh) = frame_size(w, h, 1);
            let mut buf = vec![0u8; (bw * bh * 4) as usize];
            // Locate the solid 2px caret (same detector as the test above) for a
            // given prompt; the query "fir" and caret stay identical in both frames.
            let mut caret_x = |prompt: &str| -> u32 {
                buf.fill(0);
                draw(
                    &mut buf,
                    bw,
                    bh,
                    &font,
                    prompt,
                    "fir",
                    false,
                    &[],
                    PAD,
                    &colors,
                    1,
                    true,
                    Subpixel::Gray,
                );
                let mid = font.row_h / 2;
                let at = |x: u32| &buf[idx(bw, 1, x, mid)..][..4];
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
            let advance = measure(&font, "run:", (w - 2 * PAD) as f32) as u32;
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
