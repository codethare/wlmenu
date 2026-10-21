# SPEC-UI-008: Chrome Shared With plush

## Goal

wlmenu and plush are two halves of one desktop, so they must not look like two
design systems: the hairline, the shadow and the text inks come from the same
numbers in both.

## Reference (read from plush, not invented)

- `background.color = #1e1e1ed4`, `border.color = #ffffff22` (alpha 34),
  `BORDER_DARK_ALPHA = 60`, `RADIUS = 12`, `PAD = 16` — wlmenu already matched
  all of these.
- Hairline profiles: lit `[(0,1),(0.28,0.62),(0.55,0.21),(0.85,0),(1,0)]`,
  dark `[(0,0),(0.5,0.1),(0.82,0.63),(1,1)]`.
- Shadow layers `(blur, alpha, offset)` = tight `(6,155,1)`, wide `(13,115,4)`,
  margin `SHADOW = 20`.
- Inks: primary `#ffffff f5`, secondary `#ebebf5 9b`.

## Scope

- Hairline: replace the flat dark ring and the `t²` lit falloff with plush's two
  piecewise-linear profiles, cross-fading down each card's own height.
- Shadow: adopt plush's blur/alphas/offsets and shrink the margin to 20.
- Inks: `fg_normal` becomes the secondary ink, `fg_prompt` the primary ink.
  `-n`/`-m`/`-s` still override everything.

## Code Style

- One `profile(&[(f32, f32)], t)` interpolator next to the tables; the body pass
  reads both tables per row.
- No new dependency, no new flag, no change to how geometry scales.

## Test Strategy

- Existing pixel tests keep their meaning: lit top rim, dark bottom rim, corner
  shadow, no shadow under the body, gap holds shadow.
- The caret is drawn in translucent ink now, so its tests compare against the ink
  composited over the bar instead of the palette entry.
- `cargo test`, `cargo clippy`, and a wallpaper composite rendered by eye.

## Boundaries

- No change to layout, radii, padding, matching, keyboard flow, flags, or the
  selection colour (plush has no selection to copy, and the current pair is the
  one that clears WCAG AA).
- No new config surface: wlmenu stays flag-driven.

## Success Criteria

- A wlmenu card and a plush panel side by side read as the same chrome: same
  edge fade, same shadow tightness, same grey inks.
- Tests and Clippy green; the collapsed bar keeps its frame cost.
