# SPEC-UI-004: List Fits The Output

## Goal

The result list never runs past the screen, and it stops early enough that the
gap under the list is about the same as the gap between the search bar and the
top of the screen — the panel stays a floating card on any output instead of
growing into the bottom edge.

## Scope

- Track the output's logical height next to its width (`surface_enter` already
  reads `info.logical_size`; the height half is currently dropped).
- Cap the visible rows by the space the panel actually has:
  `rows = (output_h - gap_top - gap_bottom - bar_h) / row_h`, at least 0.
  `gap_top`/`gap_bottom` are the panel's visible gaps: `TOP_MARGIN` on both
  sides in the top-anchored (Spotlight) mode, `SHADOW` on both sides in `-b`.
- Precedence: `-l N` and `MAX_VISIBLE` stay upper bounds; the screen cap always
  wins, because it is a physical limit rather than a preference.
- Before the output size is known (`surface_enter` not yet seen), keep today's
  behaviour (`MAX_VISIBLE`), and redraw once the size arrives.

## Code Style

- One pure function for the arithmetic, unit-tested without a compositor:
  `max_rows(output_h, row_h, bar_h, gap, scale) -> usize` (buffer px, saturating,
  no panic on absurd sizes).
- `App::visible()` keeps its shape: `min` of user row count, `MAX_VISIBLE`, and
  the screen cap.
- No new module, no new dependency, no new flag.

## Test Strategy

- Assert-based unit tests on `max_rows`: 1080p/1440p/4K heights, `scale` 1 and 2
  give the same row count for the same logical output, a screen too short for a
  single row yields 0, and an unknown/zero height does not underflow.
- Existing tests must keep passing (`visible()` still honours `-l` and
  `MAX_VISIBLE`).
- `cargo test`, `cargo clippy`.

## Boundaries

- No change to row height, font size, padding, margins, keyboard flow, scrolling
  (the viewport/thumb follow `visible` automatically), stdin/stdout contract, or
  wmenu flag semantics.
- No dynamic re-query of the output on rotation/hotplug mid-run: the height is
  read when the panel enters the output. Documented as a limit.
- No `-l` semantics change: `-l 0` still means auto.

## Success Criteria

- On a 1080p output at scale 1 the list stops at 32 rows (30 px rows, 32 px gaps)
  instead of 64, and no panel ever extends below `output_h - TOP_MARGIN`.
- `-l 100` on a short screen shows the rows that fit, not 100.
- The panel keeps its current position, width, animation and scrolling.
