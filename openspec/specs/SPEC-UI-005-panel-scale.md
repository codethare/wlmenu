# SPEC-UI-005: Panel Sized To The Output

## Goal

The panel's default size follows the screen it opens on, at a ratio modelled on
Spotlight's footprint: the bar covers roughly half the width of the display and
its text scales with that width, so the menu reads the same on a 1440p laptop and
a 4K desktop. Explicit `-W` and `-f <size>` still win.

## Reference (verified, not invented)

- Spotlight's window is a **fixed size** on macOS (it cannot be resized by
  dragging; Big Sur narrowed it, Tahoe keeps it fixed). Apple publishes no
  screen-fraction, so the ratio below is a deliberate product choice taken from a
  common Mac default (1440x900 logical: ~680pt wide field), not a spec quote.
- Chosen ratios: width = **45 %** of the output width, clamped to 480..960
  logical px; default text size = width / **32** (3.1 % of width), clamped to
  14..30 px. At the current 640 px default this is exactly today's 20 px.

## Scope

- `-W` becomes optional: unset means "45 % of the output width, clamped". The
  default applies once `surface_enter` reports the output width; before that the
  panel commits at `DEFAULT_WIDTH`.
- Default font size derives from the effective width (`-f <size>` still overrides
  it), so row height, bar height and the list cap all follow.
- Content padding follows the text (`0.8 em`, mac's menu inset) instead of a fixed
  16 px; `PAD` stays as the value used for the first commit.
- The shm pool hint uses the widest auto width, not the old default.

## Code Style

- Two pure helpers in `main.rs`, asserted in unit tests:
  `auto_width(output_w: u32) -> u32`, `auto_font(width: u32) -> f32`.
- Reuse the existing font re-derivation path (`pending`/`ensure_font`), as
  `apply_scale` already does.
- No new module, dependency, or flag.

## Test Strategy

- `auto_width`: 1280/1440/1920/2560 widths hit the expected values and the clamps;
  `auto_font` is 20 at 640 and clamps at both ends; scale never enters the
  formula (logical px in, logical px out).
- Existing tests keep passing; `-W`/`-f` parsing is untouched.

## Boundaries

- No change to row height ratio (1.5x), gaps, shadow, colors, keyboard flow,
  stdin/stdout, or wmenu flag semantics. `-W` and `-f` keep absolute meaning.
- No fractional scale support (still integer `scale`).
- No dynamic recompute on rotation/hotplug mid-run, as in SPEC-UI-004.

## Success Criteria

- On a 1920-wide output the panel is 864 px wide with 27 px text and 32 px rows;
  on a 1280-wide output, 576 px wide with 18 px text; on 2560+, 960 px wide.
- `-W 640` and `-f "16px"` reproduce the previous look exactly.
- Tests and Clippy stay green.
