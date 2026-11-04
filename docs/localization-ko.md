# Korean interface

Korean (`ko`) is selectable in Settings → Language and follows `ko`, `ko-KR` and
POSIX locale names such as `ko_KR.UTF-8` in Auto mode. Set
`ARTCRAFT_LAUNCHER_LOCALE=ko-KR` to try it without changing the system locale.
The preference uses `ko` and survives a restart.

The catalog is `crates/ui-egui/src/i18n/ko.tsv`, translated from the English source
labels. App-card headlines describe concrete functions from the same app’s English
details page instead of literally repeating vague familiarity or marketing claims. Korean uses one plural form. Product names, release notes, keyboard names,
paths and diagnostics without a matching message template stay as published.
“Grid / Table” is rendered as “카드 / 표” to describe the two app-list layouts.

No font is bundled. The desktop uses Apple SD Gothic Neo on macOS, Malgun Gothic on
Windows, or the KR face of Noto Sans CJK / Noto Sans KR / NanumGothic on Linux and
FreeBSD. Missing fonts keep the interface in English using the existing font gate.
As with the other system-font languages, the browser build currently stays English.

The catalog tests check completeness, placeholders, duplicates, ellipses and plural
forms. Korean-specific tests cover locale matching, dates, status messages, glyph
coverage in every UI font family, switching languages, saved preferences and the
existing install actions. `ARTCRAFT_LAUNCHER_SNAPSHOTS=<directory>` saves the Korean
settings, dark card view and light table view from the UI test:

```sh
cargo test -p artcraft-launcher-ui-egui --test korean -- --test-threads=1
```
