# SPEC-UI-013: Motion the Eye Can Follow, Anchored Where It Already Looks

## Goal

Four gaps against the Spotlight reference (`~/todoc/spotlight.md` §1, §2, §5). All
four serve one sentence from that document: *the eye must not have to re-find its
place while you type.*

1. **Exit is a copy of the entrance.** All three transitions share 180 ms
   ease-in-out, and closing also collapses the height. The reference is
   asymmetric: in 120–160 ms, out ≈100 ms, opacity only.
2. **The result list swaps content instantly** under a keystroke. The reference
   cross-fades it over 100–140 ms.
3. **The panel's top edge is a fixed 32 px.** The reference puts the bar's top
   edge at roughly a quarter to a third of the screen height — optical centre,
   not mathematical centre.
4. **The scrollbar never goes away.** The reference shows the overlay thumb while
   the list moves and fades it out afterwards.

Out of scope, already decided: selection-colour semantics (grey selection + blue
match highlight), an entrance scale, the bar's icon slot / top-hit / slow-query
spinner, and reordering the cards in `-b`. The first would need match spans
plumbed from `items.rs`, the second costs 8–12 ms per frame at scale 2 for a 3 %
grow, the third needs an image dependency, the fourth changes `-b`'s established
shape.

## Design

### 1. Asymmetric in and out

- `Anim` gains `dur: Duration`. Entrance and resize keep 180 ms ease-in-out
  cubic; fade-out uses `EXIT = 100 ms` with ease-in (it accelerates away, it does
  not settle).
- The curve is derived, not stored: `at()` picks ease-in when `to_a < from_a`.
  Opacity only ever ramps down on the exit, so that is the exit.
- **Closing freezes the height** (`to_h = from_h`). The cards keep their geometry
  and only opacity moves, so the exit also stops resizing the surface — the
  per-frame relayout that SPEC-UI-012 removed from resizing stays removed.

### 2. New rows fade in, survivors do not blink

A true cross-fade needs the previous frame (a second buffer plus a per-pixel
lerp); at scale 2 with 64 rows that is 10 MB and roughly 3–4 ms. Not affordable,
and not necessary: rows are redrawn every frame anyway. So the fade goes into the
text alpha and the identity of a row decides it.

- `App` keeps `prev_visible: Vec<usize>` (item indices drawn last frame),
  `last_query: String`, and `content_start: Option<Instant>`.
- `draw` compares `menu.query` with `last_query`. On change it restarts
  `content_start` and **keeps** `prev_visible`, so a row that was already on
  screen is never treated as new. No key handler changes.
- A visible row whose item index is absent from `prev_visible` gets
  `Row.fade = ease_out(t) * 255` over `CONTENT = 120 ms`; survivors get 255.
  `render` multiplies `fade` into the text colour's alpha byte, which `blit_plane`
  already honours (`a = cov * color[3] / 255`).
- Cost: zero extra drawing, one `u8` per row, one 64-entry `Vec`.

### 3. Optical top edge

- `top_margin(output_h: u32) -> u32 = (output_h * TOP_PCT / 100).max(TOP_MARGIN)`,
  `TOP_PCT = 26` (inside the reference's quarter-to-third band), `TOP_MARGIN = 32`
  as the floor for a short screen and for the frame before `output_h` is known.
- It feeds both the layer margin (`top_margin - SHADOW`, must stay ≥ 0 via the
  32 px floor) and the row budget.
- `max_rows` takes separate top and bottom gaps. Today it reserves the same gap
  twice; with a 280 px top on 1080p that would leave 15 rows. Reserving only the
  shadow at the bottom gives 23. `-b` passes `SHADOW` for both and is unchanged.

### 4. The scrollbar fades

- `scroll_at: Option<Instant>`, set when the list scrolls (selection move,
  filter change, overflow appearing).
- Thumb alpha 255 → 0 over `SCROLL_FADE = 400 ms`; drawn through the existing
  `rect`, whose `blend_px` already takes the alpha byte.
- The frame clock counts an unfinished fade as busy; otherwise the 500 ms blink
  deadline would make it step.

## Code Style

Pure functions plus constants in the existing single-file modules. One new field
on `Anim`, three on `App`, one on `Row`. No dependency, no second buffer, no new
state machine.

## Test Strategy

- `anim`: the exit reaches 0 opacity in `EXIT`; the height is identical on the
  first and last frame of the exit; the entrance keeps every SPEC-UI-011 claim
  (180 ms, ease-in, exact target, monotonic collapse).
- `main`: `top_margin` — unknown height → 32, 1080 → 280, 2160 → 561, never below
  `SHADOW`. `max_rows` with two gaps — 1080p → 24 rows (32 today), `-b` unchanged.
- `main`: row fade — a fresh index is below 255 at the start of `CONTENT` and 255
  after it; a survivor is 255 from the first frame; a query change restarts the
  timer without emptying the survivor set.
- `main`: scrollbar alpha 255 at the scroll and 0 after `SCROLL_FADE`.
- `render`: a row with `fade = 0` leaves the pixels under it as the body painted
  them.
- Manual: headless sway at 1× and 2× — a typing burst, Enter, Escape.

## Boundaries

- Same printed value and the same `--run` behaviour; the exit still plays before
  `finish`. Empty stdin still exits 1 immediately.
- No change to flags, width, corner radius, rim, shadow, palette, selection
  colour, matching, keyboard handling, or the stdin/stdout contract.
- `-b` keeps today's geometry: bottom-anchored surface, cards in the same order.

## Success Criteria

- A keystroke's result change reads as rows arriving rather than as a swap, and
  rows that were already on screen never blink.
- Close is visibly quicker than open and does not shrink.
- Nothing sits a fixed 32 px from the top of a large screen.
- `cargo test` green; `cargo clippy` no new warnings.

## Risks

- The scrollbar disappearing loses the standing "the list continues" hint. The
  reference trades it for quiet; the thumbnail returns on the next keypress.
- The top margin jumps once on the first `surface_enter` (as the width already
  does, round-7's known jump). Both are one-frame events before the user types.
- The row-budget drop on short screens: a 768 px screen now fits 16 rows instead
  of 22, a 1080p one 24 instead of 32. `-l` still caps lower, scrolling still
  reaches the rest.
