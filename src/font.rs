//! Font loading: an explicit font path, or an auto-picked system font (a small
//! Latin face first, with CJK-capable faces read on demand so Chinese labels
//! still render).

#[cfg(test)]
use ab_glyph::GlyphId;
use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One coverage plane of a rasterized glyph: 8-bit coverage plus where it sits
/// relative to the canonical pen position (the pen with only its subpixel part
/// kept).
#[derive(Default, Clone)]
pub struct GlyphPlane {
    pub min_x: i32,
    pub min_y: i32,
    pub w: u32,
    pub h: u32,
    pub coverage: Vec<u8>,
}

/// One rasterized glyph: the advance to move the pen by, and its coverage
/// planes — one for grayscale antialiasing, three (left/middle/right subpixel
/// sample) for LCD subpixel antialiasing.
pub struct GlyphBitmap {
    pub advance: f32,
    pub planes: Vec<GlyphPlane>,
}

/// Glyph cache key: (face index, glyph id, subpixel x bucket, subpixel y bucket,
/// antialiasing mode).
pub type GlyphKey = (u8, u16, u8, u8, u8);

pub struct MenuFont {
    /// Chain faces, index 0 = primary. Beyond the primary a face is only read
    /// when a glyph actually needs it: the CJK face is 27MB, which used to be
    /// read before the first frame just to draw Latin text.
    faces: RefCell<Vec<FaceState>>,
    /// Paths for faces 1.., parallel to `faces`; the index stays stable so the
    /// glyph cache keys stay valid across lazy loads.
    chain: Vec<(PathBuf, u32)>,
    /// Pixel size (em height).
    pub size: f32,
    pub ascent_px: f32,
    pub descent_px: f32,
    pub line_h: f32,
    /// Row height incl. padding.
    pub row_h: u32,
    /// Rasterized glyphs reused across frames: without this every frame
    /// re-rasterizes every glyph, which dominates a full (64-row) frame
    /// (~8.4 ms measured). Discarded wholesale when the font is re-derived
    /// for a new scale, so a scale change cannot serve stale bitmaps.
    pub glyphs: RefCell<HashMap<GlyphKey, GlyphBitmap>>,
}

enum FaceState {
    Loaded(FontVec),
    /// Not read yet (`chain[i - 1]`).
    Unread,
    /// Read or parse failed: never retried, or every missing glyph would pay
    /// for the failing I/O again.
    Failed,
}

/// An in-flight font load: started before the Wayland handshake, joined by the
/// event loop when it first needs a frame.
pub(crate) type FontLoad = std::thread::JoinHandle<Result<MenuFont, String>>;

/// Load `spec` on a thread, so the read + parse overlap the handshake instead
/// of preceding it. Joining is the caller's business.
pub(crate) fn spawn_load(spec: Option<String>, size: f32) -> FontLoad {
    std::thread::spawn(move || MenuFont::load(spec.as_deref(), size))
}

/// Row height incl. padding, from the size alone: ab_glyph's scaled `height()`
/// is just the requested `PxScale`, so this is face-independent and the layer
/// surface can be committed at its final height before any font is loaded.
/// Comfortable row height ≈1.5× font size: glyphs breathe top/bottom (the
/// vertical pad is a quarter of the em on each side), and the tall selection
/// band is an easier target to hit (Fitts).
pub(crate) fn row_h_for(size: f32) -> u32 {
    size.ceil() as u32 + 2 * (size * 0.25).round() as u32
}

/// The pixel size `load` will use for `spec`: an existing path is read as a font
/// file and carries no size token, otherwise the token (or the default) wins.
/// Mirrors `load`'s branch without reading the file — the point is to know the
/// height before the load runs.
/// ponytail: `exists()` can lie (a path that exists but is unreadable sends
/// `load` down the family branch, so the height can be off by one row); the
/// first frame's `set_size` corrects it.
pub(crate) fn spec_size(spec: Option<&str>, default: f32) -> f32 {
    let spec = spec.map(str::trim);
    if let Some(s) = spec
        && Path::new(s).exists()
    {
        return default;
    }
    spec.and_then(|s| parse_spec(s).2).unwrap_or(default)
}

