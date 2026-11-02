# Pin apps to top — design

**Date:** 2026-11-02  
**Repo:** storytold/craft-launcher  
**Status:** approved

## Goal

Let users pin catalog apps to a fixed block at the top of the launcher (grid and table), and reorder those pinned apps by drag-and-drop. Pin order is intentional and is not scrambled by the active sort.

## Decisions

| Topic | Choice |
|---|---|
| Pin entry points | `…` menu **Pin to top** / **Unpin**, plus a drag handle on pinned items |
| Sort interaction | Pinned stay in user order at top; sort applies only to unpinned apps |
| Grid grouping | One **Pinned** block first; then existing Installed / Not installed for the rest |
| Persistence | `Settings.pinned: Vec<String>` (catalog ids, order = display order) |
| Hide interaction | Hiding an app removes it from `pinned` |

## Data

- Add `pinned: Vec<String>` to `Settings` with `#[serde(default)]`.
- Helpers (on `Settings` or next to sort logic):
  - `is_pinned(id)`
  - `pin(id)` — append if absent
  - `unpin(id)` — remove if present
  - `reorder_pinned(from, to)` — move within the vec; ignore out-of-range
- On **Hide App**, also `unpin`.
- Unknown / removed catalog ids in `pinned` are skipped when building the visible list (left in settings until unpin/hide so re-adding an app can restore order later is unnecessary; prune on load is optional — prefer skip-at-display for simplicity).

## Ordering

`LauncherUi::visible_ids` (or equivalent):

1. Collect filtered/search-matching entries as today.
2. Partition into pinned vs unpinned using `settings.pinned` membership.
3. Emit pinned ids in `settings.pinned` order (only those present in the filtered set).
4. Sort unpinned with the existing sort keys; in Grid, still put installed before not-installed within the unpinned set only.

## UI

### Menu

In `menu` / `menu_hide` area (before Hide):

- If not pinned: **Pin to top**
- If pinned: **Unpin**

### Drag handle

- Shown only when the app is pinned (grid card and table row).
- Painter-drawn grip (six dots or two bars), matching existing glyph buttons — not emoji.
- Accessible name e.g. **Reorder {name}**.
- Drag reorders only among pinned siblings; drop inserts before/after the hovered pinned target.
- Escape cancels an in-progress drag and restores the pin order from drag start (hover already mutates order live).
- Cursor: grab / grabbing.
- Respect `reduce_motion` only for any visual feedback animation; reorder itself is immediate.

### Section header (grid)

When the pinned block is non-empty, show a **Pinned** HUD label (same style as **Not installed**) above that block.

### Table

Same id order as grid’s logical list (pinned first). Handle sits near the app name or before the `…` control without changing column semantics.

## i18n

Add English keys to every `complete_menus` catalog TSV:

- `Pin to top`
- `Unpin`
- `Pinned`
- `Reorder {name}` (accessible name / tooltip)

Translate clean-room from meaning; product names stay English.

## Tests

- Unit: pin/unpin/reorder helpers; hide clears pin; `visible_ids` puts pinned first in pin order then sorted unpinned.
- UI harness: menu shows Pin/Unpin; after pin, Pinned header (grid) / order in table; reorder via helper or simulated drag if harness allows.
- Settings round-trip: `pinned` survives serde default for old settings files.

## Out of scope

- Syncing pins across machines
- Changing upstream suite catalog order
- A new Sort key for “Pinned”
- Pinning while the Hidden filter is active (hidden apps are not in the main list; unpin-on-hide covers this)

## Files (expected)

- `crates/ui-egui/src/settings.rs` — field + helpers
- `crates/ui-egui/src/app.rs` — `visible_ids` partition
- `crates/ui-egui/src/views.rs` — menu, grid sections, drag, table handle
- `crates/ui-egui/src/widgets.rs` — grip glyph / handle widget if needed
- `crates/ui-egui/src/i18n/*.tsv` — strings
- `crates/ui-egui/tests/ui.rs` (+ unit tests near helpers)
