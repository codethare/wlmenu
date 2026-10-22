# SPEC-UI-011: Motion You Can See

## Goal

The expand/collapse transition runs at 120 ms, which is about the threshold where
a height change stops reading as motion: the panel appears to jump. Lengthen it
to a duration the eye can follow, and change the curve so the longer version
still feels fluid instead of snapping off the line.

## Scope

- `anim.rs`: `ANIM` 120 ms → 180 ms.
- `anim.rs`: ease-out cubic → ease-in-out cubic, so the first frames leave
  slowly as well as arriving slowly.

## Why these numbers

- 180 ms sits where a size change is unmistakably animated (240 ms is the point
  where a menu starts to feel like it is waiting for itself; 120 ms sits under
  the perception threshold). Transitions restart from the current height on the
  next keystroke, so a longer glide does not queue up behind typing.
- Pure ease-out moves fastest on the very first frame. At 120 ms that start was
  invisible; at 180 ms it is not, and it reads as a jolt. Ease-in-out cubic has
  zero velocity at both ends, which is the "smoothness" the slower duration
  needs.

## Code Style

- One constant, one expression in `height()`, no new state, no new flag.

## Test Strategy

- Existing test keeps its claims: starts at `from`, mid-flight between the ends,
  ends exactly at `to`, collapse monotonic.
- New assertion: at a tenth of the duration the height has moved less than 5% of
  the distance — i.e. the curve eases in (a linear or ease-out curve would be
  far past that).

## Boundaries

- No change to layout, geometry, frame clock, blinking, filtering, or flags.
  Nothing but the duration and the easing shape moves.

## Success Criteria

- The expansion is visible as motion on a 60 Hz output and still ends on the
  target without a visible stop.
- Tests and Clippy green.
