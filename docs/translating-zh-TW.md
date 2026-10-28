# Taiwan Traditional Chinese draft

ArtCraft Launcher now registers `zh-TW`, shown in Settings > Language as Traditional Chinese (Taiwan) / 繁體中文. With Auto selected, `zh-TW`, `zh-Hant`, Hong Kong and Macau locales select this catalog. An explicit `zh-Hans` still selects Simplified Chinese, even with a Taiwan region. English remains the fallback.

The editable language pack is `crates/ui-egui/src/i18n/zh-TW.tsv`. These are AI-assisted translations from the English keys, with Taiwan terminology corrections; native review is pending. `complete_menus` remains false so future strings can fall back to English while the draft is reviewed. Product names, command IDs, placeholders and the download/install protocol remain unchanged.

Desktop uses an installed Traditional Chinese face: Microsoft JhengHei on Windows, PingFang TC on macOS or Noto Sans CJK TC on Linux. No CJK font is bundled or downloaded. Without a suitable face, the existing font policy keeps the UI in English. The web build has the same limitation.

Update by retaining existing values and adding new English keys; preserve the context column, placeholders and one Chinese plural form. `cargo xtask ci` checks formatting, Clippy, tests and WASM. No Python or online translation service is required to build or run the app.
