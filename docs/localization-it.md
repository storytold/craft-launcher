# Italian interface

The `it` catalog covers the complete ArtCraft Launcher interface at the accompanying source commit, including
the tray, settings, status messages, plural counts, app descriptions and package formats.
Choose **Settings → Language → Italiano**. Auto detects `it`, `it-IT`, `it-CH` and
POSIX locale forms such as `it_IT.UTF-8`; the explicit choice is saved by the existing settings.

The translations are original Italian text based on the meaning of the English
source strings, licensed under MIT OR Apache-2.0. They address the user as “tu”.
Product names, command IDs, shortcuts, placeholders and paths remain unchanged.
Italian uses the existing one/other integer plural rule and the bundled Latin fonts.
Dates use day, month, year. A technical error supplied by macOS or another program
keeps its original detail while its enclosing status message is translated.

This catalog translates the launcher. Each app in the suite has its own localization
implementation and release: changing the launcher's language does not translate an app.
The TSV is compiled into the program; the official release does not load it as a
separate language pack. New versions therefore need the catalog integrated and rebuilt.

Validation: the existing catalog tests check key coverage, duplicates, placeholders,
plural forms and ellipses. The Italian regression test checks locale selection,
the saved preference, date order, counts and formatted error messages.