impl MenuFont {
    pub fn load(spec: Option<&str>, size: f32) -> Result<MenuFont, String> {
        let spec = spec.map(str::trim);
        // `-f /path/to/font.ttf`: use exactly that face — no system font scan,
        // no fallback chain (wmenu -f semantics). This is the fast-startup
        // path: building the system chain scans every installed font (~100ms).
        if let Some(s) = spec
            && let Ok(bytes) = std::fs::read(s)
        {
            let font = FontVec::try_from_vec(bytes).map_err(|e| format!("invalid font: {e}"))?;
            return Ok(Self::build(font, Vec::new(), size));
        }

        let chain = cached_system_chain();
        let (bytes, index, size) = match spec {
            Some(s) => resolve_family(s, size)?,
            None => {
                let (path, index) = chain
                    .first()
                    .cloned()
                    .ok_or("no usable system font found".to_string())?;
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                (bytes, index, size)
            }
        };
        let font = FontVec::try_from_vec_and_index(bytes, index)
            .map_err(|e| format!("invalid font: {e}"))?;
        // With no `-f` the chain's head is the primary; the rest of the chain
        // becomes the lazily-read fallbacks.
        let skip = usize::from(spec.is_none());
        let chain: Vec<(PathBuf, u32)> = chain.into_iter().skip(skip).collect();
        Ok(Self::build(font, chain, size))
    }

    fn build(font: FontVec, chain: Vec<(PathBuf, u32)>, size: f32) -> MenuFont {
        let scaled = font.as_scaled(PxScale::from(size));
        let ascent_px = scaled.ascent();
        let descent_px = scaled.descent();
        let line_h = scaled.height(); // == size by definition of PxScale
        let row_h = row_h_for(size);
        let faces = std::iter::once(FaceState::Loaded(font))
            .chain(chain.iter().map(|_| FaceState::Unread))
            .collect();
        MenuFont {
            faces: RefCell::new(faces),
            chain,
            size,
            ascent_px,
            descent_px,
            line_h,
            row_h,
            glyphs: RefCell::new(HashMap::new()),
        }
    }

    /// Faces in the chain, primary included.
    pub fn face_count(&self) -> usize {
        self.faces.borrow().len()
    }

    /// Chain face `i` (0 = primary, always loaded), read on first use. `None`
    /// when that face cannot be read or parsed, so the chain skips it.
    pub fn face(&self, i: usize) -> Option<Ref<'_, FontVec>> {
        let unread = matches!(self.faces.borrow().get(i), Some(FaceState::Unread));
        if unread {
            let read = i
                .checked_sub(1)
                .and_then(|k| self.chain.get(k))
                .and_then(|(path, index)| read_face(path, *index));
            if let Some(slot) = self.faces.borrow_mut().get_mut(i) {
                *slot = match read {
                    Some(font) => FaceState::Loaded(font),
                    None => FaceState::Failed,
                };
            }
        }
        Ref::filter_map(self.faces.borrow(), |faces| match faces.get(i) {
            Some(FaceState::Loaded(font)) => Some(font),
            _ => None,
        })
        .ok()
    }

    /// Chain index + glyph id for `ch`, reading faces on demand.
    #[cfg(test)]
    fn glyph_for(&self, ch: char) -> Option<(usize, GlyphId)> {
        for i in 0..self.face_count() {
            let Some(face) = self.face(i) else {
                continue;
            };
            let gid = face.as_scaled(PxScale::from(self.size)).glyph_id(ch);
            if gid != GlyphId(0) {
                return Some((i, gid));
            }
        }
        None
    }

    /// True if any face in the chain (primary first) has a glyph for `ch`.
    #[cfg(test)]
    pub fn has_glyph(&self, ch: char) -> bool {
        self.glyph_for(ch).is_some()
    }

    /// Faces not read yet (test-only): startup must read nothing but the
    /// primary.
    #[cfg(test)]
    fn unread_faces(&self) -> usize {
        self.faces
            .borrow()
            .iter()
            .filter(|f| matches!(f, FaceState::Unread))
            .count()
    }
}

