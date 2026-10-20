# SPEC-UI-006: Bar and List as Separate Cards

## Goal

The input bar and the result list read as two floating cards with air between
them, the way Spotlight separates its search field from its results — not as one
panel with a divider line inside.

## Scope

- The panel column splits into two cards: the input bar (one row) and the result
  list, separated by `SEPARATION = 8` logical px.
- Each card gets its own rounded outline, its own 1 px coverage antialiasing and
  its own hairline rim, from the same `Panel` machinery the single panel used.
- The gap exists only while the list does: the collapsed bar stays one row tall
  and gains no gap.
- The shadow pass covers every card: the per-row band is the union of the cards'
  bands, and a pixel takes the strongest contribution (a gap pixel sees both the
  bar's lower shadow and the list's upper one).
- The divider (`DIVIDER_H`/`DIVIDER_ALPHA`) is removed: the gap replaces it.
- The screen budget counts the gap: `max_rows` reserves `row_h + SEPARATION`, so
  the list still cannot reach the screen edge.

## Code Style

- One `body(buf, w, panel, color)` helper draws any card; `shadow` takes a slice
  of panels instead of one.
- A row's shadow is painted as the band minus the body span (two intervals), so
  no per-pixel "is this the body" test remains on the hot path.
- No new module, dependency, or flag; all geometry stays logical and scales.

## Test Strategy

- The selection fills the list card inside its hairline; the gap holds neither
  body colour and is less opaque than a body.
- The bar's rim is lit at the top and dark at the bottom within its flat band.
- Card geometry is checked past the corner radius, where a card's row is a
  straight span.
- Perf stays in budget at scale 2 with 64 rows (measured).

## Boundaries

- No change to row height, font sizing, prompt/rail rendering, keyboard flow,
  matching, or stdin/stdout.
- No per-card transparency settings, no blur, no new flags.

## Success Criteria

- On screen: bar card, 8 px of air, list card, each with rounded corners and a
  hairline, sharing one shadow.
- A collapsed bar is unchanged in height and position.
- `cargo test` and Clippy stay green; a scale-2 64-row frame stays within ~1 ms of
  the single-panel cost.
