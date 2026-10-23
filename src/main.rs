//! wlmenu — a wmenu/dmenu-style menu for Wayland (wlr-layer-shell).
//!
//! Plain text or `--run` launcher mode. `--run` scans .desktop files;
//! otherwise lines are read from stdin.

mod anim;
mod control;
mod desktop;
mod feed;
mod font;
mod items;
mod render;

use anim::{ANIM, Anim, EXIT};
use control::Claim;
use feed::ItemFeed;

use std::io::IsTerminal;
use std::process::exit;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;

use smithay_client_toolkit::{
    activation::{ActivationHandler, ActivationState, RequestData},
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
};

/// Width of the first commit, before `surface_enter` reports the output; the
/// panel then resizes to `auto_width` of that output (unless `-W` pins it).
const DEFAULT_WIDTH: u32 = 640;
/// Default panel width as a percentage of the output width, clamped. Spotlight is
/// a fixed size on macOS (it cannot be resized), so these are a product choice
/// modelled on its footprint at a common Mac resolution, not a spec quote.
const WIDTH_PCT: u32 = 45;
const AUTO_WIDTH_MIN: u32 = 480;
const AUTO_WIDTH_MAX: u32 = 960;
/// Default body size in px (15 pt at 96 dpi): a fraction of the panel width, so
/// the text keeps its size relative to the screen. 1/32 of 640 px is 20 px.
const FONT_PX_DIV: f32 = 32.0;
const FONT_PX_MIN: f32 = 14.0;
const FONT_PX_MAX: f32 = 30.0;
const FONT_SIZE: f32 = 20.0;
/// Text antialiasing: LCD subpixel (`Rgb`) is noticeably sharper on 1× displays.
/// Switch to `Bgr` for BGR-stripe panels (text would fringe on the wrong side)
/// or `Gray` for neutral rendering without colour fringing.
const TEXT_AA: render::Subpixel = render::Subpixel::Rgb;
/// Away-from-edge float for the upper-center panel (bottom mode floats too).
const TOP_MARGIN: i32 = 32;
// The panel sits inside the shadow margin, so the surface margin is the visible
// gap minus the margin the shadow already occupies.
const _: () = assert!(TOP_MARGIN >= render::SHADOW as i32);
/// Where the panel's top edge sits on a known-height output: a bit over a quarter
/// of the screen — the band the eye lands in without looking — leaving the rest
/// of the height for the list. `TOP_MARGIN` is the floor, for a short output and
/// for the frame before the height is known.
const TOP_PCT: u32 = 26;
/// ponytail: stdin can be huge; cap drawn rows and scroll instead of mapping a giant buffer.
const MAX_VISIBLE: usize = 64;
/// Rows of context kept above/below the selection while the list scrolls.
const SCROLLOFF: usize = 1;
/// Sanity bounds for `-W`/`-l`: zero or absurd values would size a degenerate
/// or huge shm buffer.
const MAX_WIDTH: u32 = 16384;
const MAX_LINES: usize = 1000;
/// Caret blink half-period; typing resets it to visible.
const BLINK_PERIOD: Duration = Duration::from_millis(500);
/// Frame clock for height transitions and while stdin is still streaming.
const FRAME: Duration = Duration::from_millis(16);
/// How long a row that has just arrived takes to reach full opacity. Short
/// enough that a fast typist never waits for it, long enough that the eye reads
/// the row as arriving instead of the list as swapping.
const CONTENT: Duration = Duration::from_millis(120);
/// How long the overlay scrollbar lingers after the list last moved.
const SCROLL_FADE: Duration = Duration::from_millis(400);
struct Opts {
    prompt: String,
    /// `-W`: absolute width. `None` means "a fraction of the output width"
    /// (`auto_width`), resolved once the output size is known.
    width: Option<u32>,
    lines: usize,
    ci: bool,
    font: Option<String>,
    /// `-o NAME`: show the panel on that output (wmenu `-o`).
    output: Option<String>,
    run: bool,
    bottom: bool,
    password: bool,
    colors: render::Colors,
}

fn usage() -> ! {
    eprintln!(
        "usage: wlmenu [-biPv] [-f font.ttf|FAMILY [style] [pt|Npx]] [-l lines] [-W width] [-p prompt] [-o output] [--run]\n\
               [-N color] [-n color] [-M color] [-m color] [-S color] [-s color]\n\
         \n\
         Reads lines from stdin and prints the selected line to stdout.\n\
         `--run` ignores stdin, lists .desktop applications, and launches the selection.\n\
         `-b` shows the menu at the bottom of the screen; `-P` masks typed input as asterisks;\n\
         `-i` matches case-insensitively; `-o` shows the menu on the named output.\n\
         Colors are wmenu-style `RRGGBB[AA]`: `-N`/`-n` normal bg/fg, `-M`/`-m` prompt bg/fg,\n\
         `-S`/`-s` selection bg/fg."
    );
    exit(1);
}

fn parse_opts_from(args: impl Iterator<Item = String>) -> Opts {
    let mut o = Opts {
        prompt: String::new(),
        width: None,
        lines: 0,
        ci: false,
        font: None,
        output: None,
        run: false,
        bottom: false,
        password: false,
        colors: render::Colors::default(),
    };
    let mut args = args;
    while let Some(a) = args.next() {
        let color = |s: String| render::parse_color(&s).unwrap_or_else(|| usage());
        match a.as_str() {
            "-i" => o.ci = true,
            "-b" => o.bottom = true,
            "-P" => o.password = true,
            "-p" => o.prompt = args.next().unwrap_or_else(|| usage()),
            "-l" => {
                let n: usize = args
                    .next()
                    .unwrap_or_else(|| usage())
                    .parse()
                    .unwrap_or_else(|_| usage());
                if !valid_lines(n) {
                    usage();
                }
                o.lines = n;
            }
            "-W" => {
                let n: u32 = args
                    .next()
                    .unwrap_or_else(|| usage())
                    .parse()
                    .unwrap_or_else(|_| usage());
                if !valid_width(n) {
                    usage();
                }
                o.width = Some(n);
            }
            "-f" => o.font = Some(args.next().unwrap_or_else(|| usage())),
            "-o" => o.output = Some(args.next().unwrap_or_else(|| usage())),
            "-N" => o.colors.bg_normal = color(args.next().unwrap_or_else(|| usage())),
            "-n" => o.colors.fg_normal = color(args.next().unwrap_or_else(|| usage())),
            "-M" => o.colors.bg_prompt = color(args.next().unwrap_or_else(|| usage())),
            "-m" => o.colors.fg_prompt = color(args.next().unwrap_or_else(|| usage())),
            "-S" => o.colors.bg_sel = color(args.next().unwrap_or_else(|| usage())),
            "-s" => o.colors.fg_sel = color(args.next().unwrap_or_else(|| usage())),
            "--run" => o.run = true,
            "-h" | "--help" => usage(),
            _ => {
                eprintln!("wlmenu: unknown option {a}");
                usage();
            }
        }
    }
    o
}

fn parse_opts() -> Opts {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-v" || a == "--version") {
        println!("wlmenu {}", env!("CARGO_PKG_VERSION"));
        exit(0);
    }
    parse_opts_from(args.into_iter())
}

