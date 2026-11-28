# UI facelift proposal

A layout-only facelift for ArtCraft Launcher: same features, new places, drawn with the launcher's
existing tokens, fonts, square corners and hairlines.

| File | What it is |
|---|---|
| [`proposal.html`](proposal.html) | The proposal: what moves where, the technical options, costs and risks, the delivery plan and the decisions needed, with a still of the new window. |
| [`prototype.html`](prototype.html) | A working web prototype of the new window. Click through the dock, filters, search, sort, the "…" menu, the details drawer and its tabs, and the Uninstall, Release notes and Choose version dialogs. |
| [`ROADMAP.md`](ROADMAP.md) | How to build it in egui step by step, with layout primitives and named sizes instead of hand-placed rectangles. |

Both HTML files are self-contained: open them straight from disk in any browser, offline. Only the
fonts load from Google Fonts, and fall back to system fonts without a connection.

## What the prototype shows

- **Real data:** the 17 apps of `crates/core/src/catalog.rs`, their details from `core::details`, and
  each app's last 8 GitHub releases (versions, dates, download sizes, and the latest release notes),
  fetched on 2026-11-04.
- **A sample:** which apps are installed and at which version, the download in progress, and the
  open counts behind "Frequently used". **Reset sample** restores them.
- **Simulated:** installing, updating and uninstalling. Nothing is downloaded.

## Note on the pictures

`prototype.html` embeds the details pictures from
[`storytold/craft-launcher-assets`](https://github.com/storytold/craft-launcher-assets) (tag `v1`),
shrunk to 1000 px, so it works offline. AGENTS.md keeps those pictures out of this repo, so this file
is a review copy for the proposal: it can be dropped from the branch before merge, or kept under that
exception if the maintainers prefer.
