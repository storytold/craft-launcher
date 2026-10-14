# Alternate Craft App icons

The extra icon sets the launcher offers in **Settings → App icons** (and in an installed app's `…`
menu): each Craft App's own mark on a flat tile, so the icon in the Dock, the taskbar, the Start
menu and the application menu can be changed — one app at a time or all together.

This directory is **data**. Nothing here is listed in Rust by hand: `crates/core/build.rs` reads it
at build time and generates the pack registry, so adding a pack means adding files.

## Layout

```text
alternate_icons/
  README.md                    this file
  packs.tsv                    one row per pack: folder<TAB>English label
  initial-icons/
    dark/                      a pack. Its folder path is its id: `initial-icons/dark`
      photocraft.svg           the supplied artwork — the file stem is the app id
      photocraft.png           the 512 px render, written by packaging/icons.sh
      vectorcraft.svg
      vectorcraft.png
      ...
    light/                     another pack
      ...
```

- **A pack is a folder of `<app>.svg` files.** One directory, either directly under
  `alternate_icons/` (`mono/`) or one level down (`initial-icons/dark`).
- **The file stem is the app id** — `photocraft.svg` is PhotoCraft's icon in that pack. The same id
  `crates/core/src/catalog.rs` uses, so a pack only answers for the apps it was actually drawn for.
- **Every `<app>.svg` has a 512 px `<app>.png` beside it**, written by `packaging/icons.sh` and
  committed. That render is what gets bundled; the launcher scales it down to the sizes each OS
  asks for.
- **`packs.tsv` names every pack** with the English label shown in the UI.

## How the pieces fit

| Where | What it does |
|---|---|
| `packs.tsv`, the folders here | The data: which packs exist, what they are called, which apps they cover |
| `crates/core/build.rs` | Reads them and writes `OUT_DIR/icon_packs.rs`; fails the build if the two disagree |
| `crates/core/src/icons.rs` | `include!`s that file and exposes `PACKS`, `artwork`, `packs_for` |
| `crates/ui-egui/src/icons.rs` | Draws the artwork in the launcher's own grid, table and drawer |
| `crates/native/src/app_icons.rs` | Puts it on the installed app: icon theme, `NSWorkspace`, shortcuts |

## Adding an app to an existing pack

1. Drop `<app>.svg` into the pack's folder, named after the app's id — `wordcraft.svg` for
   WordCraft. The stem must match the id in `crates/core/src/catalog.rs` exactly; a typo is caught
   by `every_artwork_file_is_named_after_a_real_app` in `cargo xtask ci`, because the file would
   otherwise be bundled for nothing while the app kept its old icon.
2. Run `packaging/icons.sh` (needs `resvg`) and commit the `<app>.png` it writes beside it. The
   build refuses to run without that render, so this step is not optional.
3. That is all — the picker now offers that pack for that app, and the app wears it on the desktop
   too. Nothing in Rust lists which apps have artwork: it is read from the files.

### Drawing one

- **Square artwork, 1:1.** The render is exactly 512 × 512, so a view box such as
  `0 0 1024 1024` is what the existing icons use. `build.rs` warns when a view box isn't square,
  because the icon would come out stretched. Vector is best — the launcher scales the one render
  down to 16 px for the icon theme and up to 256 px for a Windows `.ico`.
- **A full-bleed tile.** These are the app's mark on a flat background, not a rounded icon with
  transparent corners: the tile fills the whole square, edge to edge, as the existing ones do.
- **Cover the app, not just one platform.** The same file becomes the Dock icon, the taskbar icon
  and the menu icon, so it has to read at 16 px as well as at 256.
- **It is a trademark, not clip art.** The marks belong to the ArtCraft Team
  ([`docs/brand/LICENSE-brand.txt`](../../docs/brand/LICENSE-brand.txt)); draw alternates of them
  with the Team, and never redraw an app's own icon.

### Partly done is fine

A pack only answers for the apps it actually holds. If only `initial-icons/dark/wordcraft.svg`
exists, then picking *Initial dark* gives WordCraft that icon while *Initial light* leaves it with
its own — no error, and the picker still offers both. Finish the second pack whenever you like.

## Adding a new pack

1. Make the folder, e.g. `alternate_icons/mono/`, and drop in one `<app>.svg` per app it covers.
2. Run `packaging/icons.sh` and commit the renders it writes beside them.
3. Add one row to `packs.tsv`, in the position you want it offered in:
   ```
   mono	Mono
   ```
4. Add the same English label to **every** catalog in `crates/ui-egui/src/i18n/*.tsv` (a plain
   `label<TAB>translation` row). The catalogs marked `complete_menus` are all checked by
   `complete_catalogs_translate_every_icon_pack`, and `cargo xtask ci` fails without them.
5. Add a row to [`ATTRIBUTION.md`](../../ATTRIBUTION.md) for the artwork — every bundled asset
   needs one, with its author, source and license.
6. `cargo xtask ci`.

## Rules

- **A pack's folder is its id**, saved in the user's settings. Renaming one after a release sends
  everyone's saved choice back to *Default* (it degrades gracefully rather than erroring, but the
  choice is lost). Add a new folder instead.
- **`packs.tsv` and the folders must agree.** A folder of artwork with no row would be invisible;
  a row with no folder would be a broken pack. `build.rs` refuses to build either way, and says
  which one is missing.
- **Never redraw the artwork by hand.** The `*.svg` files are supplied design files; the `*.png`
  files are generated from them and nothing else. Regenerate rather than touch.
- **The renders are committed**, like `assets/app-icon/` — packaging and the Rust build never need
  `resvg`.
- **These icons are ArtCraft trademarks**, not MIT or Apache-2.0: see
  [`docs/brand/LICENSE-brand.txt`](../../docs/brand/LICENSE-brand.txt).

## Removing a pack

Delete its folder, its `packs.tsv` row, its rows from the i18n catalogs and its
`ATTRIBUTION.md` row, then run `cargo xtask ci`. Settings that named it fall back to *Default* on
their own.