fn main() {
    let opts = parse_opts();

    // Single instance: a repeat launch dismisses the panel that is already up
    // instead of stacking another one on top of it (Wayland happily layers
    // them, and each one has to be cancelled separately). Claimed before the
    // font load and the Wayland connection, so a repeat launch costs ~1 ms and
    // never creates a surface.
    let control = control::claim();
    if let Claim::Dismissed = control {
        exit(1);
    }

    // `--run` scans `.desktop` files and `$PATH` on a thread: the scan used to
    // run before the Wayland connection, so the bar could not appear until it
    // finished. Items now arrive through the same queue stdin uses; an empty
    // result still exits 1, via the `no_items` path in `sync_items`.
    let (items, feed): (Vec<items::Item>, Option<Arc<ItemFeed>>) = if opts.run {
        let feed = ItemFeed::spawn_with(|| {
            desktop::merged(desktop::load_apps(), desktop::path_commands())
        });
        (Vec::new(), Some(feed))
    } else {
        // Without a pipe (and without `--run`) this would just look hung while
        // it waits for stdin, so say what is happening. Behavior is unchanged.
        if std::io::stdin().is_terminal() {
            eprintln!("wlmenu: reading items from the terminal; pipe or redirect stdin");
        }
        (Vec::new(), Some(ItemFeed::spawn()))
    };

    // The font is read on a thread: it runs alongside the Wayland handshake
    // below and the compositor's first configure, and is joined by the first
    // frame (`App::ensure_font`). So the panel commits at its final row height
    // without waiting for any font file: `row_h_for` needs only the size, which
    // the `-f` spec already fixes.
    // The body size this run starts at: `-f`'s own size token, else the default.
    // The output width can raise it once `surface_enter` knows the screen.
    let start_px = font::spec_size(opts.font.as_deref(), FONT_SIZE);
    let bar_h = font::row_h_for(start_px);
    let font_thread = font::spawn_load(opts.font.clone(), start_px);

    let conn = Connection::connect_to_env().unwrap_or_else(|e| {
        eprintln!("wlmenu: cannot connect to Wayland: {e}");
        exit(1);
    });
    let (globals, event_queue) = registry_queue_init(&conn).unwrap_or_else(|e| {
        eprintln!("wlmenu: cannot init globals: {e}");
        exit(1);
    });
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell unsupported");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm missing");
    // calloop drives both the Wayland socket and sctk's key-repeat timer, so
    // holding a key (C-p/C-n, arrows) auto-repeats at the compositor's rate.
    let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap_or_else(|e| {
        eprintln!("wlmenu: {e}");
        exit(1);
    });
    let loop_handle = event_loop.handle();
    let mut event_queue = event_queue;

    let output_state = OutputState::new(&globals, &qh);
    let height = bar_h * (row_capacity(opts.lines) as u32 + 1);
    let shadow = 2 * render::SHADOW;
    // Hint for the widest the panel can become, not just the first commit's width.
    let hint_w = opts.width.unwrap_or(AUTO_WIDTH_MAX);
    let pool = SlotPool::new(((hint_w + shadow) * (height + shadow) * 4) as usize, &shm)
        .expect("failed to allocate shm pool");
    let item_count = items.len();
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state,
        shm,
        compositor,
        layer_shell,
        layer: None,
        pool,
        qh: qh.clone(),
        conn: conn.clone(),
        activation: ActivationState::bind(&globals, &qh).ok(),
        token: None,
        keyboard: None,
        items,
        feed,
        menu: MenuState::new(item_count).with_prompt(!opts.prompt.is_empty()),
        top: 0,
        font: None,
        pending: Some((start_px, font_thread)),
        font_spec: opts.font.clone(),
        prompt: opts.prompt.clone(),
        width: opts.width.unwrap_or(DEFAULT_WIDTH),
        width_explicit: opts.width,
        font_px: start_px,
        scale: 1,
        lines: opts.lines,
        ci: opts.ci,
        colors: opts.colors,
        password: opts.password,
        bottom: opts.bottom,
        output_w: None,
        output_h: None,
        dirty: false,
        frame_pending: false,
        blink: true,
        next_blink: Instant::now() + BLINK_PERIOD,
        anim: None,
        target_h: None,
        shown_h: bar_h,
        prev_visible: Vec::new(),
        baseline: Vec::new(),
        last_query: String::new(),
        content_start: None,
        scroll_at: None,
        closing: false,
        sent: Sent::default(),
        first_configure: true,
        run: opts.run,
        launches: Vec::new(),
        no_items: false,
        mods: Modifiers::default(),
        // Cloned: the loop sources are inserted after the `-o` roundtrip.
        loop_handle: loop_handle.clone(),
    };

    // `-o NAME` binds the panel to that output; the compositor picks otherwise.
    // Output *names* arrive as events on the loop's own queue, so they have to
    // be dispatched through it — `Connection::roundtrip` only drains the
    // connection's inner queue, which leaves `output_state` empty and made
    // every `-o` fail. Runs before the loop, so it costs a roundtrip only when
    // `-o` is given.
    let target = match opts.output.as_deref() {
        Some(name) => {
            event_queue.roundtrip(&mut app).unwrap_or_else(|e| {
                eprintln!("wlmenu: {e}");
                exit(1);
            });
            let found = app
                .output_state
                .outputs()
                .find(|o| app.output_state.info(o).and_then(|i| i.name).as_deref() == Some(name));
            match found {
                Some(o) => Some(o),
                None => {
                    eprintln!("wlmenu: no output named {name}");
                    exit(1);
                }
            }
        }
        None => None,
    };

    WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle.clone())
        .unwrap_or_else(|e| {
            eprintln!("wlmenu: {e}");
            exit(1);
        });
    // The dismissal channel from `control::claim`: a second launch connecting
    // to our socket closes this menu, exactly like Escape does. Level-triggered
    // on the loop's poller, so idle wakeups are unchanged.
    if let Claim::Owner(listener) = control {
        loop_handle
            .insert_source(
                Generic::new(listener, Interest::READ, Mode::Level),
                |_, listener, app: &mut App| {
                    let _ = listener.accept();
                    app.menu.done = Some(Done::Cancel);
                    Ok(PostAction::Continue)
                },
            )
            .unwrap_or_else(|e| {
                eprintln!("wlmenu: {e}");
                exit(1);
            });
    }

    let surface = app.compositor.create_surface(&app.qh);
    let layer = app.layer_shell.create_layer_surface(
        &app.qh,
        surface,
        Layer::Overlay,
        Some("wlmenu"),
        target.as_ref(),
    );
    layer.set_anchor(if app.bottom {
        Anchor::BOTTOM
    } else {
        Anchor::TOP
    });
    layer.set_margin(
        if app.bottom {
            0
        } else {
            top_margin(app.output_h) as i32
        },
        0,
        0,
        0,
    );
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    // Start collapsed to the single input bar (Spotlight); the list appears
    // once the query has content. The pool keeps the max height for rows.
    // The surface carries the panel plus its shadow margin on both sides, so the
    // margin above is the panel's visible gap to the screen edge minus that
    // margin.
    layer.set_size(app.width + shadow, bar_h + shadow);
    layer.commit();
    app.layer = Some(layer);

    loop {
        // Sleep only as long as the next deadline allows: streaming items and a
        // running height transition need the frame clock, otherwise the caret
        // blink is the only thing left. Wayland input wakes the loop
        // immediately, so an idle menu goes from ~62 wakeups/s to ~2/s.
        let now = Instant::now();
        let busy = app.streaming()
            || app.anim.as_ref().is_some_and(|a| a.at(now).2)
            || app.content_fading(now)
            || app.scroll_fading(now);
        if event_loop
            .dispatch(loop_timeout(busy, app.next_blink, now), &mut app)
            .is_err()
        {
            break;
        }
        app.flush_launches();
        app.sync_items();
        app.tick();
        if app.menu.done.is_some() {
            // Play the exit transition before finishing: printing, launching and
            // the exit code all wait for it.
            app.begin_close();
            if app.close_finished() {
                break;
            }
        }
    }
    app.finish();
}

/// How long the event loop may sleep. `busy` covers everything that needs the
/// frame clock (a height transition, the row fade-in, the scrollbar fading, items
/// still streaming in); otherwise the caret blink is the only deadline.
fn loop_timeout(busy: bool, next_blink: Instant, now: Instant) -> Duration {
    if busy {
        FRAME
    } else {
        next_blink.saturating_duration_since(now)
    }
}

/// Opacity of a row that just arrived: 0 → 255 over `CONTENT`, ease-out. `None`
/// means no refresh is running, so everything is whole.
fn fresh_alpha(elapsed: Option<Duration>) -> u8 {
    let Some(elapsed) = elapsed else {
        return 255;
    };
    let t = (elapsed.as_secs_f32() / CONTENT.as_secs_f32()).clamp(0.0, 1.0);
    ((1.0 - (1.0 - t).powi(3)) * 255.0).round() as u8
}

/// Opacity for one visible row: a row already on screen stays whole (no blink
/// under a keystroke that merely reorders), one that just arrived ramps in.
fn row_fade(prev: &[usize], index: usize, elapsed: Option<Duration>) -> u8 {
    if prev.contains(&index) {
        255
    } else {
        fresh_alpha(elapsed)
    }
}

/// Opacity of the overlay scrollbar: full at the moment the list moved, gone
/// `SCROLL_FADE` later. `None` means it has never moved, so there is no thumb.
fn scroll_alpha(elapsed: Option<Duration>) -> u8 {
    let Some(elapsed) = elapsed else {
        return 0;
    };
    let t = (elapsed.as_secs_f32() / SCROLL_FADE.as_secs_f32()).clamp(0.0, 1.0);
    ((1.0 - t) * 255.0).round() as u8
}

/// Top row of the visible window: keeps the selection in view with `SCROLLOFF`
/// rows of context on each side, using the previous top as hysteresis so the
/// list does not jitter while the selection moves inside the window. Pure, so
/// it is testable without a compositor.
fn viewport_top(sel: usize, top: usize, visible: usize, total: usize) -> usize {
    if visible == 0 {
        return 0;
    }
    let max_top = total.saturating_sub(visible);
    if total > visible && sel + SCROLLOFF >= top + visible {
        (sel + SCROLLOFF + 1).saturating_sub(visible).min(max_top)
    } else if sel < top.saturating_add(SCROLLOFF) {
        sel.saturating_sub(SCROLLOFF).min(max_top)
    } else {
        // Inside the window: keep the offset, but clamp a stale one (the list
        // can shrink under a filter, and the slice below would panic).
        top.min(max_top)
    }
}

