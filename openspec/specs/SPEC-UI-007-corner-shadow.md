# SPEC-UI-007: Shadow Measured From The Outline

## Goal

The drop shadow wraps the rounded corners. Before this, the falloff was measured
row by row from the body's *span*, so a corner only ever saw a horizontal
distance: the shadow stopped at the arc and left a square, seamed halo.

## Scope

- The shadow's falloff is a true signed distance to the rounded rectangle
  (`outline_gap(qx, qy, r)`), the same outline the body's rounded spans come
  from, so body and shadow cannot disagree at the corner.
- Per row the profile depends on x alone: `qy` is fixed, so the alpha table is
  rebuilt only when a row's geometry changes (rows level with a card's straight
  part share one profile; a card beyond its blur is dropped from the key).
- Nothing is painted under a translucent body: pixels inside a card's own
  outline are skipped, and the row's copy is split around the card's span.
- Same approach as `plush`'s "Wrap the drop shadow around the panel's rounded
  corners" commit, adapted to two cards sharing one pass.

## Code Style

- `Shape` (centre, half extents, clamped radius) + `outline_gap`; the row band
  is a byte table, the row write is a copy of the stretches outside the body.
- No new dependency; geometry stays logical and scales with density.

## Test Strategy

- Corner carries a share of the edge's shadow (measured 4 px out on the corner
  diagonal, at least a third of the edge's alpha), and the diagonal outside the
  arc is shadow-only, never body colour.
- Nothing is painted under the body: the strip inside the top edge is body.
- Shadow still decays with distance and stops at the margin.
- Frame cost at scale 2 with 64 rows stays inside the frame budget.

## Boundaries

- No change to blur radii, offsets, alphas, body colours, hairlines, layout,
  keyboard flow, or matching.
- The corner stays a circular arc (no squircle); the body's own outline is
  unchanged, so no visual difference to the card shape itself.

## Success Criteria

- On screen the halo follows each card's arc with no square corner or seam.
- Tests and Clippy green; a scale-2 64-row frame stays under ~9 ms, the collapsed
  bar under ~2 ms, measured.
