# Zendaya Launcher storefront

The shared native egui interface uses a store layout with a global search field and four persistent navigation entries:

- Home features Photozen and links to the library and upcoming games.
- Applications lists the catalog, retaining installation, update, progress and table controls.
- Games clearly announces that games are coming later.
- Library lists detected installed applications, with a useful empty state.

Selecting an application replaces the main content with a full detail page. The sidebar and search remain available. Back and Escape return to the originating section; enlarged screenshots can still use a lightbox. Installation and settings confirmations remain dialogs.

Photozen keeps its PhotoCraft executable stems, display-name alias and bundle ID for existing releases and installations. The launcher keeps its machine IDs and persistence path when displaying the new product name.

The featured image is bundled so discovery and the first Photozen screenshot work offline. Other screenshots and release information retain the existing backend loading and error handling.

Navigation tests: `cargo test -p artcraft-launcher-ui-egui --test ui store_ -- --test-threads=1`.
Set `ARTCRAFT_LAUNCHER_SNAPSHOTS` to an existing output directory to save rendered previews while running these tests.
