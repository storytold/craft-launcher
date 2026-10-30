# Persian localization

Select **فارسی** in Settings, or use a locale such as `fa-IR` or `fa_IR.UTF-8`.
The existing source-keyed TSV registry handles translations and English fallback.
Product names and release notes stay as published; dates retain the Gregorian calendar.

## Direction and typography

The main header, dialog headers and actions, product identity and screenshot navigation keep
the upstream layout. Persian card order/content, settings bodies, popup options and release
facts read right to left. Scrollbars retain a reserved gutter so they do not cover labels.

The upstream egui 0.37 fork handles logical Unicode text, bidi, shaping, wrapping and editing.
There is no display-order conversion or separate bidi dependency in this localization.
The crate-private `rtl` helpers only scope content layout/alignment and translated tooltips.
Never reorder stored text, paths, versions, clipboard data or backend messages for display.

Vazirmatn's small OFL variable fallback follows the original typography weight roles and
keeps the Latin faces first. Source and license are in `ATTRIBUTION.md` and
`assets/fonts/Vazirmatn-OFL.txt`; no proprietary font or local preview runner is bundled.

## Maintenance and verification

App descriptions and feature copy use the app id as context and the exact English source
as key. Changed copy falls back to current English until translated. Add new interface
strings to the complete Persian catalog through the same coverage/placeholder/plural gates
as other complete languages. Avoid requiring translations of all future editorial copy.

Run `cargo xtask ci` (formatting, strict clippy, workspace tests and WASM). Persian integration
coverage exercises logical accessibility names, localized search, card order, product details,
menus and compact Settings. Optional screenshots use `ARTCRAFT_LAUNCHER_SNAPSHOTS` as in the
existing UI tests. Before release, review the bundled font and native mixed Persian/Latin
input, selection, clipboard and screen readers on Windows, macOS and Linux. Automated UI
coverage does not substitute for those platform checks.
