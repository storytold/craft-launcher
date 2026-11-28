# ArtCraft Launcher UI facelift: roadmap

How to get from today's window to the one in [`prototype.html`](prototype.html) by letting egui lay
things out, instead of placing every rectangle by hand. Nothing below changes what the launcher does:
`crates/core`, `crates/native`, the app shells, `LauncherUi::logic` / `handle`, the `Command` /
`Event` protocol, the settings file and CI stay as they are. Every button keeps sending the command it
sends today.

## Where the UI is today

The UI is immediate-mode egui, and most of its layout is arithmetic written by hand:

- **Placed rectangles.** `views.rs`, `details.rs`, `widgets.rs` and `app.rs` together have 43 places
  that compute a `Rect` or allocate an exact size (`allocate_exact_size`, `Rect::from_min_size`,
  `painter_at`, `new_child`). The card grid works out its own columns (`width / CARD_MIN_W`) and
  positions every card (`grid_block`). Each card paints its strip, icon, text and buttons at computed
  offsets (`card`).
- **Loose numbers.** 55 literal `add_space(…)` calls, plus font sizes, paddings and widths written
  inline (`22.0`, `13.5`, `176.0`, `206.0 + grip_w` …). Colours already come from `theme::Tokens`;
  spacing and sizes don't.
- **Width checks everywhere.** Narrow layouts are decided locally (`bar_w < 150.0`, `w < 220.0`,
  `toolbar_compact_row`, `fitted`), so each widget guesses on its own when to shrink.

That's why a layout change today means rewriting placement maths, and why the same idea (a card, a
gap, a narrow window) is answered differently in different places.

## Where we want it

1. **Named sizes.** Spacing, type sizes and control sizes are named in the theme, like colours are.
   A layout change is a token change.
2. **Layout primitives instead of arithmetic.** Rows, columns, grids and panels are declared and
   egui sizes them: panels for the frame, `StripBuilder` for fixed and leftover splits, a flex row
   for things that wrap, a grid that fills columns by itself.
3. **One size class.** The window's width maps once per frame to `Compact` / `Regular` / `Wide`,
   and every view reads it, instead of measuring on its own.
4. **Components, not drawings.** The tile, the row, the dock, the drawer and its tabs are widgets in
   `widgets.rs` with clear inputs (an `AppEntry`, a status, a size class) that return the action the
   user took. Views compose them and pass the action on; they don't reach into `self.install_app`
   from inside a drawing routine.

Pixel work doesn't disappear: icons, glyphs, the corner marks and the segmented progress bar are still
painted. But painting is kept inside small widgets, and *where* those widgets go is decided by layout.

## The tools

| Tool | Status for egui 0.37 | Used for |
|---|---|---|
| egui panels (`Panel::top`, `left`, `right`, `bottom`) | Built in, already used | Window frame, side rail, details drawer |
| `egui_extras::StripBuilder` | Built in, already a dependency | Header, banner, button rows: fixed + leftover sizes |
| `egui_extras::TableBuilder` | Built in, already used | Table view (unchanged) |
| `egui_flex` 0.9 | Needs egui ^0.37 (released 2026-10-19) | Rows that wrap: update band, dock actions, status lines |
| `egui_taffy` 0.16 | Needs egui ^0.37 and taffy ^0.15 (released 2026-10-22) | The tile grid and row list: auto-fill columns, rows sized by content |
| `egui_kittest` snapshots | Already set up (`wgpu`, `snapshot` features; 30 snapshots in `tests/ui.rs`) | Proving each step changes only what it means to |

The two new crates need a maintainer's approval. Both releases are under three weeks old: pin exact
versions, check their licences for `ATTRIBUTION.md`, and check the `wasm32` build and its size. If
`egui_taffy` isn't approved, steps 3 and 4 still work with a hand-built grid that reads the new tokens
and size class: less flexible, but no new dependency.

## Steps

Each step is its own PR, ships on its own, and keeps `cargo xtask ci` green. None touches CI.

### 1. Groundwork (no visible change)

- Add `theme::Space` (4, 8, 12, 16, 24, 32) and `theme::Size` (control heights, icon sizes, the type
  scale) next to `theme::Tokens`.
- Replace the loose numbers in `views.rs`, `details.rs` and `widgets.rs` with those names.
- Add `SizeClass` (`Compact` / `Regular` / `Wide`), worked out once in `LauncherUi::ui`, and route
  the existing width checks through it.
- Done when the existing snapshots in `tests/ui.rs` pass without being regenerated.

### 2. Actions out of the drawing code

- Card, row and menu functions return a `UiAction` (or `Option<Command>`) instead of calling
  `install_app`, `launch_app` or `act_command` themselves. `LauncherUi` applies it, as `flush_actions`
  already does.
- Done when every test passes and the snapshots are unchanged. This step is what makes the later
  layout changes safe.

### 3. The frame

- Side rail (`Panel::left`): the filters with their counts, pinned apps, and the footer's contents
  (last check, platform, links). The footer panel goes.
- Header: search, the Grid/Table switch, refresh. Filter chips replace the rail in the `Compact`
  size class.
- Snapshots are regenerated on purpose and reviewed as images in the PR.

### 4. Tiles and rows

- Installed apps as tiles in an auto-fill grid (at least 208 px per column), with status and the one
  main action pinned to the bottom edge, so actions line up across a row.
- Apps that aren't installed as 78 px rows with the same columns in every row (icon, name and
  description, Install, "…").
- The number strip goes; Website / GitHub / Releases move into the "…" menu (`actions_in(…,
  links_in_menu)` already supports this).
- Update band above the grid, naming what will update.

### 5. Dock and details drawer

- The launcher dock: the selected installed app (icon, name, headline, Open / Update / Details) over
  a row of every installed app's icon. Click selects, double-click opens. It uses the existing launch
  command and launch counts.
- App details as a drawer from the right (`Panel::right` or an `Area` anchored right) with tabs:
  Overview, Pictures, Features, Release. Lines and accents take the app's colour through
  `Tokens::app_ink` and `tint`. The Release tab holds the release notes and Choose version, both
  dialogs that exist today.

### 6. Polish

- The hover touches: app-colour tint and line, corner marks, a segmented progress bar.
- Keyboard: Ctrl+K for search, ← → through tabs and pictures, Esc closes the top layer.
- A last pass on the table view so it uses the same tokens.

## Rules that still apply

From [AGENTS.md](../../../AGENTS.md), unchanged by this work:

- Non-test code never panics: no `unwrap`, `expect`, indexing on network data.
- Colours come from `theme::Tokens`; square corners and 1 px hairlines; Archivo, Inter and Geist Mono.
- Motion follows the OS (`Host::reduce_motion`); progress bars and spinners keep moving.
- Every new user-facing string goes through `tl!` and gets a row in every catalog marked
  `complete_menus`. The plan reuses existing strings wherever it can (All, Installed, Updates, Not
  installed, Pinned, Update all, Search apps, Choose version, Release notes…).
- Details pictures stay out of this repo: they load from `craft-launcher-assets` at a pinned tag.

## Decisions needed

1. Is a side rail acceptable next to the getartcraft.com apps page, which has no rail?
2. May `ui-egui` take `egui_flex` and `egui_taffy` as dependencies?
3. Can the Website and GitHub links leave the card face and live in the "…" menu?
4. Can the footer go, with its content in the rail?
5. Should app details open as a tabbed drawer from the right instead of today's centred page?