/// `-W` must be a usable panel width (0 would create a zero-width buffer).
fn valid_width(w: u32) -> bool {
    (1..=MAX_WIDTH).contains(&w)
}

/// `-l 0` means "auto" (see `row_capacity`); the cap keeps the pool hint sane.
fn valid_lines(n: usize) -> bool {
    n <= MAX_LINES
}

fn top_margin(output_h: Option<u32>) -> u32 {
    output_h.map_or(TOP_MARGIN as u32, |h| {
        (h * TOP_PCT / 100).max(TOP_MARGIN as u32)
    })
}

/// List rows that fit on an output `output_h` logical px tall between the panel's
/// top and bottom gaps: one row of that budget is the input bar, and `SEPARATION`
/// px is the air between it and the list. Saturating throughout, so a screen too
/// short for a single row yields 0 instead of underflowing into a huge count.
fn max_rows(output_h: u32, row_h: u32, top: u32, bottom: u32, scale: u32) -> usize {
    let s = scale.max(1);
    let budget = output_h
        .saturating_mul(s)
        .saturating_sub(top.saturating_mul(s))
        .saturating_sub(bottom.saturating_mul(s))
        .saturating_sub(row_h)
        .saturating_sub(render::SEPARATION * s);
    (budget / row_h.max(1)) as usize
}

/// Default panel width for an output: a fixed fraction of it, clamped so the bar
/// stays a bar on a small screen and does not become a page on a large one.
fn auto_width(output_w: u32) -> u32 {
    (output_w.saturating_mul(WIDTH_PCT) / 100).clamp(AUTO_WIDTH_MIN, AUTO_WIDTH_MAX)
}

/// Default text size for a panel width, so bar height and row height scale with
/// the screen instead of staying fixed while the panel grows around them.
fn auto_font(width: u32) -> f32 {
    (width as f32 / FONT_PX_DIV).clamp(FONT_PX_MIN, FONT_PX_MAX)
}

/// Content inset: 0.8 em, as in a mac menu, and never less than the corner
/// radius so text cannot graze the curve.
fn pad_for(font_px: f32) -> u32 {
    ((font_px * 0.8).round() as u32).max(render::CORNER_RADIUS)
}

/// Spotlight collapse: the list stays hidden until the query has content — unless
/// a `-p` prompt makes this a confirm-style menu, whose answers must be visible.
fn list_hidden(query: &str, prompt: &str) -> bool {
    query.is_empty() && prompt.is_empty()
}

/// Subpixel AA pays off on 1× outputs; on HiDPI the pixel grid is already dense,
/// so it only adds colour fringing and ~55% more paint time there.
fn text_aa(scale: u32) -> render::Subpixel {
    if scale > 1 {
        render::Subpixel::Gray
    } else {
        TEXT_AA
    }
}

/// Row capacity for the shm pool: stdin streams in, so size for the cap up
/// front instead of whatever happened to arrive by startup. (The pool grows on
/// demand anyway; this is the initial hint.)
fn row_capacity(lines: usize) -> usize {
    if lines > 0 { lines } else { MAX_VISIBLE }
}

/// Menu outcome: `None` while running, `Some` once the user (or an error) closed it.
#[derive(Debug, PartialEq)]
enum Done {
    Cancel,
    Select(String),
}

/// Keyboard/filter state, Wayland-free so it is unit-testable.
struct MenuState {
    query: String,
    matches: Vec<usize>,
    sel: usize,
    done: Option<Done>,
    /// Values picked by Ctrl-Return while the menu keeps running (multi-select).
    picked: Vec<String>,
    /// The list is on screen from the start: a `-p` prompt makes this a question
    /// with ready-made answers rather than a Spotlight launcher, so Enter may take
    /// the highlighted row before anything is typed.
    prompt: bool,
}

impl MenuState {
    fn new(count: usize) -> Self {
        Self {
            query: String::new(),
            matches: (0..count).collect(),
            sel: 0,
            done: None,
            picked: Vec::new(),
            prompt: false,
        }
    }

    /// `-p` was given (see [`MenuState::prompt`]).
    fn with_prompt(mut self, prompt: bool) -> Self {
        self.prompt = prompt;
        self
    }

    fn refilter(&mut self, items: &[items::Item], ci: bool) {
        self.matches = items::filter(items, &self.query, ci);
        self.sel = 0;
    }

    /// The value Return picks: the highlighted item, or (dmenu/wmenu contract)
    /// the typed line when there is no match — that's how `echo "" | wlmenu -p …`
    /// prompt-style scripts work.
    fn pick_value(&self, items: &[items::Item]) -> String {
        match self.matches.get(self.sel) {
            Some(&i) => items[i].value.clone(),
            None => self.query.clone(),
        }
    }

    /// Plain Return: the highlighted row when the list is on screen, else cancel
    /// on an empty query (hidden list: nothing is visible to pick) or echo the
    /// typed line when nothing matches (dmenu contract).
    fn enter(&mut self, items: &[items::Item]) {
        let pickable = !self.query.is_empty() || self.prompt;
        self.done = match self.matches.get(self.sel) {
            Some(_) if pickable => Some(Done::Select(self.pick_value(items))),
            _ if self.query.is_empty() => Some(Done::Cancel),
            _ => Some(Done::Select(self.query.clone())),
        };
    }

