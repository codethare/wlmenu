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
/// Gap between the input bar and the result list (logical px): they are two
/// cards, the way Spotlight floats its field above the results.
pub const SEPARATION: u32 = 12;

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

/// Both drop-shadow layers for every card in one pass. The falloff is measured from the
/// card's *outline* — a true signed distance to the rounded rectangle — so it wraps the
/// corners instead of being cut off by them the way a per-row span is. A pixel takes the
/// strongest card touching it, and nothing is painted under a translucent body.
fn shadow(buf: &mut [u8], w: u32, h: u32, panels: &[Panel], scale: u32) {
    let s = scale.max(1);
    // (blur, vertical offset, alpha by squared distance)
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
    let drop = layers.iter().map(|l| l.1.max(0) as u32).max().unwrap_or(0);
    let shapes: Vec<Shape> = panels.iter().map(Shape::new).collect();
    // The area any layer can touch: the cards' outlines grown by the blur (and by the
    // downward offset), clipped to the buffer.
    let left = shapes.iter().map(|p| p.x0()).min().unwrap_or(0);
    let right = shapes.iter().map(|p| p.x1()).max().unwrap_or(w);
    let x0 = left.saturating_sub(reach);
    let x1 = (right + reach).min(w);
    let y0 = panels
        .iter()
        .map(|p| p.y)
        .min()
        .unwrap_or(0)
        .saturating_sub(reach);
    let y1 = panels
        .iter()
        .map(|p| p.y + p.h)
        .max()
        .unwrap_or(0)
        .saturating_add(reach + drop)
        .min(h);

    // Alpha by horizontal position, rebuilt only when a row's geometry changes: every row
    // level with the straight part of a card shares one profile. `inked` is the span of it
    // that holds anything, so a row copies only that stretch.
    let mut band = vec![0u8; (x1 - x0) as usize];
    let mut inked = 0..0;
    let mut cached = [u32::MAX; 4];

    for y in y0..h.min(y1.max(y0)) {
        let py = y as f32 + 0.5;
        // This row's distance to every layer of every card, measured from where its
        // corner arcs begin (`qy`), plus the same for the cards' own outlines.
        let mut qys = [[0.0f32; 2]; 2];
        let mut body_qy = [0.0f32; 2];
        let mut key = [u32::MAX; 4];
        let mut live = 0u32;
        for (m, shape) in shapes.iter().enumerate().take(2) {
            // Only the negative side collapses: the body contour needs the real value,
            // or a row just outside the card reads as if it were under it.
            body_qy[m] = shape.qy(py, 0).max(0.0);
            for (k, (blur, offset, _)) in layers.iter().enumerate() {
                // Rows level with the straight part all see the same falloff, so the
                // negative side collapses to one key; past the blur the layer is gone.
                let qy = shape.qy(py, *offset);
                let reach = *blur as f32 + shape.r;
                qys[m][k] = qy.max(0.0).min(reach);
                if qy < reach {
                    key[m * 2 + k] = qys[m][k].to_bits();
                    live |= 1 << (m * 2 + k);
                }
            }
        }
        let live_bits = live;
        if key != cached {
            cached = key;
            let mut first = band.len();
            let mut last = 0;
            for (i, out) in band.iter_mut().enumerate() {
                let px = x0 as f32 + i as f32 + 0.5;
                let mut a = 0u32;
                for (m, shape) in shapes.iter().enumerate().take(2) {
                    // No layer of this card reaches the row at all.
                    if live_bits & (0b11 << (m * 2)) == 0 {
                        continue;
                    }
                    let qx = shape.qx(px);
                    // Under the card itself: painted by its own body, never by the shadow.
                    if outline_gap(qx, body_qy[m], shape.r) <= 0.0 {
                        continue;
                    }
                    let mut pa = 0u32;
                    for (k, (blur, _, lut)) in layers.iter().enumerate() {
                        if live_bits & (1 << (m * 2 + k)) == 0 {
                            continue;
                        }
                        let d = outline_gap(qx, qys[m][k], shape.r).max(0.0);
                        let la = lut[((d * d) as u32).min(blur * blur) as usize];
                        pa = la + div255(pa * (255 - la));
                    }
                    a = a.max(pa);
                }
                if a > 0 {
                    first = first.min(i);
                    last = i;
                }
                *out = a as u8;
            }
            inked = if last >= first { first..last + 1 } else { 0..0 };
        }
        let from = (x0 + inked.start as u32).min(w);
        let to = (x0 + inked.end as u32).min(w);
        // Outside the stretch this row paints, a reused buffer slot still holds the
        // previous frame.
        if from > 0 {
            set_span(buf, w, y, 0, from, CLEAR);
        }
        if to < w {
            set_span(buf, w, y, to, w, CLEAR);
        }
        // The card standing on this row paints its own body, so the shadow goes in
        // around it instead of under it: two stretches, not the whole width.
        let body = panels
            .iter()
            .find(|p| y >= p.y && y < p.y + p.h)
            .map(|p| p.span(y - p.y));
        let (bl, br) = match body {
            Some((bx0, bx1)) => (bx0.max(from).min(to), bx1.min(to).max(from)),
            None => (to, to),
        };
        for (seg_from, seg_to) in [(from, bl), (br, to)] {
            for (pixel, a) in buf[((y * w + seg_from) * 4) as usize..]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .take((seg_to.saturating_sub(seg_from)) as usize)
                .zip(&band[(seg_from - x0) as usize..(seg_to - x0) as usize])
            {
                pixel.copy_from_slice(&[0, 0, 0, *a]);
            }
        }
    }
    // Rows the layers cannot reach at all.
    for y in (0..y0).chain(y1.min(h)..h) {
        set_span(buf, w, y, 0, w, CLEAR);
    }
}

