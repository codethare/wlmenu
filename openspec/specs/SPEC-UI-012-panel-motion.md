# SPEC-UI-012: Open and Close Motion, Without the Stutter

## Goal

Two things at once:

1. The height transition stutters. Fix the cause, not the curve.
2. Give the panel an appearance and an exit — macOS 27 Spotlight fades in and
   fades out — instead of appearing and vanishing between two frames.

## Root cause of the stutter (read from the code, not guessed)

Every animated frame called `LayerSurface::set_size`, `set_margin` and
`set_buffer_scale` and then attached a buffer of the *new* size. Each of those is
a request that makes the compositor re-lay-out its scene and answer with a
configure, so the transition was fighting a reflow per frame — and the
`configure` handler then asked for another draw, so frames arrived in bursts.

## Design

- **The surface is sized once per transition**, to the further of its two ends
  (`max(from_h, to_h)`), and the cards animate *inside* it. The compositor never
  reflows the scene while a transition runs; the surface only changes size when a
  transition starts or ends, and only when the value actually differs from what
  was already sent (`Sent`).
- **Opacity** is a scale of the premultiplied buffer (`render::fade`), so the
  panel fades in on open and out on close with no extra compositing protocol.
- **The input region follows the cards**, otherwise the transparent part the
  transition needs would swallow clicks.
- **The exit plays before the process finishes**: `begin_close` starts a height +
  opacity ramp to zero, the loop keeps spinning until it ends, and `finish`
  prints, launches and exits after it. Keys are ignored while it plays.
- Height/opacity use SPEC-UI-011's 180 ms ease-in-out for all three transitions
  (open, resize, close).

## Code Style

- `Anim` carries `from_h/to_h/from_a/to_a` and one `at()`; `draw_panel` takes the
  animated height and the anchor; `fade` is a four-line pixel pass. No new flags,
  no new dependency, no new state machine beyond `closing` and `Sent`.

## Test Strategy

- `anim`: existing height test keeps its claims; new test that opacity ramps
  monotonically to its target in both directions.
- `render`: `draw_panel` draws the cards at the animated height inside a larger
  surface and leaves the rest transparent, for both anchors (top and `-b`).
- `render`: `fade` is a no-op at 1, halves at 0.5, clears at 0.
- Frames of the transition rendered headless over a gradient and checked by eye.

## Boundaries

- No change to layout, geometry, radius, padding, matching, flags, selection
  colour, or the stdin/stdout contract: the same value is printed, and `--run`
  launches the same command — now after the exit animation instead of during it.
- Exit status and the `no_items` fast path are unchanged: an empty stdin still
  exits 1 immediately (nothing to animate).

## Success Criteria

- No per-frame layer-shell requests during a transition; smooth motion at the
  compositor's frame rate.
- The panel visibly fades in and out, and a selection is still printed before the
  process exits.
- Tests and Clippy green.