/// `-f` family-style spec: "SourceCodePro medium 13" — family, optional style
/// word, optional size (points, or pixels with a "px" suffix; Pango/wmenu
/// convention). Resolved through the full system font database.
fn resolve_family(spec: &str, default_size: f32) -> Result<(Vec<u8>, u32, f32), String> {
    let (family, weight, size) = parse_spec(spec);
    query_family(&family, weight)
        .map(|(data, index)| (data, index, size.unwrap_or(default_size)))
        .ok_or_else(|| format!("font family not found: {family}"))
}

/// Split "Family [style] [size]": the first numeric token is the size
/// (points, or pixels with a "px" suffix); recognized style words pick the
/// weight; everything else is the family. Returned size is device pixels.
fn parse_spec(spec: &str) -> (String, fontdb::Weight, Option<f32>) {
    let mut family = Vec::new();
    let mut weight = fontdb::Weight::NORMAL;
    let mut size = None;
    for tok in spec.split_whitespace() {
        if size.is_none()
            && let Some(s) = size_in_px(tok)
        {
            size = Some(s);
            continue;
        }
        if let Some(w) = weight_of(tok) {
            weight = w;
        } else {
            family.push(tok);
        }
    }
    (family.join(" "), weight, size)
}

/// Pango-aligned size token → pixel size: a bare number is points, converted
/// at Pango's default 96 dpi (×96/72); a "px" suffix is absolute pixels.
fn size_in_px(tok: &str) -> Option<f32> {
    if let Some(px) = tok.to_ascii_lowercase().strip_suffix("px") {
        return px.trim().parse::<f32>().ok();
    }
    tok.parse::<f32>().ok().map(|pt| pt * 96.0 / 72.0)
}

fn weight_of(word: &str) -> Option<fontdb::Weight> {
    match word.to_ascii_lowercase().as_str() {
        "thin" => Some(fontdb::Weight::THIN),
        "light" => Some(fontdb::Weight::LIGHT),
        "normal" | "regular" | "book" | "roman" => Some(fontdb::Weight::NORMAL),
        "medium" => Some(fontdb::Weight::MEDIUM),
        "semibold" | "demibold" => Some(fontdb::Weight::SEMIBOLD),
        "bold" => Some(fontdb::Weight::BOLD),
        "extrabold" | "ultrabold" | "heavy" => Some(fontdb::Weight::EXTRA_BOLD),
        "black" => Some(fontdb::Weight::BLACK),
        _ => None,
    }
}

/// Family name → bytes, through the on-disk cache: the lookup underneath is a
/// full fontdb scan (it parses every installed font header, ~40ms here) and it
/// used to run on every single launch of `-f FAMILY`.
fn query_family(family: &str, weight: fontdb::Weight) -> Option<(Vec<u8>, u32)> {
    let (path, index) = cached_family_path(family, weight)?;
    let bytes = std::fs::read(&path).ok()?;
    Some((bytes, index))
}

fn cached_family_path(family: &str, weight: fontdb::Weight) -> Option<(PathBuf, u32)> {
    let fam = family.to_string();
    let scan = move || query_family_path(&fam, weight).into_iter().collect();
    cached_chain(family_cache_path(family, weight), cache_key(), scan)
        .into_iter()
        .next()
}

fn query_family_path(family: &str, weight: fontdb::Weight) -> Option<(PathBuf, u32)> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let fam = fontdb::Family::Name(family);
    let q = |w| fontdb::Query {
        families: std::slice::from_ref(&fam),
        weight: w,
        ..Default::default()
    };
    db.query(&q(weight))
        .or_else(|| db.query(&q(fontdb::Weight::NORMAL)))
        .and_then(|id| face_path(&db, id))
}

