# SPEC-UI-001: Panel Polish

## Goal

Refine the existing software-rendered menu panel with clearer input/list hierarchy and a lighter selected-row treatment inspired by current macOS app surfaces.

## Scope

- Increase the panel corner radius from 10 to 12 logical pixels.
- Increase content padding from 12 to 16 logical pixels.
- Draw a 1 logical pixel divider between the input row and result list.
- Draw selected rows with an 8 logical pixel horizontal inset and 8 logical pixel corner radius.
- Keep all geometry HiDPI-scalable.

## Code Style

- Keep changes in the existing constants and pure software renderer.
- Reuse `rounded_span`, `rect`, `blend_px`, and existing row geometry.
- Add no dependency, configuration, asset, or rendering abstraction.

## Test Strategy

- Extend headless pixel tests for divider visibility and selected-row inset/rounding.
- Verify the new geometry scales at 2x.
- Run `cargo fmt --check`, `cargo test`, and `cargo clippy`.

## Boundaries

- Do not change default colors or wmenu flag semantics.
- Do not change panel width, row height, animation, keyboard behavior, filtering, stdin, or stdout.
- Do not add grid/category layouts, app icons, themes, or true blur.
- Do not add cairo/pango or any new dependency.

## Success Criteria

- The panel uses a 12 px radius and 16 px content padding.
- Expanded results have a subtle 1 px divider.
- Selected rows are visually inset and rounded without changing row height.
- Custom wmenu colors still drive prompt, list, selection, and text rendering.
- Existing and new tests pass, with no new Clippy warnings.
