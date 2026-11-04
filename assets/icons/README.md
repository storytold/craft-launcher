# Menu icons

The small icon at the start of every row in an app's `...` menu. All from
[Lucide](https://lucide.dev) 0.468.0, ISC licensed, on its own 24 px grid.

| File | Lucide name | Row it marks |
| --- | --- | --- |
| `file-text.svg` | `file-text` | Release notes |
| `download.svg` | `download` | Download again, and the per-platform download list |
| `play.svg` | `play` | Open |
| `folder-open.svg` | `folder-open` | Show in Explorer / Reveal in Finder / Open containing folder |
| `rotate-ccw.svg` | `rotate-ccw` | Reinstall |
| `trash-2.svg` | `trash-2` | Uninstall |
| `tag.svg` | `tag` | All releases |
| `eye-off.svg` | `eye-off` | Hide App |
| `eye.svg` | `eye` | Unhide App |

`globe.svg` (Website) and `github.svg` (GitHub repository) live in `assets/social/`.

The `stroke="currentColor"` Lucide ships was changed to `stroke="#FFFFFF"`, the same trick
`assets/social` uses, so the app can tint each icon to its own colour: muted grey on most rows,
red on Uninstall. `tag.svg` carries one more of the same: the dot's `fill` is white too, or the
tint would leave it black.

Lucide is ISC. Its notice, which the licence asks to travel with every copy, is
`LICENSE-Lucide.txt` beside these files and is also listed in the repository's `NOTICE`.

Nothing here draws a character: the icons are images, so no font has to carry a symbol. The menu
row paints the icon and then the label, and a screen reader is told only the label.