/// A card's outline for the shadow: centre, half extents, and the corner radius clamped
/// to them, so the distances below stay meaningful on a degenerate card.
struct Shape {
    cx: f32,
    cy: f32,
    half_w: f32,
    half_h: f32,
    r: f32,
    x: u32,
    w: u32,
}

impl Shape {
    fn new(panel: &Panel) -> Self {
        let half_w = panel.w as f32 / 2.0;
        let half_h = panel.h as f32 / 2.0;
        Self {
            cx: panel.x as f32 + half_w,
            cy: panel.y as f32 + half_h,
            half_w,
            half_h,
            r: (panel.r as f32).min(half_w).min(half_h),
            x: panel.x,
            w: panel.w,
        }
    }

    /// Horizontal gap from where the corner arcs begin; `|qx| > r` is the straight side.
    fn qx(&self, px: f32) -> f32 {
        (px - self.cx).abs() - (self.half_w - self.r)
    }

    /// The same for a row, with the layer's downward offset applied.
    fn qy(&self, py: f32, offset: i64) -> f32 {
        (py - self.cy - offset as f32).abs() - (self.half_h - self.r)
    }

    fn x0(&self) -> u32 {
        self.x
    }

    fn x1(&self) -> u32 {
        self.x + self.w
    }
}

/// Signed distance from a rounded rectangle's outline, given the horizontal and vertical
/// gaps to where its corner arcs begin: negative inside.
fn outline_gap(qx: f32, qy: f32, radius: f32) -> f32 {
    if qy <= 0.0 {
        qx - radius
    } else if qx <= 0.0 {
        qy - radius
    } else {
        (qx * qx + qy * qy).sqrt() - radius
    }
}

