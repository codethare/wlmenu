# SPEC-UI-002: Full-Row Selection

## Goal

Restore the selected result background to full row coverage, while keeping the UI-001 panel radius, padding, and input/list divider.

## Scope

- Remove the selected-row horizontal inset and inner corner radius.
- Fill the selected background across the panel width, clipped only by the outer rounded panel outline.
- Keep the existing row height, selection colors, and text placement.

## Code Style

- Reuse the existing `rounded_span` and `set_span` primitives.
- Remove the now-unused selection geometry constants.
- Add no dependency, configuration, or rendering abstraction.

## Test Strategy

- Update the headless render test to assert selection reaches both panel edges.
- Keep the 2x geometry test and divider assertions.

## Boundaries

- Do not change default colors or wmenu flag semantics.
- Do not change panel radius, padding, width, row height, animation, keyboard, stdin, or stdout behavior.
- Do not modify UI-001 divider behavior.

## Success Criteria

- A selected row uses the full available panel width.
- The background remains clipped by the outer rounded panel corners.
- Other UI-001 changes remain intact.
- Formatting, tests, and Clippy pass.