    fn on_key(
        &mut self,
        keysym: Keysym,
        utf8: Option<String>,
        mods: Modifiers,
        items: &[items::Item],
        ci: bool,
        visible: usize,
    ) {
        match keysym {
            Keysym::Escape => self.done = Some(Done::Cancel),
            // Ctrl-Return: pick the highlighted entry, print it, and keep the
            // menu open so scripts can multi-select (App emits `picked` after
            // this call); advance so repeated presses walk the list.
            Keysym::Return | Keysym::KP_Enter if mods.ctrl => {
                self.picked.push(self.pick_value(items));
                if self.sel + 1 < self.matches.len() {
                    self.sel += 1;
                }
            }
            // Shift-Return: submit exactly what's typed, matches or not.
            Keysym::Return | Keysym::KP_Enter if mods.shift => {
                self.done = Some(Done::Select(self.query.clone()));
            }
            Keysym::Return | Keysym::KP_Enter => self.enter(items),
            Keysym::BackSpace => {
                if self.query.pop().is_some() {
                    self.refilter(items, ci);
                }
            }
            Keysym::Up => {
                if self.sel > 0 {
                    self.sel -= 1;
                }
            }
            Keysym::Down => {
                if self.sel + 1 < self.matches.len() {
                    self.sel += 1;
                }
            }
            Keysym::Page_Up => {
                self.sel = self.sel.saturating_sub(visible.max(1));
            }
            Keysym::Page_Down => {
                self.sel = (self.sel + visible.max(1)).min(self.matches.len().saturating_sub(1));
            }
            Keysym::Home => self.sel = 0,
            Keysym::End => self.sel = self.matches.len().saturating_sub(1),
            Keysym::Tab => {
                // dmenu habit: complete the highlighted item into the query.
                if let Some(&i) = self.matches.get(self.sel) {
                    let text = items[i].text.clone();
                    if text != self.query {
                        self.query = text;
                        self.refilter(items, ci);
                        // refilter resets sel to 0; anchor back on the completed item
                        // (prefix ties like "libreoffice" vs "libreoffice-stable").
                        if let Some(p) = self.matches.iter().position(|&m| m == i) {
                            self.sel = p;
                        }
                    }
                }
            }
            _ => {
                // xkb encodes ctrl+key as a single control char (ctrl+a -> U+0001).
                // Bind the chords we support; everything else must not become
                // query text (the old guard dropped them silently).
                if let Some(t) = utf8 {
                    match t.chars().next() {
                        Some('\u{3}') | Some('\u{7}') => self.done = Some(Done::Cancel), // C-c / C-g
                        Some('\u{8}') => {
                            // C-h = backspace
                            if self.query.pop().is_some() {
                                self.refilter(items, ci);
                            }
                        }
                        Some('\u{a}') | Some('\u{d}') => self.enter(items), // C-j / C-m = Return
                        Some('\u{15}') => {
                            // C-u: delete everything left of the caret (end of line)
                            if !self.query.is_empty() {
                                self.query.clear();
                                self.refilter(items, ci);
                            }
                        }
                        Some('\u{e}') => {
                            // C-n: next row
                            if self.sel + 1 < self.matches.len() {
                                self.sel += 1;
                            }
                        }
                        Some('\u{10}') => {
                            // C-p: previous row
                            if self.sel > 0 {
                                self.sel -= 1;
                            }
                        }
                        Some('\u{17}') => {
                            // C-w: kill the last word and its separator (readline
                            // style). Caret is at the end — no cursor support yet.
                            let end = self.query.trim_end_matches(' ').len();
                            let start = self.query[..end].rfind(' ').unwrap_or(0);
                            self.query.truncate(start);
                            self.refilter(items, ci);
                        }
                        _ => {
                            if !t.is_empty() && !t.chars().any(char::is_control) {
                                self.query.push_str(&t);
                                self.refilter(items, ci);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What the layer surface was last told, so a frame whose geometry did not change
/// sends no requests. Only changes cost a compositor reflow.
#[derive(Clone, Copy, Default, PartialEq)]
struct Sent {
    size: Option<(u32, u32)>,
    margin: Option<(i32, i32, i32, i32)>,
    scale: Option<u32>,
    /// Input region as (y, height) in surface-logical pixels; the width is the
    /// whole surface.
    input: Option<(u32, u32)>,
}

#[allow(dead_code)] // state objects kept alive for their proxy bindings
struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    compositor: CompositorState,
    layer_shell: LayerShell,
    layer: Option<LayerSurface>,
    pool: SlotPool,
    qh: QueueHandle<App>,
    /// Kept for the private activation queue `--run` launches roundtrip on.
    conn: Connection,
    /// xdg-activation, when the compositor offers it: a launched window without
    /// a token may not be allowed to take focus.
    activation: Option<ActivationState>,
    /// Token from the most recent activation request.
    token: Option<String>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// Latest modifier state; sctk drops modifiers from KeyEvent, so
    /// `update_modifiers` is the only place they arrive.
    mods: Modifiers,
    items: Vec<items::Item>,
    /// Stdin lines still streaming in (`None` in `--run` mode).
    feed: Option<Arc<ItemFeed>>,
    menu: MenuState,
    top: usize,
    /// The font, loaded on a thread at startup and re-derived on scale change;
    /// `None` until `ensure_font` joins the load.
    font: Option<font::MenuFont>,
    /// In-flight load: the size it was requested at, so a scale change can tell
    /// an obsolete load from one that already matches.
    pending: Option<(f32, font::FontLoad)>,
    /// The `-f` spec, kept so the font can be re-derived at a new scale.
    font_spec: Option<String>,
    prompt: String,
    /// Effective panel width: `-W`, or the output-derived default once known.
    width: u32,
    /// `-W` given: an absolute width the output size cannot override.
    width_explicit: Option<u32>,
    /// Logical default body size in effect (screen-derived unless `-f` pins it).
    font_px: f32,
    /// Output scale (HiDPI density); 1 until `surface_enter` reports it.
    scale: u32,
    lines: usize,
    ci: bool,
    colors: render::Colors,
    password: bool,
    bottom: bool,
    /// Logical size of the output the panel is mapped on; `None` until
    /// surface_enter. The width centers the panel, the height caps the list so
    /// the menu cannot grow past the screen.
    output_w: Option<u32>,
    output_h: Option<u32>,
    dirty: bool,
    frame_pending: bool,
    /// Caret blink state and when to toggle it next (typing keeps it solid).
    blink: bool,
    next_blink: Instant,
    /// Height and opacity transition in flight, its destination, and the height
    /// last rendered (the animation's starting point).
    anim: Option<Anim>,
    target_h: Option<u32>,
    shown_h: u32,
    /// Item indices drawn in the previous frame.
    prev_visible: Vec<usize>,
    /// The rows that were on screen when the query last changed. Held for the
    /// length of the fade, so a row that arrived this refresh keeps ramping on
    /// every frame instead of turning into a survivor on the second one; a row
    /// already in it never blinks (SPEC-UI-013).
    baseline: Vec<usize>,
    last_query: String,
    content_start: Option<Instant>,
    /// When the list last moved (the viewport top changed); the overlay scrollbar
    /// fades out from there.
    scroll_at: Option<Instant>,
    /// The menu is on its way out: the cards fade (the height is frozen), then the
    /// process finishes with whatever `done` says.
    closing: bool,
    /// What the layer surface was last told, so frames that change nothing in its
    /// geometry send no requests.
    sent: Sent,
    first_configure: bool,
    run: bool,
    /// `--run` picks waiting to be spawned. Filled from key handlers, drained
    /// outside the dispatch, where the activation roundtrip is safe.
    launches: Vec<String>,
    /// Stdin closed without producing a single line (keeps the old
    /// `wlmenu: no items` + exit 1 contract).
    no_items: bool,
    loop_handle: smithay_client_toolkit::reexports::calloop::LoopHandle<'static, App>,
}

impl App {
    fn visible(&self) -> usize {
        let rows = if self.lines > 0 {
            self.lines
        } else {
            MAX_VISIBLE
        };
        // The screen is a physical limit, `-l` a preference: the smaller wins.
        match self.screen_rows() {
            Some(fit) => rows.min(fit),
            None => rows,
        }
        .min(self.menu.matches.len())
    }

    /// Rows that fit on this output with the panel's gaps above and below;
    /// `None` until `surface_enter` reported the output height.
    fn screen_rows(&self) -> Option<usize> {
        let output_h = self.output_h?;
        // `row_h` is buffer px; before the font is joined the provisional height
        // is the same value startup used to size the first commit.
        let row_h = match &self.font {
            Some(font) => font.row_h,
            // Before the font is joined the provisional height is the same value
            // startup used to size the first commit.
            None => font::row_h_for(self.font_px * self.scale as f32),
        };
        // `-b` floats SHADOW off the bottom edge and reserves that at both ends;
        // the top-anchored panel starts at its optical top and reserves only the
        // shadow below, so the list is not squeezed by a margin twice.
        let (top, bottom) = if self.bottom {
            (render::SHADOW, render::SHADOW)
        } else {
            (top_margin(Some(output_h)), render::SHADOW)
        };
        Some(max_rows(output_h, row_h, top, bottom, self.scale))
    }

    /// True while stdin is still feeding items in (`--run` never streams).
    fn streaming(&self) -> bool {
        self.feed.as_ref().is_some_and(|f| !f.done())
    }

    /// True while rows that just arrived are still fading in, so the loop keeps
    /// the frame clock instead of sleeping to the next caret blink.
    fn content_fading(&self, now: Instant) -> bool {
        self.content_start
            .is_some_and(|s| now.saturating_duration_since(s) < CONTENT)
    }

    /// True while the overlay scrollbar is still fading, for the same reason.
    fn scroll_fading(&self, now: Instant) -> bool {
        self.scroll_at
            .is_some_and(|s| now.saturating_duration_since(s) < SCROLL_FADE)
    }

    /// Pull streamed stdin lines into the menu. Called every loop tick and
    /// before a key is handled, so typing always filters the freshest list.
    fn sync_items(&mut self) {
        let Some(feed) = self.feed.clone() else {
            return;
        };
        let fresh = feed.drain();
        if !fresh.is_empty() {
            let sel = self.menu.sel;
            self.items.extend(fresh);
            self.menu.refilter(&self.items, self.ci);
            // Keep the highlighted row steady while more data streams in.
            self.menu.sel = sel.min(self.menu.matches.len().saturating_sub(1));
            self.request_draw();
        }
        if feed.done() && self.items.is_empty() && !self.no_items {
            self.no_items = true;
            self.menu.done = Some(Done::Cancel);
        }
    }

    /// Install the font for this frame, joining an in-flight load. Called from
    /// `draw` only: that is what lets a load started at startup overlap the
    /// Wayland handshake and the configure round trip.
    fn ensure_font(&mut self) {
        let Some((_, handle)) = self.pending.take() else {
            return;
        };
        let loaded = match handle.join() {
            Ok(loaded) => loaded,
            Err(_) => Err("font load panicked".to_string()),
        };
        match loaded {
            Ok(font) => self.font = Some(font),
            // Nothing to fall back to before the first frame: same contract as
            // the old load, which ran before the Wayland connection.
            Err(e) if self.font.is_none() => {
                eprintln!("wlmenu: {e}");
                exit(1);
            }
            // A failed re-derivation keeps the previous font, as it always has.
            Err(_) => {}
        }
    }

    /// Adopt the output's HiDPI density: re-derive the font at `scale`× the
    /// logical size, so the buffer is rendered at native density (crisp text)
    /// instead of being upscaled by the compositor.
    fn apply_scale(&mut self, scale: i32) {
        let scale = scale.max(1) as u32;
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        self.derive_font();
    }

    /// Adopt the output's size: the panel width and, unless `-W`/`-f` pinned them,
    /// the text size follow the screen.
    fn apply_output_size(&mut self, output_w: u32) {
        if self.width_explicit.is_none() {
            self.width = auto_width(output_w);
        }
        let px = font::spec_size(self.font_spec.as_deref(), auto_font(self.width));
        if px != self.font_px {
            self.font_px = px;
            self.derive_font();
        }
    }

    /// Load the font for the current density and body size, unless a load with
    /// exactly that size is already in flight or in hand.
    fn derive_font(&mut self) {
        let size = self.font_px * self.scale as f32;
        let loaded = self
            .font
            .as_ref()
            .is_some_and(|f| (f.size - size).abs() < 0.01);
        if loaded || matches!(&self.pending, Some((pending, _)) if *pending == size) {
            return;
        }
        // Dropping the old handle detaches that thread rather than waiting for it.
        self.pending = Some((size, font::spawn_load(self.font_spec.clone(), size)));
        self.request_draw();
    }

    /// Caret blink: toggle every `BLINK_PERIOD` while idle. Typing calls
    /// `wake_caret` so the caret stays solid while the user is working.
    fn tick(&mut self) {
        let now = Instant::now();
        // Keep a transition moving; the loop's 16 ms timeout is our frame clock
        // (the Wayland frame callback only paces real frames). The row fade-in and
        // the scrollbar fade need the same clock.
        if self.anim.as_ref().is_some_and(|a| a.at(now).2)
            || self.content_fading(now)
            || self.scroll_fading(now)
        {
            self.request_draw();
        }
        if now < self.next_blink {
            return;
        }
        self.blink = !self.blink;
        self.next_blink = now + BLINK_PERIOD;
        self.request_draw();
    }

    fn wake_caret(&mut self) {
        self.blink = true;
        self.next_blink = Instant::now() + BLINK_PERIOD;
    }

    fn on_key(&mut self, keysym: Keysym, utf8: Option<String>) {
        if self.closing {
            return;
        }
        self.sync_items();
        self.wake_caret();
        let visible = self.visible();
        self.menu
            .on_key(keysym, utf8, self.mods, &self.items, self.ci, visible);
        // Ctrl-Return multi-select: emit each pick while the menu keeps running.
        let picked = std::mem::take(&mut self.menu.picked);
        for v in picked {
            if self.run {
                self.launches.push(v);
            } else {
                println!("{v}");
            }
        }
        self.request_draw();
    }

    /// Spawn queued `--run` picks. Called outside the dispatch: the token below
    /// dispatches events, which must not happen inside a handler.
    fn flush_launches(&mut self) {
        for cmd in std::mem::take(&mut self.launches) {
            self.launch(&cmd);
        }
    }

    /// Spawn a `--run` selection via sh (selections are shell command lines).
    fn launch(&mut self, cmd: &str) {
        let mut child = std::process::Command::new("sh");
        child.arg("-c").arg(cmd).stdin(std::process::Stdio::null());
        if let Some(token) = self.activation_token() {
            child.env("XDG_ACTIVATION_TOKEN", token);
        }
        let _ = child.spawn();
    }

    /// xdg-activation token for the window about to be launched, the way
    /// `wmenu-run` gets one. The request goes on a private queue — the loop's
    /// queue belongs to `WaylandSource`, and dispatching it from here would
    /// spin — and the sync sent after the commit is answered after the token.
    /// Surface only: sctk's `KeyEvent` carries no serial to attach.
    fn activation_token(&mut self) -> Option<String> {
        let surface = self.layer.as_ref()?.wl_surface().clone();
        let mut queue = self.conn.new_event_queue::<App>();
        self.activation.as_ref()?.request_token(
            &queue.handle(),
            RequestData {
                app_id: None,
                seat_and_serial: None,
                surface: Some(surface),
                udata: (),
            },
        );
        queue.roundtrip(self).ok()?;
        self.token.take()
    }

    fn request_draw(&mut self) {
        if self.frame_pending {
            self.dirty = true;
        } else {
            self.draw();
        }
    }

    fn draw(&mut self) {
        if self.frame_pending {
            return;
        }
        let Some(layer) = self.layer.clone() else {
            return;
        };
        // The only join point for the font load, so the read/parse overlaps the
        // handshake and the configure round trip that got us here.
        self.ensure_font();
        // Field borrow, not a method: `draw` also holds `&mut self.pool`.
        let font = self.font.as_ref().expect("font joined before use");

        // Spotlight: no list until the query has content; collapse back to the
        // single input bar when the query is cleared. A `-p` prompt is a question
        // with ready-made answers (the dmenu confirm idiom), so its list is shown
        // from the start instead.
        let visible = if list_hidden(&self.menu.query, &self.prompt) {
            0
        } else {
            self.visible()
        };
        let total = self.menu.matches.len();

        // A new query means rows arrive. The query is compared here rather than in
        // the key handler so every path that edits it (typing, backspace, Tab)
        // restarts the fade without each having to remember to.
        let now = Instant::now();
        if self.last_query != self.menu.query {
            // The baseline is what was on screen a frame ago, so a fast typist
            // restarts the ramp for genuinely new rows only, not for the ones the
            // previous keystroke just settled.
            self.baseline.clone_from(&self.prev_visible);
            self.last_query.clone_from(&self.menu.query);
            self.content_start = Some(now);
        }
        let fresh = self.content_start.map(|s| now.saturating_duration_since(s));

        // Keep the selection in view with one row of context on each side
        // (scrolloff), so the band never sits flush against a panel edge while
        // more rows exist beyond it.
        let mut rows: Vec<render::Row> = Vec::with_capacity(visible);
        let mut seen: Vec<usize> = Vec::with_capacity(visible);
        let mut any_fresh = false;
        if visible > 0 {
            let top = viewport_top(self.menu.sel, self.top, visible, total);
            if top != self.top {
                self.top = top;
                // The thumb is an overlay: it appears when the list moves and
                // fades once it stops (SPEC-UI-013).
                self.scroll_at = Some(now);
            }
            for (i, &mi) in self.menu.matches[top..(top + visible).min(total)]
                .iter()
                .enumerate()
            {
                let it = &self.items[mi];
                let fade = row_fade(&self.baseline, mi, fresh);
                any_fresh |= fade != 255;
                rows.push(render::Row {
                    text: &it.text,
                    selected: top + i == self.menu.sel,
                    fade,
                });
                seen.push(mi);
            }
        }
        // Nothing arrived, so nothing needs the frame clock: a keystroke that
        // changes no visible row costs one frame instead of eight.
        if !any_fresh {
            self.content_start = None;
        }
        self.prev_visible = seen;

        let scale = self.scale;
        let w = self.width;
        let mo = render::SHADOW * scale;
        // `row_h` already includes the density, so heights here are buffer
        // pixels; the layer surface itself is sized in logical pixels.
        let row_h = font.row_h;
        // Bar, `SEPARATION` px of air, then the list — the gap only exists while
        // the list does, so the collapsed bar stays one row tall.
        let target = row_h * (visible as u32 + 1)
            + if visible > 0 {
                render::SEPARATION * scale
            } else {
                0
            };
        // Smooth every change: the bar appearing, the list expanding under a
        // keystroke, the collapse back to the bar, and the fade-out on the way
        // out. Each new destination restarts the ease from what is on screen now,
        // and keeps the opacity it is at, so the three never fight each other.
        // Closing freezes the height: the cards leave by fading, not by shrinking,
        // so nothing moves under the pointer on the way out (and the surface stops
        // resizing for the last 100 ms of the process).
        let target = if self.closing { self.shown_h } else { target };
        if self.target_h != Some(target) {
            let from_a = self.anim.as_ref().map_or(1.0, |a| a.at(now).1);
            self.anim = Some(Anim {
                from_h: self.shown_h,
                to_h: target,
                from_a,
                to_a: if self.closing { 0.0 } else { 1.0 },
                dur: ANIM,
                start: now,
            });
            self.target_h = Some(target);
        }
        let (h, alpha, running) = self
            .anim
            .as_ref()
            .map_or((target, 1.0, false), |a| a.at(now));
        if !running {
            self.anim = None;
        }
        self.shown_h = h;
        // The surface is sized for the whole transition — the further of its two
        // ends — so the cards animate *inside* it and the compositor never has to
        // reflow the scene per frame.
        let surface_h = match &self.anim {
            Some(a) => a.from_h.max(a.to_h),
            None => target,
        };
        // Where the cards sit inside that surface, and so where clicks land: the
        // shadow margin hangs into the screen edge, so the surface margin is the
        // panel's visible gap minus the margin the shadow already occupies. The
        // panel keeps its old position at the top, and floats there in bottom mode.
        let shadow = render::SHADOW as i32;
        let margin = if self.bottom {
            (0, 0, 0, 0)
        } else {
            // Upper-center, horizontally centered on the whole surface.
            let left = self
                .output_w
                .map(|ow| ((ow as i64 - (w + 2 * render::SHADOW) as i64) / 2).max(0) as i32)
                .unwrap_or(0);
            (top_margin(self.output_h) as i32 - shadow, 0, 0, left)
        };
        if self.sent.margin != Some(margin) {
            layer.set_margin(margin.0, margin.1, margin.2, margin.3);
            self.sent.margin = Some(margin);
        }
        let size = (
            w + 2 * render::SHADOW,
            surface_h.div_ceil(scale) + 2 * render::SHADOW,
        );
        if self.sent.size != Some(size) {
            layer.set_size(size.0, size.1);
            self.sent.size = Some(size);
        }
        if self.sent.scale != Some(scale) {
            let _ = layer.set_buffer_scale(scale);
            self.sent.scale = Some(scale);
        }

        let bw = (w + 2 * render::SHADOW) * scale;
        let bh = surface_h + 2 * render::SHADOW * scale;
        let (buffer, canvas) = match self.pool.create_buffer(
            bw as i32,
            bh as i32,
            (bw * 4) as i32,
            wl_shm::Format::Argb8888,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("wlmenu: buffer error: {e}");
                self.menu.done = Some(Done::Cancel);
                return;
            }
        };
        // Content inset follows the text (0.8 em), from the size actually loaded.
        let pad = pad_for(font.size / scale as f32);
        render::draw_panel(
            canvas,
            bw,
            bh,
            h,
            self.bottom,
            font,
            &self.prompt,
            &self.menu.query,
            self.password,
            &rows,
            pad,
            &self.colors,
            scale,
            self.blink,
            text_aa(scale),
        );

        // Scroll position indicator: an overlay thumb, only while the list
        // overflows the viewport, fading out after the last movement.
        let sep = render::SEPARATION * scale;
        let thumb_alpha = scroll_alpha(self.scroll_at.map(|s| now.saturating_duration_since(s)));
        if let Some((ty, th)) = render::scroll_thumb(
            row_h + sep,
            h.saturating_sub(row_h + sep),
            visible,
            total,
            self.top,
        ) {
            let tw = 3 * scale;
            let tx = bw - mo - (pad * scale + tw) / 2;
            let accent = self.colors.label_accent;
            render::rect(
                canvas,
                bw,
                bh,
                tx,
                ty + mo,
                tw,
                th,
                [
                    accent[0],
                    accent[1],
                    accent[2],
                    (accent[3] as u32 * thumb_alpha as u32 / 255) as u8,
                ],
            );
        }

        render::fade(canvas, alpha);

        // Clicks belong to the cards, not to the transparent rest of the surface
        // the transition needs; the region only moves when the cards do.
        let input = (h + 2 * render::SHADOW * scale).div_ceil(scale);
        let input = (if self.bottom { size.1 - input } else { 0 }, input);
        if self.sent.input != Some(input)
            && let Ok(region) = Region::new(&self.compositor)
        {
            region.add(0, input.0 as i32, size.0 as i32, input.1 as i32);
            layer
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
            self.sent.input = Some(input);
        }

        layer.wl_surface().damage_buffer(0, 0, bw as i32, bh as i32);
        let surface = layer.wl_surface().clone();
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        if buffer.attach_to(layer.wl_surface()).is_err() {
            self.menu.done = Some(Done::Cancel);
            return;
        }
        layer.commit();
        self.frame_pending = true;
        self.dirty = false;
    }

    /// Start the exit transition. Nothing to animate when there is nothing to
    /// show (stdin was empty), so those exits stay immediate.
    fn begin_close(&mut self) {
        if self.closing || self.no_items {
            return;
        }
        self.closing = true;
        // Started here rather than in the next draw: a frame may already be in
        // flight, and the exit must not be skipped because of it.
        let now = Instant::now();
        let from_a = self.anim.as_ref().map_or(1.0, |a| a.at(now).1);
        self.anim = Some(Anim {
            from_h: self.shown_h,
            to_h: self.shown_h,
            from_a,
            to_a: 0.0,
            dur: EXIT,
            start: now,
        });
        self.target_h = Some(self.shown_h);
        self.request_draw();
    }

    /// True once the exit transition has played out, or there was none to play.
    fn close_finished(&self) -> bool {
        self.closing && self.anim.as_ref().is_none_or(|a| !a.at(Instant::now()).2)
    }

    fn finish(&mut self) {
        if self.no_items {
            eprintln!("wlmenu: no items");
            exit(1);
        }
        match self.menu.done.take() {
            Some(Done::Select(value)) => {
                if self.run {
                    self.launch(&value);
                    exit(0);
                }
                println!("{value}");
                exit(0);
            }
            _ => exit(1),
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        scale: i32,
    ) {
        self.apply_scale(scale);
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.dirty {
            self.draw();
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        let Some(info) = self.output_state.info(output) else {
            return;
        };
        if let Some((w, h)) = info.logical_size {
            let (w, h) = (w.max(0) as u32, h.max(0) as u32);
            if self.output_w != Some(w) || self.output_h != Some(h) {
                self.output_w = Some(w); // center the panel on this output
                self.output_h = Some(h); // and keep the list inside it
                // Density first: the default size is derived at the new scale.
                self.apply_scale(info.scale_factor);
                self.apply_output_size(w);
                self.request_draw();
            }
        }
        self.apply_scale(info.scale_factor);
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.menu.done = Some(Done::Cancel);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        if self.first_configure {
            self.first_configure = false;
            // Appearance: fade in at the bar's height. Started here rather than at
            // construction, which can be a whole font load earlier than this frame.
            let h = self.shown_h;
            self.anim = Some(Anim {
                from_h: h,
                to_h: h,
                from_a: 0.0,
                to_a: 1.0,
                dur: ANIM,
                start: Instant::now(),
            });
            self.draw();
        } else {
            self.request_draw();
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            // Route repeats through the calloop loop (the sctk repeat timer);
            // with plain get_keyboard the repeat callback is never armed.
            self.keyboard = self
                .seat_state
                .get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    self.loop_handle.clone(),
                    Box::new(|app, _kbd, event| app.on_key(event.keysym, event.utf8)),
                )
                .ok();
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(kb) = self.keyboard.take()
        {
            kb.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.on_key(event.keysym, event.utf8);
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
        // Repeats are delivered via the get_keyboard_with_repeat callback, not here.
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.mods = modifiers;
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ActivationHandler for App {
    type RequestUdata = ();

    fn new_token(&mut self, token: String, _: &RequestData<()>) {
        self.token = Some(token);
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(App);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_flag_is_parsed() {
        assert_eq!(
            opts(&["-o", "HDMI-A-1"]).output.as_deref(),
            Some("HDMI-A-1")
        );
        assert_eq!(opts(&[]).output, None);
    }

    #[test]
    fn loop_timeout_sleeps_to_the_next_deadline() {
        let now = Instant::now();
        // Streaming or animating: keep the frame clock.
        assert_eq!(loop_timeout(true, now + BLINK_PERIOD, now), FRAME);
        // Idle: sleep right up to the next caret blink, not a fixed tick.
        assert_eq!(
            loop_timeout(false, now + Duration::from_millis(500), now),
            Duration::from_millis(500)
        );
        // A deadline that already passed must not sleep, or the blink stalls.
        assert_eq!(
            loop_timeout(false, now, now + Duration::from_millis(5)),
            Duration::ZERO
        );
    }

    #[test]
    fn subpixel_aa_is_only_used_at_scale_one() {
        assert_eq!(
            text_aa(1),
            render::Subpixel::Rgb,
            "1× displays get the sharper mode"
        );
        assert_eq!(
            text_aa(2),
            render::Subpixel::Gray,
            "HiDPI does not need it (and pays for it)"
        );
    }

    #[test]
    fn width_and_lines_bounds_are_validated() {
        assert!(valid_width(640) && valid_width(1) && valid_width(MAX_WIDTH));
        assert!(!valid_width(0), "zero width would make a degenerate buffer");
        assert!(!valid_width(MAX_WIDTH + 1));
        assert!(valid_lines(0) && valid_lines(MAX_LINES), "0 means auto");
        assert!(!valid_lines(MAX_LINES + 1));
    }

    /// The list stops where the panel would otherwise reach the screen edge: what
    /// fits under the bar between the panel's top edge and the shadow below it.
    #[test]
    fn list_fits_between_the_screen_margins() {
        let (row, shadow) = (30, render::SHADOW);
        // 1080p: the top edge is 280 down, and the bottom reserves the shadow only.
        assert_eq!(top_margin(Some(1080)), 280);
        assert_eq!(max_rows(1080, row, top_margin(Some(1080)), shadow, 1), 24);
        assert_eq!(max_rows(1440, row, top_margin(Some(1440)), shadow, 1), 33);
        assert_eq!(max_rows(2160, row, top_margin(Some(2160)), shadow, 1), 51);
        // HiDPI: the same logical output gives the same number of rows.
        assert_eq!(
            max_rows(1080, row * 2, top_margin(Some(1080)), shadow, 2),
            24
        );
        // Too short for one row, or an absurd height: no rows, no underflow.
        let top = top_margin(Some(1080));
        assert_eq!(
            max_rows(top + shadow + row + render::SEPARATION, row, top, shadow, 1),
            0
        );
        assert_eq!(max_rows(0, row, top, shadow, 1), 0);
        assert!(
            max_rows(1080, 0, top, shadow, 1) <= 1080,
            "a zero row height must not divide by zero"
        );
        // `-b` sits SHADOW from the bottom edge: symmetric around that gap.
        assert_eq!(max_rows(1080, row, shadow, shadow, 1), 32);
    }

    /// The top edge is optical, not a fixed offset: about a quarter of the way
    /// down a tall screen, and never above the floor that keeps the shadow on it.
    #[test]
    fn top_edge_is_optical_not_fixed() {
        assert_eq!(
            top_margin(None),
            TOP_MARGIN as u32,
            "before the output is known"
        );
        assert_eq!(top_margin(Some(1080)), 280);
        assert_eq!(top_margin(Some(2160)), 561);
        assert_eq!(
            top_margin(Some(100)),
            TOP_MARGIN as u32,
            "a screen shorter than the floor clamps to it"
        );
        assert_eq!(top_margin(Some(900)), 234, "26% of the logical height");
    }

    /// The panel's default size follows the output: width a fixed fraction of it,
    /// text a fixed fraction of the width, so the bar keeps its proportions from a
    /// 1280-wide laptop to a 4K desktop rather than growing into a page.
    #[test]
    fn default_size_follows_the_output() {
        // Width is 45% of the output, clamped at both ends.
        assert_eq!(auto_width(1280), 576);
        assert_eq!(auto_width(1440), 648);
        assert_eq!(auto_width(1920), 864);
        assert_eq!(auto_width(2560), 960, "clamped on a wide screen");
        assert_eq!(auto_width(1024), 480, "clamped on a small one");
        // Text is 1/32 of the panel width: 640 -> 20 px, as before.
        assert_eq!(auto_font(640), 20.0);
        assert_eq!(auto_font(864), 27.0);
        assert_eq!(auto_font(480), 15.0);
        assert_eq!(auto_font(4000), FONT_PX_MAX);
        assert_eq!(auto_font(100), FONT_PX_MIN);
        // The inset follows the text but never undercuts the corner radius.
        assert_eq!(pad_for(20.0), 16);
        assert_eq!(pad_for(27.0), 22);
        assert_eq!(pad_for(10.0), render::CORNER_RADIUS);
    }

    #[test]
    fn viewport_keeps_the_selection_in_view_with_context() {
        // The whole list fits: never scroll, even with the last row selected.
        assert_eq!(viewport_top(9, 0, 10, 10), 0);
        // Scrolling down leaves one row of context below the band.
        assert_eq!(viewport_top(9, 0, 10, 100), 1);
        assert_eq!(viewport_top(19, 1, 10, 100), 11);
        // Scrolling up leaves one row of context above.
        assert_eq!(viewport_top(10, 20, 10, 100), 9);
        // Moving inside the window keeps the offset (no jitter).
        assert_eq!(viewport_top(12, 5, 10, 100), 5);
        // Collapsed bar and short lists sit at the top.
        assert_eq!(viewport_top(0, 0, 0, 100), 0);
        assert_eq!(viewport_top(3, 0, 10, 4), 0);
        // A shrunken list clamps a stale offset instead of panicking the slice.
        assert_eq!(viewport_top(0, 50, 10, 3), 0);
    }

    fn sample_items() -> Vec<items::Item> {
        ["firefox", "alacritty", "libreoffice"]
            .iter()
            .map(|s| items::Item {
                lc: s.to_lowercase(),
                text: s.to_string(),
                value: s.to_string(),
                extra: String::new(),
            })
            .collect()
    }

    fn opts(args: &[&str]) -> Opts {
        parse_opts_from(args.iter().map(|s| s.to_string()))
    }

    fn mods(ctrl: bool, shift: bool) -> Modifiers {
        Modifiers {
            ctrl,
            shift,
            ..Default::default()
        }
    }

    /// Key press with default modifiers; `visible=3` mirrors the 3-item sample list.
    fn key(m: &mut MenuState, items: &[items::Item], ks: Keysym, u: Option<&str>) {
        m.on_key(
            ks,
            u.map(String::from),
            Modifiers::default(),
            items,
            false,
            3,
        );
    }

    /// xkeysym for any printable 'F' key; the `_` arm only uses the utf8 text.
    fn type_key(m: &mut MenuState, items: &[items::Item], c: char) {
        key(m, items, Keysym::from(0x66u32), Some(&c.to_string()));
    }

    #[test]
    fn color_flags_match_wmenu_fields() {
        // `-M`/`-m` are the prompt rows, `-S`/`-s` the selection (wmenu semantics).
        let o = opts(&[
            "-N", "112233", "-n", "445566", "-M", "778899", "-m", "aabbcc", "-S", "ddeeff", "-s",
            "010203",
        ]);
        assert_eq!(o.colors.bg_normal, [0x33, 0x22, 0x11, 0xff]);
        assert_eq!(o.colors.fg_normal, [0x66, 0x55, 0x44, 0xff]);
        assert_eq!(o.colors.bg_prompt, [0x99, 0x88, 0x77, 0xff]);
        assert_eq!(o.colors.fg_prompt, [0xcc, 0xbb, 0xaa, 0xff]);
        assert_eq!(o.colors.bg_sel, [0xff, 0xee, 0xdd, 0xff]);
        assert_eq!(o.colors.fg_sel, [0x03, 0x02, 0x01, 0xff]);
    }

    #[test]
    fn bottom_and_password_flags() {
        let o = opts(&["-b", "-P"]);
        assert!(o.bottom && o.password);
        let d = opts(&[]);
        assert!(!d.bottom && !d.password);
    }

    #[test]
    fn escape_cancels() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        assert!(m.done.is_none());
        key(&mut m, &items, Keysym::Escape, None);
        assert_eq!(m.done, Some(Done::Cancel));
    }

    #[test]
    fn enter_selects_highlighted_value() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'f'); // narrows to firefox + libreoffice, firefox first
        key(&mut m, &items, Keysym::Return, None);
        assert_eq!(m.done, Some(Done::Select("firefox".into())));
    }

    #[test]
    fn enter_with_empty_query_cancels() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        key(&mut m, &items, Keysym::Return, None);
        assert_eq!(m.done, Some(Done::Cancel));
    }

    /// `-p` makes the menu a question with ready-made answers: the list is on
    /// screen from the start and Enter takes the highlighted one, so
    /// `printf "no\nyes" | wlmenu -p "System Suspend?"` works without typing.
    #[test]
    fn prompt_menus_show_and_take_their_answers_at_launch() {
        assert!(list_hidden("", ""), "launcher stays collapsed");
        assert!(!list_hidden("", "System Suspend?"), "prompt shows the list");
        assert!(!list_hidden("y", ""), "typing shows the list");

        let items = sample_items();
        let mut m = MenuState::new(items.len()).with_prompt(true);
        key(&mut m, &items, Keysym::Return, None);
        assert_eq!(m.done, Some(Done::Select("firefox".into())));
    }

    /// The same keystroke on a launcher (no prompt) still cancels: its list is
    /// hidden, so there is nothing visible to pick blind.
    #[test]
    fn launcher_enter_on_empty_query_still_cancels() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        assert!(!m.prompt);
        key(&mut m, &items, Keysym::Return, None);
        assert_eq!(m.done, Some(Done::Cancel));
    }

    #[test]
    fn typing_filters_and_enter_picks_only_match() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'l');
        type_key(&mut m, &items, 'i');
        assert_eq!(m.matches.len(), 1);
        key(&mut m, &items, Keysym::Return, None);
        assert_eq!(m.done, Some(Done::Select("libreoffice".into())));
    }

    #[test]
    fn enter_with_no_match_echoes_query() {
        // dmenu/wmenu contract: with nothing to pick, Enter prints the typed
        // line — that's how `echo "" | wlmenu -p …` prompt scripts work.
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'z');
        assert!(m.matches.is_empty());
        key(&mut m, &items, Keysym::Return, None);
        assert_eq!(m.done, Some(Done::Select("z".into())));
    }

    #[test]
    fn unbound_ctrl_chords_do_not_garbble_query() {
        // Wayland hands ctrl+a .. ctrl+z to us as U+0001..U+001A control chars;
        // unbound chords (C-a, C-v) must still never become query text.
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        m.on_key(
            Keysym::from(0x61u32),
            Some("\u{1}".into()),
            mods(true, false),
            &items,
            false,
            3,
        );
        m.on_key(
            Keysym::from(0x76u32),
            Some("\u{16}".into()),
            mods(true, false),
            &items,
            false,
            3,
        );
        assert_eq!(m.query, "");
        assert_eq!(m.matches.len(), 3);
    }

    #[test]
    fn backspace_restores_matches() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'a'); // only "alacritty"
        assert_eq!(m.matches.len(), 1);
        key(&mut m, &items, Keysym::BackSpace, None);
        assert_eq!(m.matches.len(), 3);
    }

    #[test]
    fn arrows_move_selection_within_bounds() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'i'); // matches all three, original order (no prefix hit)
        key(&mut m, &items, Keysym::Up, None); // clamp at top
        assert_eq!(m.sel, 0);
        key(&mut m, &items, Keysym::Down, None);
        assert_eq!(m.sel, 1);
        key(&mut m, &items, Keysym::Down, None);
        key(&mut m, &items, Keysym::Down, None); // clamp at bottom
        assert_eq!(m.sel, 2);
        key(&mut m, &items, Keysym::Return, None);
        assert_eq!(m.done, Some(Done::Select("libreoffice".into())));
    }

