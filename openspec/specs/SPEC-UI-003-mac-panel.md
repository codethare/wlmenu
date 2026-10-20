# SPEC-UI-003: macOS Panel Surfaces

## Goal

Redesign the panel surfaces after macOS window chrome: a translucent glass body,
a single hairline edge, and a drop shadow that grows out of that edge instead of
being cut off by it. The panel background becomes translucent.

## Scope

- Reserve a shadow margin (24 logical px) around the panel; the shm buffer and
  the layer surface grow by it, the panel keeps its size and screen position.
- Panel body is premultiplied-alpha: `bg_normal`/`bg_prompt`/`bg_sel` carry an
  alpha channel, text and edge blending composite over it correctly.
- Two-layer drop shadow outside the panel outline: tight contact
  (blur 7, peak alpha 170, offset 2) plus wide ambient (blur 17, peak alpha 130,
  offset 6), logical px, black. Each layer starts at half its peak and eases out
  with a cosine falloff (a linear ramp leaves a visible cusp). Both layers are
  composed per pixel and written once.
- One hairline on the panel's outermost pixel: lit along the top
  (`BORDER_LIGHT_ALPHA` falling off over the top third), dark underneath
  (`BORDER_DARK_ALPHA`). It covers the seam where the translucent body meets
  the shadow, so the two read as one continuous edge.
- Panel edge gets 1 px coverage antialiasing from the fractional corner inset
  (the rounded outline stops stair-stepping).
- The input/list divider is inset by the content padding so it never touches the
  border ring.
- New defaults: `bg_normal #1e1e1ed4`, `bg_prompt #2c2c2cdc`, `bg_sel #0b6285ee`.

## Code Style

- Constants and pure helpers in `render.rs`; no new module, no new dependency.
- `rounded_span` keeps its signature; `rounded_inset` becomes the fractional
  primitive it rounds.
- All geometry stays logical and multiplies by `scale`, as today.

## Test Strategy

- Headless pixel tests: shadow falls off outside the outline and never appears
  inside the panel; edge pixels are partially covered (0 < alpha < body alpha);
  the border ring is one pixel and differs from the body; body pixels are
  premultiplied; text ink raises alpha toward opaque.
- Existing geometry tests translate through the shadow margin instead of
  measuring buffer coordinates.
- `cargo test`, `cargo clippy`.

## Boundaries

- No blur / vibrancy: real background blur needs `ext-background-effect-v1` and
  a new dependency. Out of scope by decision.
- No squircle/continuous-curvature corners; the outline stays circular.
- No new flags, no change to wmenu flag semantics, keyboard flow, filtering,
  stdin/stdout, row height, or animation.
- No cairo/pango and no new dependency.

## Success Criteria

- The panel renders translucent over the wallpaper with an antialiased rounded
  edge, a 1 px hairline, and a soft shadow continuous with that hairline.
- `RRGGBBAA` colors composite correctly (premultiplied buffer).
- Panel size, position, and open/close animation are unchanged.
- Existing and new tests pass, with no new Clippy warnings.