/// One card: rounded body fill, 1 px coverage antialiasing on both sides, and
/// the hairline rim that makes its edge read as glass against the shadow.
fn body(buf: &mut [u8], w: u32, panel: &Panel, bg: Bgra) {
    for by in panel.y..panel.y + panel.h {
        let rel = by - panel.y;
        let (x0, x1) = panel.span(rel);
        set_span(buf, w, by, x0, x1, bg);
        let inset = rounded_inset(panel.w, panel.h, panel.r, rel);
        let cov = ((inset.ceil() - inset) * 255.0).round() as u32;
        if cov > 0 && x0 > 0 {
            over_pm(&mut buf[(by * w + x0 - 1) as usize * 4..][..4], bg, cov);
        }
        if cov > 0 && x1 < w {
            over_pm(&mut buf[(by * w + x1) as usize * 4..][..4], bg, cov);
        }
        // Hairline on the outermost pixel: the seam where the body meets the
        // shadow. Lit along the top, fading out over the card, dark underneath.
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
    let column = Panel {
        x: mo,
        y: mo,
        w: w - 2 * mo,
        h: h - 2 * mo,
        r: CORNER_RADIUS * s,
    };
    // Two cards: the input bar, and the list floating `sep` below it.
    let sep = SEPARATION * s;
    let bar = Panel {
        h: row_h.min(column.h),
        ..column
    };
    let list = (column.h > bar.h + sep).then(|| Panel {
        y: bar.y + bar.h + sep,
        h: column.h - bar.h - sep,
        ..column
    });
    let panels: Vec<Panel> = match list {
        Some(l) => vec![bar, l],
        None => vec![bar],
    };
    shadow(buf, w, h, &panels, s);

    // Body: whole-pixel spans, premultiplied, one write per pixel. What is left
    // of the fractional corner inset is the coverage of the single pixel just
    // outside the span, which is what antialiases the rounded edge.
    let (bg_normal, bg_prompt, bg_sel) = (
        premul(colors.bg_normal),
        premul(colors.bg_prompt),
        premul(colors.bg_sel),
    );
    body(buf, w, &bar, bg_prompt);
    if let Some(list) = list {
        body(buf, w, &list, bg_normal);
    }

    // Prompt / input row text.
    let left = bar.x as f32;
    let right = (bar.x + bar.w) as f32;
    let baseline = row_baseline(font, bar.y);
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
            bar.x + p,
            bar.y + rail_y,
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
            (bar.w - 2 * p) as f32,
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
    let Some(list) = list else { return };
    for (i, row) in rows.iter().enumerate() {
        let rel = row_h * i as u32;
        if rel >= list.h {
            break;
        }
        if row.selected {
            for yy in rel..(rel + row_h).min(list.h) {
                // Inside the hairline: the ring frames the selection instead of
                // being painted over by it.
                let (x0, x1) = list.span(yy);
                if x0 + 1 < x1 {
                    set_span(buf, w, list.y + yy, x0 + 1, x1 - 1, bg_sel);
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
            row_baseline(font, list.y + rel),
            row.text,
            fg,
            (list.w - 2 * p) as f32,
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

    /// Panel height for `rows` result rows: bar, the separating gap, the list.
    fn frame_h(row_h: u32, rows: u32, scale: u32) -> u32 {
        row_h * (rows + 1) + SEPARATION * scale
    }

    /// Row `i` of the list, in panel coordinates.
    fn row_y(row_h: u32, i: u32, scale: u32) -> u32 {
        row_h + SEPARATION * scale + row_h * i
    }

    /// Headless render check: draws a real frame (text incl. CJK) to a raw BGRA dump.
    #[test]
    fn renders_frame_with_cjk_text() {
        let font = MenuFont::load(None, 16.0).expect("system font available");
        let (w, h) = (320u32, frame_h(font.row_h, 3, 1));
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
        assert_eq!(px(w / 2, row_y(font.row_h, 0, 1) + 1), &pm(colors.bg_sel));
        // Normal row (rows[1]) has the normal background.
        assert_eq!(
            px(w / 2, row_y(font.row_h, 1, 1) + 1),
            &pm(colors.bg_normal)
        );
        // Prompt row has the prompt background.
        assert_eq!(px(w / 2, 1), &pm(colors.bg_prompt));
        // First text row must contain non-background pixels (glyph ink).
        let mut text_has_ink = false;
        'outer: for y in 2..24u32 {
            for x in PAD..200u32 {
                if px(x, row_y(font.row_h, 0, 1) + y) != pm(colors.bg_sel) {
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
        let (w, h) = (320u32, frame_h(font.row_h, 1, 1));
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
        // shows through: no colour of its own, and less opaque than the body.
        // One pixel diagonally outside each rounded corner: the shadow wraps the arc.
        for (x, y) in [(0, 0), (w, h)] {
            let p = &buf[(idx(bw, 1, x, y) - bw as usize * 4 - 4)..][..4];
            assert_eq!(p[..3], [0, 0, 0], "shadow only at ({x},{y})");
            assert!(p[3] > 0, "the corner carries shadow: ({x},{y}) {p:?}");
        }
        // The list card's body is translucent glass, premultiplied.
        assert_eq!(
            px(w / 2, row_y(font.row_h, 0, 1) + 2),
            &pm(colors.bg_normal)
        );
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
        let font = MenuFont::load(None, 20.0).expect("system font available");
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
        let mid = font.row_h / 2; // inside the bar card
        assert_eq!(px(1, mid), &pm(colors.bg_prompt), "body starts at x=1");
        assert_ne!(px(0, mid), &pm(colors.bg_prompt), "ring at x=0");
        // Lit along the top, dark along the bottom: that is what makes the edge
        // read as a glass rim instead of a hard cut. Inside the bar card's flat
        // band — above/below the corner radius the rim is off the body.
        let top = CORNER_RADIUS;
        let bottom = font.row_h - CORNER_RADIUS - 1;
        assert!(px(0, top)[0] > px(1, top)[0], "top rim is lit");
        assert!(px(0, bottom)[0] < px(1, bottom)[0], "bottom rim is dark");
    }

    /// The shadow wraps a corner instead of being cut off at it: measured straight
    /// out of the arc, the corner carries a good share of what the edge beside it
    /// carries.
    #[test]
    fn the_shadow_wraps_the_corners() {
        let font = MenuFont::load(None, 20.0).expect("system font available");
        let (w, h) = (200u32, font.row_h * 3);
        let (bw, bh) = frame_size(w, h, 1);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
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
            &Colors::default(),
            1,
            false,
            Subpixel::Gray,
        );
        let alpha = |x: u32, y: u32| buf[((y * bw + x) * 4 + 3) as usize];
        let (mo, r) = (SHADOW, CORNER_RADIUS);

        let edge = alpha(mo - 4, mo + h / 2);
        // Four pixels out of the arc, on the diagonal through the corner's centre.
        let step = (r as f64 + 4.0) / std::f64::consts::SQRT_2;
        let at = (mo as f64 + r as f64 - step) as u32;
        let corner = alpha(at, at);

        assert!(corner > 0, "the corner has no shadow at all");
        assert!(
            corner * 3 >= edge,
            "the corner carries {corner} against the edge's {edge}"
        );
    }

    /// The body is translucent, so nothing may be painted underneath it: what shows
    /// through inside the top edge is body, not the shadow the layer would cast there.
    #[test]
    fn no_shadow_is_painted_under_the_body() {
        let font = MenuFont::load(None, 20.0).expect("system font available");
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
        let column = w / 2;
        assert_eq!(
            px(column, 2),
            px(column, 8),
            "the strip inside the top edge is body, not shadow"
        );
        assert_eq!(px(column, 2), &pm(colors.bg_prompt));
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
        let (w, h) = (200u32, frame_h(font.row_h, 1, 1));
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
        assert_eq!(px(w / 2, row_y(font.row_h, 0, 1) + 2), &pm(def.bg_normal));
    }

    /// The bar and the list are separate cards: the selection fills the list
    /// card edge to edge inside its hairline, and the gap between the cards shows
    /// the wallpaper (plus the bar's shadow), never either body colour.
    #[test]
    fn selected_row_fills_the_list_card_and_a_gap_separates_the_cards() {
        let font = MenuFont::load(None, 20.0).expect("system font available");
        let (w, h) = (240u32, frame_h(font.row_h, 1, 1));
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

        // Past the corner radius, where the card's row is a straight span.
        let selection_y = row_y(font.row_h, 0, 1) + CORNER_RADIUS + 1;
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

        // Between the cards: nothing but the two cards' shadows — no body colour.
        let gap = px(w / 2, font.row_h + 2);
        assert_ne!(gap, &pm(colors.bg_prompt));
        assert_ne!(gap, &pm(colors.bg_normal));
        assert_eq!(gap[..3], [0, 0, 0], "the gap holds shadow, not a body");
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
        let (w, h) = (480u32, frame_h(font.row_h, 2, 2));
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

        let selection_y = row_y(font.row_h, 0, 2) + font.row_h / 2 + 1;
        assert_eq!(px(1, selection_y), &pm(def.bg_sel));
        assert_eq!(px(w - 2, selection_y), &pm(def.bg_sel));
        assert_ne!(
            px(w / 2, font.row_h + 2 * SEPARATION),
            &pm(def.bg_prompt),
            "the logical gap scales to physical pixels"
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