/// CJK-preferring ordered list of usable system font paths (primary candidate
/// first), used both for auto-pick and as the fallback chain. Mono leads: it is
/// 0.6MB against the CJK face's 27MB, and only the leading face is read at load
/// time (the rest are read on demand).
fn system_chain() -> Vec<(PathBuf, u32)> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    // CJK list: mutual alternates, not complements. One face covers all CJK +
    // kana glyphs, and SC/TC/JP are usually faces of the same ~20MB TTC — so
    // take the first hit and stop.
    const CJK: &[&str] = &[
        "Noto Sans CJK SC",
        "Noto Sans CJK TC",
        "Noto Sans CJK JP",
        "WenQuanYi Micro Hei",
        "Source Han Sans SC",
    ];
    const MONO: &[&str] = &["Noto Sans Mono", "DejaVu Sans Mono", "Liberation Mono"];
    let mut out = Vec::new();
    for fam in MONO {
        if let Some(found) = query_path(&db, fam) {
            out.push(found);
        }
    }
    for fam in CJK {
        if let Some(found) = query_path(&db, fam) {
            out.push(found);
            break;
        }
    }
    // Nerd Font PUA icons (patched families / "Symbols Nerd Font"): the mono
    // and CJK faces never carry -style glyphs, so a face whose family mentions
    // "Nerd Font" joins the chain to resolve them.
    for face in db.faces() {
        if face
            .families
            .iter()
            .any(|f| f.0.to_ascii_lowercase().contains("nerd font"))
            && let Some(found) = face_path(&db, face.id)
        {
            out.push(found);
            break;
        }
    }
    if out.is_empty() {
        // Give up on family preference: just take the first face so the menu still renders.
        if let Some(face) = db.faces().next()
            && let Some(found) = face_path(&db, face.id)
        {
            out.push(found);
        }
    }
    out
}

fn query_path(db: &fontdb::Database, family: &str) -> Option<(PathBuf, u32)> {
    let id = db.query(&fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        ..Default::default()
    })?;
    face_path(db, id)
}

fn face_path(db: &fontdb::Database, id: fontdb::ID) -> Option<(PathBuf, u32)> {
    let face = db.face(id)?;
    let path = match &face.source {
        fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => p.clone(),
        fontdb::Source::Binary(_) => return None,
    };
    Some((path, face.index))
}

/// Read + parse one chain face. Best-effort: a font that cannot be loaded is
/// skipped by the chain.
fn read_face(path: &Path, index: u32) -> Option<FontVec> {
    let bytes = std::fs::read(path).ok()?;
    FontVec::try_from_vec_and_index(bytes, index).ok()
}

/// Persistent cache of the resolved system chain, so the fontdb scan (it parses
/// every installed font header: 655 files / 422MB here) only runs when fonts
/// actually change. Keyed on the standard font dirs' mtimes — the same trick as
/// fontconfig's fc-cache.
/// ponytail: dirs from a custom /etc/fonts/fonts.conf aren't keyed, so chain
/// changes there go unnoticed until a keyed dir changes.
fn cached_system_chain() -> Vec<(PathBuf, u32)> {
    cached_chain(cache_path(), cache_key(), system_chain)
}

/// What the on-disk cache is worth this launch.
enum CacheUse {
    Fresh(Vec<(PathBuf, u32)>),
    /// Key changed (a font package touched a dir) or a chain path vanished, but
    /// every cached path is still there: a usable font choice, so use it now
    /// and refresh the cache on a thread.
    Stale(Vec<(PathBuf, u32)>),
    /// Nothing usable: compute now.
    None,
}

