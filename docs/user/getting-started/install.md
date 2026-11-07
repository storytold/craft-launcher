---
title: Install ArtCraft Launcher
description: Download ArtCraft Launcher for Windows, macOS or Linux from GitHub, install it, and find out where it puts your apps.
order: 10
verified: 0.4.1
---

Download the file for your system from the [latest release](https://github.com/storytold/craft-launcher/releases/latest) and install it. In the file names, `<version>` stands for the version number, for example `0.4.1`.

## Windows 11

1. Open the [latest release](https://github.com/storytold/craft-launcher/releases/latest).
2. Download `artcraft-launcher-<version>-windows-x64.msi`. It fits most PCs (Intel and AMD, 64-bit).
3. Run the file and follow the installer.
4. Start **ArtCraft Launcher** from the Start menu.

The release also has an `arm64` installer for Windows on Arm and an `x86` installer for 32-bit Windows. The `x64` installer installs for the current user and needs no admin rights. It puts the launcher in `%LOCALAPPDATA%\Programs\ArtCraft Launcher`.

To run it without installing, download `artcraft-launcher-<version>-windows-x64-portable.zip` instead. Unzip it and run `artcraft-launcher.exe`. It keeps its settings in an `ArtCraftLauncherData` folder next to the program.

## macOS (11 Big Sur or later)

1. Download `artcraft-launcher-<version>-macos-universal.dmg`. It works on Apple silicon and Intel Macs.
2. Open it and drag ArtCraft Launcher to Applications.
3. Open ArtCraft Launcher from Applications.

## Linux

Use a 64-bit system with glibc 2.35 or newer, such as Ubuntu 22.04, Debian 12 or Fedora 36. Pick one file from the release. Replace `x86_64` with `aarch64` on Arm.

| File                                                | How to use it                                                     |
|-----------------------------------------------------|-------------------------------------------------------------------|
| `artcraft-launcher-<version>-linux-x86_64.AppImage` | Run `chmod +x` on the file, then run it.                          |
| `artcraft-launcher-<version>-linux-x86_64.deb`      | `sudo apt install ./artcraft-launcher-<version>-linux-x86_64.deb` |
| `artcraft-launcher-<version>-linux-x86_64.rpm`      | `sudo dnf install ./artcraft-launcher-<version>-linux-x86_64.rpm` |

The release also has a Flatpak and a plain `.tar.gz`. The Flatpak can download apps but has limited power to install them on your computer, so use the AppImage, `.deb` or `.rpm` when you can. The tray icon shows on KDE, Xfce, Cinnamon and most other desktops. On GNOME, install the AppIndicator extension.

FreeBSD 14 and RISC-V Linux builds are also on the release page.

## First run

1. Open the launcher. It lists every app in the suite and checks each one for the latest release. Until it has an answer, an app shows **Checking…**.
2. Apps you already have on this computer show as installed. The launcher finds apps you installed by hand.
3. To install an app, see [Install and update Craft apps](../guides/install-and-update-apps.md).

## Where apps get installed

The launcher installs each app where your system normally keeps applications, using the app's official release package.

| System  | Where                                 | How                                                  |
|---------|---------------------------------------|------------------------------------------------------|
| macOS   | `/Applications/<App>.app`             | Copied from the release DMG                          |
| Windows | `C:\Program Files\<App>`              | The release MSI, through Windows Installer           |
| Linux   | `/usr/bin`, `/usr/share/applications` | The release `.deb` or `.rpm`, with a password prompt |
| FreeBSD | `/usr/local`                          | The release files, with a password prompt            |

On Linux, without those, the launcher uses a Flatpak, or an AppImage in `~/Applications`. On FreeBSD it installs to `~/.local` if polkit is not installed.

## Update the launcher

When a new version is out, the launcher shows a banner with **Update and restart** and **What's new**. **Update and restart** downloads the new version, replaces the old one and opens it again. You can also check by hand with the refresh button in the top bar, labelled **Check for updates**.

## Still stuck?

If the launcher does not open, see [The launcher does not open](../troubleshooting/launcher-does-not-open.md). Otherwise ask in [Discord](https://discord.gg/artcraft) or open an issue in the [issue tracker](https://github.com/storytold/craft-launcher/issues).
