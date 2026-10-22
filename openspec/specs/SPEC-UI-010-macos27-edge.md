# SPEC-UI-010: A macOS 27 Edge

## Goal

plush's rim (SPEC-UI-009) draws its boundary with a bright white line over the
upper half of the card. macOS 27 reads a window edge differently: the contour is
carried by a *dark* edge plus a specular highlight on its upper reaches, with the
drop shadow doing the layering, and no bright stroke around the panel. wlmenu
takes that reading, so a card floats over the wallpaper as glass instead of
sitting inside a drawn outline.

## Reference (the reading, not invented numbers)

macOS 27's window light is described as edge shading plus specular refraction: a
darker edge and a brighter mirror highlight on Liquid Glass, a shadow that
separates the focused window from the ones behind it, restrained radii, and no
heavy solid border. No blur radius, direction or alpha is published, so the
numbers below are our translation of that reading, not a quote.

## Scope

- `render.rs`, the rim ink only: the two alphas, the lit curve, the dark curve,
  and the inner band's alpha.
- The dark edge now runs the whole height (it is the contour), and the lit ink is
  a dimmer specular that has died by the lower half:

  | | SPEC-UI-009 (plush) | now |
  |---|---|---|
  | lit alpha | 90 | 60 |
  | dark alpha | 90 | 110 |
  | dark at the top | 0.08 | 0.30 |
  | lit at t=0.45 | 0.52 | 0.35 |
  | lit at t=0.72 | 0.09 | 0.06 |
  | inner band alpha | 130 | 100 |

## Code Style

- Constants only; `body()` and `profile()` unchanged. No new flag, no new
  dependency.

## Test Strategy

- `border_ring_is_one_pixel_wide` keeps its claims: the top rim is still lighter
  than the body, the bottom rim still darker.
- `a_dark_band_sits_one_pixel_inside_the_rim` keeps both claims unchanged: at a
  `100` band the lower wall is still clearly darker than the body.
- Rendered frame checked by eye over a gradient backdrop, 1× and 8× on the edge.
- `cargo test`, `cargo clippy`, `cargo fmt --check`.

## Boundaries

- No change to the shadow layers or margin (SPEC-UI-009's `SHADOW = 26` and its
  two layers stay: they already do the layering this reading asks for), nor to
  radius, padding, layout, selection colour, matching or flags.
- Chrome parity with plush is **intentionally broken** here. If plush follows
  macOS 27, re-sync the numbers; until then the two projects differ on the rim.

## Success Criteria

- The card edge reads as dark glass with a highlight, not as a white line.
- The panel is still separated from a mid-tone wallpaper without the rim doing
  the work alone.
- Tests and Clippy green.