/// `fresh` is the cache read with the current key, `any` the same file read
/// without key validation; either is usable only while every path still exists.
fn cache_use(
    fresh: Option<Vec<(PathBuf, u32)>>,
    any: Option<Vec<(PathBuf, u32)>>,
    exists: impl Fn(&Path) -> bool,
) -> CacheUse {
    let usable = |c: Option<Vec<(PathBuf, u32)>>| c.filter(|c| c.iter().all(|(p, _)| exists(p)));
    match (usable(fresh), usable(any)) {
        (Some(chain), _) => CacheUse::Fresh(chain),
        (None, Some(chain)) => CacheUse::Stale(chain),
        (None, None) => CacheUse::None,
    }
}

/// Run `scan` through the cache at `path`: fresh → as-is; stale but usable →
/// as-is, with `scan` re-run on a thread to refresh the cache for the next
/// launch; no usable cache → `scan` now. A stale key used to block the first
/// frame on the scan itself (~40-90ms warm here, seconds when the font files
/// are cold too).
fn cached_chain(
    path: Option<PathBuf>,
    key: Option<Vec<(PathBuf, std::time::SystemTime)>>,
    scan: impl Fn() -> Vec<(PathBuf, u32)> + Send + 'static,
) -> Vec<(PathBuf, u32)> {
    let (Some(path), Some(key)) = (path, key) else {
        return scan(); // no XDG_CACHE_HOME/HOME — scan every launch
    };
    match cache_use(read_cache(&path, &key), read_chain(&path), Path::exists) {
        CacheUse::Fresh(chain) => chain,
        CacheUse::Stale(chain) => {
            std::thread::spawn(move || {
                let chain = scan();
                // The key is re-read: the launch that found it stale is the very
                // reason it changed.
                if let Some(key) = cache_key() {
                    write_cache(&path, &key, &chain);
                }
            });
            chain
        }
        CacheUse::None => {
            let chain = scan();
            write_cache(&path, &key, &chain);
            chain
        }
    }
}

/// Standard user/system font dirs (fontdb's no-fontconfig scan list); their
/// mtimes are the cache key. A font added/removed touches the dir mtime.
fn cache_key() -> Option<Vec<(PathBuf, std::time::SystemTime)>> {
    let mut dirs = Vec::new();
    if let Some(h) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(h).join("fonts"));
    } else if let Some(h) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(h).join(".fonts"));
    }
    if let Some(h) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(h).join(".local/share/fonts"));
    }
    dirs.push(PathBuf::from("/usr/local/share/fonts"));
    dirs.push(PathBuf::from("/usr/share/fonts"));
    let key: Vec<_> = dirs
        .into_iter()
        .filter_map(|d| Some((d.clone(), std::fs::metadata(&d).ok()?.modified().ok()?)))
        .collect();
    if key.is_empty() { None } else { Some(key) }
}

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|base| base.join("wlmenu"))
}

fn cache_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("font-chain"))
}

/// One file per `-f FAMILY [style]` lookup; the size token doesn't affect
/// resolution, so it stays out of the name.
fn family_cache_path(family: &str, weight: fontdb::Weight) -> Option<PathBuf> {
    Some(cache_dir()?.join(family_file_name(family, weight)))
}

fn family_file_name(family: &str, weight: fontdb::Weight) -> String {
    let token: String = family
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("font-family-{}-{token}", weight.0)
}

/// Bump when the chain's *meaning* changes (order, membership rules): the font
/// dirs' mtimes will not have moved, so the old file would otherwise be served
/// as if the new resolver had produced it.
const CACHE_POLICY: u32 = 2;

fn policy_line() -> String {
    format!("p\t{CACHE_POLICY}")
}

fn mtime_parts(t: std::time::SystemTime) -> (u64, u32) {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => (d.as_secs(), d.subsec_nanos()),
        Err(_) => (0, 0), // pre-epoch clock: constant key
    }
}

