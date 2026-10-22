# SPEC-UI-009: plush's Glass Rim and Wider Shadow

## Goal

plush moved its chrome on: the rim keeps its strength down the top corner arcs
and then dies quickly, the dark underside is stronger, a dark band sits one
pixel inside the rim so the edge reads as glass with thickness, and the shadow
is softer but gets more room. wlmenu takes the same numbers, so a menu and a
plush panel still read as one product.

## Reference (read from plush @ nzwuxuvn, not invented)

- `border.color = #ffffff5a` (alpha 90), `BORDER_DARK_ALPHA = 90`
- `SHADOW = 26`; layers `(blur, alpha, offset)`: tight `(9,120,2)`, wide
  `(20,95,6)`
- lit `[(0,1),(0.16,0.9),(0.42,0.55),(0.78,0),(1,0)]`
- dark `[(0,0.08),(0.45,0.15),(0.72,0.4),(0.9,0.85),(1,1)]`
- inner band `[(0,0),(0.3,0.05),(0.6,0.3),(0.85,0.8),(1,1)]` at alpha 130,
  black, one stroke inside the rim

## Scope

- `render.rs`: the two profile tables, the two alphas, the shadow layers and
  margin, plus the band's table and alpha.
- `body()`: paint the band on the pixel inside the rim at both card edges, from
  the same `t` the rim uses.

## Code Style

- Reuse `profile()`; no new helper, no new dependency, no new flag.

## Test Strategy

- New test: the band is absent on the card's top row (`t = 0`, nothing is above
  the rim to shadow it) and clearly darker than the body near the bottom.
- `border_ring_is_one_pixel_wide` keeps its meaning, but its body probe moves
  from x=1 to x=2: x=1 carries the band now, so the body starts one pixel
  further in. That is the behavior this change introduces, not a repair.
- `cargo test`, `cargo clippy`, `cargo fmt --check`.

## Boundaries

- No change to layout, radius, padding, card gap, matching, keyboard flow,
  flags, or the selection colour.
- `SHADOW` grows, so everything derived from it grows with it: the buffer, the
  surface size and the pool. The panel's on-screen position does not move — the
  layer margin is `TOP_MARGIN - SHADOW`. Bottom mode (`-b`) floats one `SHADOW`
  higher than before, by construction.

## Success Criteria

- A wlmenu card next to a plush panel: same rim, same shadow, same weight.
- Tests and Clippy green.
