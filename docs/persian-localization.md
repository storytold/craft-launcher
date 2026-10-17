# Persian localization

Select **فارسی** in Settings, or use an OS locale such as `fa-IR`. Persian is registered
through the existing language registry; settings serialization and backend protocols are
unchanged. All menu strings, notifications, package labels and message templates use the
existing source-keyed TSV catalog. Product names remain in English.

## Direction and typography

The main header, product identity, screenshot navigation and dialog action positions keep
the original layout. Filters, card order/content, settings content, popup options and product
details content flow from right to left. Technical values (versions, URLs and paths) retain
their Latin order. Editable strings remain logical Unicode; display-order text is never
persisted or sent to the backend.

The small `rtl` module adapts text at the paint boundary for egui 0.36's per-script shaping.
It uses `unicode-bidi` for directional runs. Wrapped copy is split in logical order before
each line is reordered. Custom-painted labels and tooltips expose logical accessibility text. Keep using
`rtl::paragraph` for wrapped prose, rather than wrapping a pre-reordered label. This is a
scoped presentation adapter, not a replacement for native bidirectional text editing.

The adapter retains attribution to NibrasAz7's work in
[PR #397](https://github.com/storytold/craft-launcher/pull/397). Its API is crate-private.

Vazirmatn's OFL variable font is bundled as a fallback after the original Latin faces.
Weights follow the existing regular, medium, semibold, display and mono roles. Source and
license are in `ATTRIBUTION.md` and `assets/fonts/Vazirmatn-OFL.txt`. No proprietary font,
local font path, preview runner or owner-specific configuration is part of this change.

## Editorial copy and dates

Product descriptions, feature copy and screenshot captions use the app id as translation
context and the exact English source as key. A changed or new description falls back to
English until translated. Release notes remain in their source language. Gregorian dates
retain the existing conversion and sorting; only their month names are translated. Solar
Hijri conversion and third-party product comparison labels are outside this change.

## Verification

```sh
cargo test -p artcraft-launcher-ui-egui
cargo clippy -p artcraft-launcher-ui-egui --all-targets -- -D warnings
cargo xtask wasm
cargo xtask ci
```

The registry marks Persian menus complete, so the shared coverage, placeholder, plural and
package-format tests apply. Additional checks cover bundled glyphs and weight roles,
logical text wrapping, mixed technical tokens, localized search, popup interactions and
scrollbar clearance, direction boundaries and compact settings windows. Optional rendered
screenshots follow the existing `ARTCRAFT_LAUNCHER_SNAPSHOTS` test convention. Persian tests
use the actual bundled font without a workstation font override.

Before landing, verify the normal desktop shell and native keyboard/screen-reader behavior
on the supported platforms. In particular, egui's native text input/caret behavior is not
implemented by this display adapter; mixed-direction editing needs separate platform QA.
