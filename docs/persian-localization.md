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

## Font provenance and maintainer decisions

The bundled file is the unmodified `fonts/variable/Vazirmatn[wght].ttf` from
[rastikerdar/vazirmatn at 83629f877e8f084cc07b47030b5d3a0ff06c76ec](https://github.com/rastikerdar/vazirmatn/blob/83629f877e8f084cc07b47030b5d3a0ff06c76ec/fonts/variable/Vazirmatn%5Bwght%5D.ttf).
It is 241,328 bytes. Its SHA-256, also recorded in `ATTRIBUTION.md`, is:

```
696249a2c74b39ffdef55de4df2809c5b639d3ff80d618d8160a095d2fd49dca
```

Verify the local file with `shasum -a 256 assets/fonts/Vazirmatn-Variable.ttf`.
This small fallback gives the shared desktop/web UI consistent Persian glyph coverage and
weight roles without relying on a font installed on the host. It does not change the CJK or
Georgian system-font paths. Bundling this fallback and keeping RTL scoped to content are
proposed product decisions for maintainers to confirm; they do not establish a policy for
other scripts. Locale matching uses the same Auto preference as the other registered languages.

Persian-language feedback informed translation, spacing and direction during development.
The automated tests verify coverage, placeholders, glyphs, logical accessibility labels and
interactions. They do not certify native-speaker approval or platform accessibility. Final
native-language review and Windows/Linux runtime and assistive-technology QA remain release
checks, and should be recorded against the actual commit tested.