/// `None` = cache missing, unparsable, or keyed to different fonts.
fn read_cache(
    path: &Path,
    key: &[(PathBuf, std::time::SystemTime)],
) -> Option<Vec<(PathBuf, u32)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "wlmenu-font-cache v1" {
        return None;
    }
    // The resolver's policy is part of the key: a new chain order must not be
    // served from an old file (dir mtimes alone would not notice). Mismatch
    // lands in the serve-stale path, so the change costs no latency.
    if lines.next()? != policy_line() {
        return None;
    }
    for (dir, mt) in key {
        let mut it = lines.next()?.splitn(4, '\t');
        let (tag, d, s, n) = (
            it.next()?,
            it.next()?,
            it.next()?.parse::<u64>().ok()?,
            it.next()?.parse::<u32>().ok()?,
        );
        if tag != "d" || d != dir.to_str()? || (s, n) != mtime_parts(*mt) {
            return None;
        }
    }
    parse_chain(lines)
}

/// The cached chain regardless of the key: what a stale cache still offers.
fn read_chain(path: &Path) -> Option<Vec<(PathBuf, u32)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "wlmenu-font-cache v1" {
        return None;
    }
    parse_chain(lines.skip_while(|l| l.starts_with("d\t") || l.starts_with("p\t")))
}

fn parse_chain<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Vec<(PathBuf, u32)>> {
    let mut chain = Vec::new();
    for line in lines {
        let mut it = line.splitn(3, '\t');
        if it.next()? != "f" {
            return None;
        }
        let index: u32 = it.next()?.parse().ok()?;
        chain.push((PathBuf::from(it.next()?), index));
    }
    if chain.is_empty() {
        return None;
    }
    Some(chain)
}