    #[test]
    fn ctrl_n_p_move_selection() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'i'); // all three matches
        m.on_key(
            Keysym::from(0x70u32),
            Some("\u{10}".into()),
            mods(true, false),
            &items,
            false,
            3,
        ); // C-p clamp at top
        assert_eq!(m.sel, 0);
        m.on_key(
            Keysym::from(0x6eu32),
            Some("\u{e}".into()),
            mods(true, false),
            &items,
            false,
            3,
        ); // C-n
        assert_eq!(m.sel, 1);
        m.on_key(
            Keysym::from(0x70u32),
            Some("\u{10}".into()),
            mods(true, false),
            &items,
            false,
            3,
        ); // C-p
        assert_eq!(m.sel, 0);
        m.on_key(
            Keysym::from(0x6eu32),
            Some("\u{e}".into()),
            mods(true, false),
            &items,
            false,
            3,
        );
        m.on_key(
            Keysym::from(0x6eu32),
            Some("\u{e}".into()),
            mods(true, false),
            &items,
            false,
            3,
        ); // C-n clamp at bottom
        assert_eq!(m.sel, 2);
    }

    #[test]
    fn page_keys_jump_by_visible_rows() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        m.on_key(
            Keysym::Page_Down,
            None,
            mods(false, false),
            &items,
            false,
            2,
        );
        assert_eq!(m.sel, 2); // clamped to last (3 items, page 2)
        m.on_key(Keysym::Page_Up, None, mods(false, false), &items, false, 2);
        assert_eq!(m.sel, 0);
        m.on_key(Keysym::Page_Up, None, mods(false, false), &items, false, 2); // clamp at top
        assert_eq!(m.sel, 0);
    }

    #[test]
    fn home_end_jump_selection() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        key(&mut m, &items, Keysym::Down, None);
        key(&mut m, &items, Keysym::Home, None);
        assert_eq!(m.sel, 0);
        key(&mut m, &items, Keysym::End, None);
        assert_eq!(m.sel, 2);
        key(&mut m, &items, Keysym::End, None); // clamp
        assert_eq!(m.sel, 2);
    }

    #[test]
    fn ctrl_c_cancels() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'a');
        m.on_key(
            Keysym::from(0x63u32),
            Some("\u{3}".into()),
            mods(true, false),
            &items,
            false,
            3,
        );
        assert_eq!(m.done, Some(Done::Cancel));
    }

    #[test]
    fn ctrl_u_clears_query() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'f');
        m.on_key(
            Keysym::from(0x75u32),
            Some("\u{15}".into()),
            mods(true, false),
            &items,
            false,
            3,
        );
        assert_eq!(m.query, "");
        assert_eq!(m.matches.len(), 3);
    }

    #[test]
    fn ctrl_w_kills_last_word_readline_style() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        for c in ['a', 'b', ' ', 'c', 'd'] {
            type_key(&mut m, &items, c); // "ab cd"
        }
        m.on_key(
            Keysym::from(0x77u32),
            Some("\u{17}".into()),
            mods(true, false),
            &items,
            false,
            3,
        );
        assert_eq!(m.query, "ab");
    }

    #[test]
    fn ctrl_w_on_trailing_space_kills_word_and_separator() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        for c in ['a', 'b', ' ', 'c', 'd', ' '] {
            type_key(&mut m, &items, c); // "ab cd "
        }
        m.on_key(
            Keysym::from(0x77u32),
            Some("\u{17}".into()),
            mods(true, false),
            &items,
            false,
            3,
        );
        assert_eq!(m.query, "ab");
    }

    #[test]
    fn ctrl_h_backspaces() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'l');
        m.on_key(
            Keysym::from(0x68u32),
            Some("\u{8}".into()),
            mods(true, false),
            &items,
            false,
            3,
        );
        assert_eq!(m.query, "");
        assert_eq!(m.matches.len(), 3);
    }

    #[test]
    fn ctrl_j_enters_like_return() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'l'); // libreoffice
        m.on_key(
            Keysym::from(0x6au32),
            Some("\u{a}".into()),
            mods(true, false),
            &items,
            false,
            3,
        );
        assert_eq!(m.done, Some(Done::Select("libreoffice".into())));
    }

    #[test]
    fn shift_return_submits_typed_text() {
        // Even with matches available, Shift-Return submits exactly the typed line.
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'i'); // matches all three, sel=0 (firefox)
        m.on_key(Keysym::Return, None, mods(false, true), &items, false, 3);
        assert_eq!(m.done, Some(Done::Select("i".into())));
    }

    #[test]
    fn ctrl_return_picks_and_continues() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'i'); // all three match, sel=0 -> firefox
        let ctrl = mods(true, false);
        m.on_key(Keysym::Return, None, ctrl, &items, false, 3);
        assert_eq!(m.picked, vec!["firefox".to_string()]);
        assert!(m.done.is_none());
        assert_eq!(m.sel, 1); // advanced so repeated Ctrl-Return walks the list
        m.on_key(Keysym::Return, None, ctrl, &items, false, 3);
        assert_eq!(m.sel, 2);
        m.on_key(Keysym::Return, None, ctrl, &items, false, 3);
        assert_eq!(m.sel, 2); // clamps at last
        assert_eq!(
            m.picked,
            vec![
                "firefox".to_string(),
                "alacritty".to_string(),
                "libreoffice".to_string()
            ]
        );
    }

    #[test]
    fn tab_completes_highlighted_item_into_query() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'o'); // matches: firefox, libreoffice
        key(&mut m, &items, Keysym::Down, None); // highlight libreoffice (2nd match)
        key(&mut m, &items, Keysym::Tab, None);
        assert_eq!(m.query, "libreoffice");
        assert_eq!(m.matches.len(), 1); // refiltered to the completed item
        assert_eq!(m.sel, 0);
        key(&mut m, &items, Keysym::Return, None);
        assert_eq!(m.done, Some(Done::Select("libreoffice".into())));
    }

    #[test]
    fn tab_with_hidden_list_fills_first_item() {
        // Spotlight collapses the list on empty query, but the internal
        // selection still exists — Tab fills it like any other state.
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        key(&mut m, &items, Keysym::Tab, None);
        assert_eq!(m.query, "firefox");
        key(&mut m, &items, Keysym::Return, None);
        assert_eq!(m.done, Some(Done::Select("firefox".into())));
    }

    #[test]
    fn tab_with_no_match_is_noop() {
        let items = sample_items();
        let mut m = MenuState::new(items.len());
        type_key(&mut m, &items, 'z'); // no matches
        key(&mut m, &items, Keysym::Tab, None);
        assert_eq!(m.query, "z");
        assert!(m.matches.is_empty());
    }

    /// A row that has just arrived ramps in; a row that was already on screen
    /// stays whole, so a keystroke that merely reorders rows does not blink them.
    #[test]
    fn fresh_rows_fade_in_and_survivors_stay_whole() {
        assert_eq!(
            fresh_alpha(None),
            255,
            "no refresh running: everything whole"
        );
        assert_eq!(
            fresh_alpha(Some(Duration::ZERO)),
            0,
            "a new row arrives empty"
        );
        let mid = fresh_alpha(Some(CONTENT / 2));
        assert!(mid > 0 && mid < 255, "mid-flight: {mid}");
        assert_eq!(fresh_alpha(Some(CONTENT)), 255, "settles exactly opaque");
        assert_eq!(fresh_alpha(Some(CONTENT * 2)), 255);

        // Identity, not position: index 7 was on screen, so it is whole at once.
        assert_eq!(row_fade(&[7, 3], 7, Some(Duration::ZERO)), 255);
        assert_eq!(row_fade(&[7, 3], 9, Some(Duration::ZERO)), 0);
        assert_eq!(row_fade(&[], 7, None), 255);
    }

    /// The overlay thumb is full at the moment the list moves and gone
    /// `SCROLL_FADE` later, so it does not sit on the list while you read it.
    #[test]
    fn scrollbar_fades_out_after_the_list_stops_moving() {
        assert_eq!(scroll_alpha(None), 0, "never moved: no thumb");
        assert_eq!(scroll_alpha(Some(Duration::ZERO)), 255);
        let mid = scroll_alpha(Some(SCROLL_FADE / 2));
        assert!(mid > 0 && mid < 255, "fading: {mid}");
        assert_eq!(scroll_alpha(Some(SCROLL_FADE)), 0);
        assert_eq!(scroll_alpha(Some(SCROLL_FADE * 2)), 0);
    }
}
