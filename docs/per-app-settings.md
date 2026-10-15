# Per-app versions and installation settings

Open an app's **⋮ → App settings…** menu in either grid or table view.

Table view keeps the Open and menu buttons visible when resizing; optional information
columns return as the window grows.

The **Version** tab loads published GitHub releases for that app. The dropdown labels the
installed and latest releases. Older pages load on demand, up to 600 releases. Drafts are
excluded; pre-releases are optional. Releases without compatible assets or verifiable
versions cannot be installed. Refresh retries a failed request without changing installations.

Transient server errors (500, 502, 503, 504), network timeouts and connection failures retry
automatically while App settings stays open. A countdown shows the next attempt: 5, 10,
20, 40, then 60 seconds between failures. GitHub's Retry-After or rate-limit reset takes
precedence when it requires a longer wait. Existing releases stay visible during failures;
the failed page is retried until it succeeds. Closing settings stops further requests.
Permanent request, permission, certificate and malformed-response errors require manual
action. Refresh remains available between attempts.

Selecting a version edits a draft. **Switch to …** explicitly installs it, checks its published
SHA-256, and confirms the installed version. **Keep this version after switching** prevents automatic
updates and **Update all** from replacing it. Preferences become active only after a successful
switch. To change the policy without reinstalling, select the installed release and use
**Save update preferences**. Following the latest release uses the global update settings.

Paused or failed version downloads retain their exact release across restarts. Resuming them
does not silently select the latest release. If a kept version's metadata is no longer loaded,
open its settings before reinstalling it.

The **Installation** tab shows the actual folder, opens it in the file manager, and offers the
existing uninstall confirmation. New Windows portable ZIP installations can use a chosen
parent folder. Windows ZIPs install under `<parent>/<app-id>`. Existing portable ZIP installations retain
their location and package format. AppImage updates continue to use the standard ~/Applications
folder; user-managed AppImages elsewhere are left alone.

## Package limits

| Format | Version changes | Custom folder |
| --- | --- | --- |
| Windows portable ZIP | Upgrade and downgrade; executable VERSIONINFO must match the selected release | New installations only |
| Windows MSI | Upgrade only; downgrade blocked | Installer-managed |
| macOS DMG | Existing bundle replacement and version checks | Existing application location |
| Linux AppImage | Replace verified download in ~/Applications | Standard folder |
| Linux DEB | Existing apt installation, including its downgrade support | System-managed |
| Linux RPM / Flatpak | Upgrade; downgrade blocked | System-managed |
| FreeBSD tarball | Existing installer path | Existing prefix |

## Launcher self-update

Open **Settings → Choose launcher version…** to select the launcher's own GitHub release.
The same release dropdown, pre-release filter, pagination and transient-error retry apply.
Selection alone does not install. **Switch to …** downloads and verifies the chosen release,
waits for other installations to finish, then replaces the launcher and restarts it. The
installation format and location stay unchanged.

**Keep this version after switching** also applies to the launcher. Its preference is staged
when the restart helper is scheduled, then committed only when the restarted launcher reports
the selected version. A failed switch retains the previous preference. Pinning the currently
installed release with **Save update preferences** prevents automatic launcher updates;
unchecking it returns to the global update policy. Older releases may not understand these
new preferences, so pinning cannot be guaranteed after downgrading into them.

Launcher MSI installations support upgrades only: the running
launcher cannot coordinate its own removal/recovery. Use a portable copy for launcher
downgrades. Portable ZIP, macOS app, Linux AppImage and DEB replacement support older releases;
RPM downgrades remain blocked. Development and externally managed copies cannot switch in place.

The global **Automatic updates** setting also updates the launcher unless its version is pinned.
Every installation requires a published checksum. Windows MSI self-update retains the existing
/passive progress window; quiet installation and MSI downgrade recovery are separate changes.

Windows portable replacement backs up recorded program files and rolls back on copy,
version-verification or manifest-save errors. Unrecorded settings and documents are preserved,
including during uninstall. The launcher permits host directory links but rejects links/junctions inside its owned folder, unsafe ZIP paths,
duplicate names and collisions with unrecorded files. Failed rollback retains recovery files
and reports their location. External installers and power loss have platform-specific limits;
this feature does not promise transactional rollback for them.

Tests cover version selection, pinning, resumed URL validation and portable rollback without
changing installed software. A real MSI uninstall/install cycle requires a disposable Windows
environment. macOS and FreeBSD installers still require native validation. The browser keeps
its download-only interface; native settings and install actions remain desktop-only.

## Validation

Run `cargo xtask ci`. Set `RUST_TEST_THREADS=1` for serial UI tests and
`ARTCRAFT_LAUNCHER_SNAPSHOTS=<existing-folder>` to export settings screenshots.
