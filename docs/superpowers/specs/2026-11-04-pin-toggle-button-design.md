# Pin toggle button on app cards — design

**Date:** 2026-11-04  
**Repo:** storytold/craft-launcher  
**Status:** approved  
**Builds on:** `2026-11-02-pin-apps-design.md` (#1149)

## Goal

Make pinning as discoverable as reordering: every visible, non-hidden app card/row gets a pin toggle button. Keep the `…` menu Pin to top / Unpin items.

## Decisions

| Topic | Choice |
|---|---|
| Affordance | Outline pin = pin; filled pin = unpin |
| Menu | Keep Pin to top / Unpin / Move up / Move down |
| Grip | Still only when pinned (reorder among pins) |
| Hidden apps | No pin button (menu still omits Pin to top) |
| Surfaces | Grid cards and table rows; paused-download rows too |

## Layout

`[primary | resume…] [pin toggle] [grip if pinned] [… | discard…]`

Width reservation (`squares`) must include the pin square always (when not hidden) and the grip only when pinned, so Open/Resume does not jump.

## Behaviour

- Click outline pin → `settings.pin_app`
- Click filled pin → `settings.unpin_app`; if that id (or the drag source) is no longer pinned, clear `pin_dragging`
- Pin click uses `Sense::click` only — must not latch `pin_dragging`
- Active download progress rows stay as today (pause/cancel only); no new pin there

## Glyphs

Painter-drawn bookmark ribbon (`Glyph::Pin` outline / `Glyph::PinFilled` fill), accent-coloured when pinned. Button accessible names are per-app (like the grip): **Pin {name} to top** / **Unpin {name}**. Menu keeps plain **Pin to top** / **Unpin**. Move up/down stay on grid/table only (not the details drawer).

## CodeRabbit guards (from #1149)

- Correct `squares` / `main_w` on normal and paused rows
- Hidden apps: no pin button
- Unpin mid-drag clears drag state safely
- Glyph shapes stay inside the control rect (bounds test)
- UI tests: button pins, button unpins, hidden has no pin button; menu still works
- No unwrap/expect/todo in production paths

## Out of scope

- Changing pin order persistence or Pinned section rules
- Pin button on mid-download progress rows
