# Community GitHub sources

The desktop ArtCraft Launcher can manage opt-in community repositories under **Settings → Sources**.
These apps are marked **Unofficial** and do not replace entries in the official Craft App Suite.
No community repositories are bundled or suggested by default.

## Add and use a source

1. Enter a public GitHub `owner/repository` or `https://github.com/owner/repository` URL.
2. Choose a display name and the package filename prefix (for example, `pdfcraft` for a fork
   whose release files still start with `pdfcraft-`, regardless of the repository name).
3. Confirm that you trust the publisher, then select **Add source**.
4. **Refresh** checks its latest published stable release. **Install / update** downloads a
   compatible package; **Open** starts the installed copy. **Releases** opens the repository's
   release page, including on platforms without a managed installer.

Sources and enabled states persist with the launcher's settings. Enabled sources are checked
once at startup, sequentially; additional checks and all community installations/updates are
manual. Official automatic updates never install community releases. Disabling a source stops
its startup checks and actions. **Remove source** forgets it without deleting installed files.
Adding the same repository again finds the installation when the package name is unchanged.
Repository names are case-insensitive and duplicate sources are rejected (at most 32 sources).

## Publisher package contract

The first version supports public GitHub repositories and forks, using their own release assets:

- Windows: `<package>-<version>-windows-{x64,x86,arm64}-portable.zip`, containing exactly one
  `<package>.exe` (optionally in a subdirectory).
- Linux: `<package>-<version>-linux-{x86_64,aarch64,riscv64}.AppImage` for a matching architecture.
- The same release must publish `SHA256SUMS.txt` with exactly one checksum entry for that asset.
- Release tags must consist of ASCII letters, digits, dots, underscores, hyphens or plus signs.
- Downloads are limited to 2 GiB; ZIP extraction to 20,000 entries and 4 GiB of expanded data.
  Paths escaping the installation, links, device names, and conflicting output paths are rejected.

MSI, DMG, deb/rpm, arbitrary installers, local executables, private repositories requiring
credentials, custom Git servers, prerelease channels and automatic community updates are outside
this first version. The web build explains that source management requires the desktop launcher.

## Storage and trust

Installations use `<launcher-data>/community/<repository-hash>/versions/<unique-version>/app/`.
A small atomically replaced `installed.json` selects the active version. Failed publication
leaves the previous receipt intact; older versions and any files created by those apps are kept.
There is no automatic cleanup or uninstall in this first version. Removing a source does not
remove its files, and the official installation namespace is never reused.

Portable markers are preserved. Portable settings stored beside an executable remain in that
version's directory; **this first version does not migrate those settings to a newer version**.
Publishers that honor HOME/APPDATA/LOCALAPPDATA/XDG environment overrides can keep settings across
updates in the persistent per-source `home/` directory supplied at launch. Windows Known Folder
APIs and applications that ignore those overrides can still use the normal user profile.

This is installation-directory separation, **not an application sandbox**. Community applications
run with the user's permissions and are not endorsed, audited or signature-verified by ArtCraft.
SHA256 checks detect a corrupt or substituted download relative to the publisher's checksum;
they do not establish the publisher's trustworthiness. Add only publishers you trust.

This implements the GitHub-source portion of issue #309; favorites, custom icons, reordering,
and shortcuts to arbitrary existing installations remain separate features.