/// Best-effort: any I/O failure just means the next launch rescans.
fn write_cache(path: &Path, key: &[(PathBuf, std::time::SystemTime)], chain: &[(PathBuf, u32)]) {
    if chain.is_empty() {
        return;
    }
    let mut text = String::from("wlmenu-font-cache v1\n");
    text.push_str(&policy_line());
    text.push('\n');
    for (dir, mt) in key {
        let Some(d) = dir.to_str() else { return };
        let (s, n) = mtime_parts(*mt);
        text.push_str(&format!("d\t{d}\t{s}\t{n}\n"));
    }
    for (p, i) in chain {
        let Some(p) = p.to_str() else { return };
        if p.contains('\n') {
            return;
        }
        text.push_str(&format!("f\t{i}\t{p}\n"));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path); // atomic: never serve a torn cache
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_family_spec_extracts_name_weight_size() {
        // 13 pt (Pango/wmenu convention) at 96 dpi.
        let (fam, w, sz) = parse_spec("SourceCodePro medium 13");
        assert_eq!(fam, "SourceCodePro");
        assert_eq!(w, fontdb::Weight::MEDIUM);
        assert_eq!(sz, Some(13.0 * 96.0 / 72.0));
    }

    #[test]
    fn px_suffix_is_absolute_pixels_not_points() {
        let (_, _, sz) = parse_spec("monospace 12px");
        assert_eq!(sz, Some(12.0));
        // The same bare number is points: 12 pt == 16 px at 96 dpi.
        let (_, _, sz) = parse_spec("monospace 12");
        assert_eq!(sz, Some(16.0));
    }

    #[test]
    fn load_wmenu_style_family_with_weight_and_size() {
        // The documented `-f "Noto Sans Mono Medium 12"` form: 12 pt == 16 px
        // at 96 dpi, so it renders identically to wmenu.
        let m = MenuFont::load(Some("Noto Sans Mono Medium 12"), 16.0).expect("medium face found");
        assert_eq!(m.size, 16.0);
        assert!(m.has_glyph('a'));
    }

    #[test]
    fn file_spec_loads_exactly_that_font_without_chain() {
        // `-f /path.ttf` must not build the system chain: no fallbacks at all
        // (wmenu -f semantics) — this is the fast-startup path. The path comes
        // from the app's own discovery, so the test does not have to guess the
        // layout (Arch keeps ttfs two levels down, Debian adds a family dir).
        let (file, _) = system_chain().into_iter().next().expect("a system font");
        let m = MenuFont::load(file.to_str(), 16.0).expect("font file loads");
        assert_eq!(m.face_count(), 1, "explicit file means no fallback chain");
        assert_eq!(m.size, 16.0);
    }

    #[test]
    fn the_cjk_face_is_not_read_before_a_cjk_glyph() {
        // Startup must read the primary only: the CJK face is 27MB and used to
        // be read before the first frame just to draw Latin text.
        let m = MenuFont::load(None, 16.0).expect("system font available");
        let fallbacks = m.face_count() - 1;
        if fallbacks == 0 {
            return; // single-font system: nothing to be lazy about
        }
        assert_eq!(m.unread_faces(), fallbacks, "only the primary was read");
        assert!(m.has_glyph('中'), "CJK must resolve on demand");
        assert!(
            m.unread_faces() < fallbacks,
            "the CJK lookup must have read at least one fallback face"
        );
    }

    #[test]
    fn chain_takes_one_cjk_face_only() {
        // SC/TC/JP/文泉驿/思源 are alternates, not complements (usually faces of
        // the same ~20MB TTC): the chain must never hold more than one big face.
        // Threshold is above any nerd-font/mono face (<8MB) but below a CJK TTC.
        let chain = system_chain();
        let big = chain
            .iter()
            .filter(|(p, _)| {
                std::fs::metadata(p)
                    .map(|m| m.len() > 8_000_000)
                    .unwrap_or(false)
            })
            .count();
        assert!(big <= 1, "at most one large (CJK) face in chain: {chain:?}");
    }

    #[test]
    fn font_cache_round_trips_and_invalidates_on_key_change() {
        let dir = std::env::temp_dir().join(format!("wlmenu-cache-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let font = dir.join("a.ttf");
        std::fs::write(&font, b"font").unwrap();
        let path = dir.join("font-chain");
        let key = vec![(
            font.clone(),
            std::fs::metadata(&font).unwrap().modified().unwrap(),
        )];
        let chain = vec![(font.clone(), 3u32)];
        write_cache(&path, &key, &chain);
        assert_eq!(read_cache(&path, &key), Some(chain.clone()));
        // A changed dir mtime (font added/removed) invalidates the cache.
        let stale = vec![(
            font.clone(),
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000),
        )];
        assert_eq!(read_cache(&path, &stale), None);
        // ...but the chain is still readable, which is what serve-stale uses.
        assert_eq!(read_chain(&path), Some(chain));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_usable_stale_chain_beats_rescanning() {
        let mk = |p: &str| vec![(PathBuf::from(p), 0u32)];
        let have = |p: &Path| p == Path::new("/have.ttf");
        assert!(matches!(
            cache_use(Some(mk("/have.ttf")), None, have),
            CacheUse::Fresh(_)
        ));
        // Key mismatch, paths intact: serve now, refresh on a thread.
        assert!(matches!(
            cache_use(None, Some(mk("/have.ttf")), have),
            CacheUse::Stale(_)
        ));
        // A vanished path is never served, fresh or stale.
        assert!(matches!(
            cache_use(Some(mk("/gone.ttf")), Some(mk("/gone.ttf")), have),
            CacheUse::None
        ));
        assert!(matches!(cache_use(None, None, have), CacheUse::None));
    }

    #[test]
    fn family_cache_name_is_per_family_and_weight() {
        let normal = family_file_name("Noto Sans Mono", fontdb::Weight::NORMAL);
        assert_eq!(normal, "font-family-400-Noto_Sans_Mono");
        assert_ne!(
            normal,
            family_file_name("Noto Sans Mono", fontdb::Weight::BOLD)
        );
        assert_ne!(
            normal,
            family_file_name("Noto Sans CJK SC", fontdb::Weight::NORMAL)
        );
        // Odd/long families still make one legal filename.
        let long = family_file_name(&"\u{dc}nicode fa/mily ".repeat(20), fontdb::Weight::NORMAL);
        assert!(long.len() < 255 && !long.contains('/'));
    }

    #[test]
    fn chain_renders_nerd_font_pua_icons() {
        // Skip on systems without any Nerd Font (no PUA icons to draw anyway).
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let sample = db.faces().find_map(|f| {
            if !f
                .families
                .iter()
                .any(|f| f.0.to_ascii_lowercase().contains("nerd font"))
            {
                return None;
            }
            db.with_face_data(f.id, |d, i| (d.to_vec(), i))
                .and_then(|(d, i)| FontVec::try_from_vec_and_index(d, i).ok())
        });
        let Some(fv) = sample else { return };
        // Pick one PUA codepoint the installed Nerd face actually covers.
        let scaled = fv.as_scaled(PxScale::from(16.0));
        let Some(cp) = (0xF000..=0xFDFF)
            .find(|&cp| scaled.glyph_id(char::from_u32(cp).unwrap()) != GlyphId(0))
        else {
            return;
        };
        let m = MenuFont::load(None, 16.0).expect("system font available");
        assert!(
            m.has_glyph(char::from_u32(cp).unwrap()),
            "Nerd Font PUA glyph U+{cp:X} must resolve via the chain"
        );
    }

    #[test]
    fn provisional_row_height_matches_the_loaded_font() {
        // The layer surface commits at `row_h_for(size)` before the font exists,
        // so the two must agree for every size a spec can ask for.
        let (file, _) = system_chain().into_iter().next().expect("a system font");
        for size in [13.0, 16.0, 32.0, 20.0 * 96.0 / 72.0] {
            let m = MenuFont::load(file.to_str(), size).expect("font file loads");
            assert_eq!(m.row_h, row_h_for(size), "size {size}");
        }
        let m = MenuFont::load(Some("Noto Sans Mono 12"), 16.0).expect("family loads");
        assert_eq!(m.row_h, row_h_for(m.size));
    }

    #[test]
    fn spec_size_matches_what_load_will_use() {
        assert_eq!(spec_size(None, 16.0), 16.0);
        // Family + size token: `Npx` is absolute, a bare number is points.
        assert_eq!(spec_size(Some("Noto Sans Mono 20px"), 16.0), 20.0);
        assert_eq!(spec_size(Some("Noto Sans Mono 12"), 16.0), 16.0);
        // A readable path is used as a font file and carries no size token.
        let (file, _) = system_chain().into_iter().next().expect("a system font");
        assert_eq!(spec_size(file.to_str(), 16.0), 16.0);
        // Whatever it predicts must be what the font actually comes out as.
        let m = MenuFont::load(Some("Noto Sans Mono 20px"), 16.0).expect("family loads");
        assert_eq!(m.size, spec_size(Some("Noto Sans Mono 20px"), 16.0));
    }

    #[test]
    fn parse_family_plain_and_multiword() {
        let (fam, w, sz) = parse_spec("monospace");
        assert_eq!(fam, "monospace");
        assert_eq!(w, fontdb::Weight::NORMAL);
        assert_eq!(sz, None);

        let (fam, w, sz) = parse_spec("Source Code Pro bold 12.5");
        assert_eq!(fam, "Source Code Pro");
        assert_eq!(w, fontdb::Weight::BOLD);
        assert_eq!(sz, Some(12.5 * 96.0 / 72.0));
    }

    #[test]
    fn unknown_style_word_stays_in_family() {
        let (fam, _, _) = parse_spec("Fira Code Extra");
        assert_eq!(fam, "Fira Code Extra");
    }

    #[test]
    fn fallback_chain_covers_cjk_for_latin_primary() {
        // Noto Sans Mono has no CJK glyphs; the chain must resolve them.
        let m = MenuFont::load(Some("Noto Sans Mono"), 13.0).expect("system font available");
        assert!(m.has_glyph('a'));
        assert!(
            m.has_glyph('中'),
            "CJK must resolve via the fallback chain when the primary is latin-only"
        );
    }
}
