---
title: The launcher does not open
description: What to try when ArtCraft Launcher does nothing, closes straight away or exits before its window appears, mostly on Windows.
order: 10
verified: 0.4.1
---

Install the [latest release](https://github.com/storytold/craft-launcher/releases/latest) first. Version 0.4.0 fixed the most common reason the launcher did not open on Windows. If it still does not open, start it with OpenGL. The steps are below.

## Symptoms

- You click the launcher and nothing happens. It does not appear in Task Manager, even for a moment.
- The window flashes and closes.
- Reinstalling, repairing and updating graphics drivers does not help.
- The portable version may start when the installed one does not, or the other way round.

If you start the launcher from a terminal, the error ends with `Parent device is lost`, or mentions `dxil.dll` or `DXC compile error`.

On a PC with Intel HD or UHD Graphics 500/600 series, the launcher can exit with code 101 and the error `Shader translation error ... (0x80070057)` ([issue #111](https://github.com/storytold/craft-launcher/issues/111)).

## Fix

Try these in order.

### 1. Install the latest version

1. Check your version. If the launcher opens at all, open **Settings**. The version is under the title.
2. If it is older than 0.4.0, or you cannot tell, download the installer from the [latest release](https://github.com/storytold/craft-launcher/releases/latest) and install it. See [Install ArtCraft Launcher](../getting-started/install.md).
3. Start the launcher again.

### 2. Windows: start with OpenGL

If the latest version still does not open, start it with OpenGL. The reporter of [issue #111](https://github.com/storytold/craft-launcher/issues/111) found this works on Intel HD/UHD 500/600 graphics with the `Shader translation error ... (0x80070057)` error. That issue is still open.

1. Open PowerShell.
2. Go to the folder with `artcraft-launcher.exe`. The installed copy is in `%LOCALAPPDATA%\Programs\ArtCraft Launcher`.
3. Run these two commands:

        $env:WGPU_BACKEND = 'gl'
        .\artcraft-launcher.exe

The launcher window should open. The setting only applies to that PowerShell window, so repeat it each time you start the launcher this way.

### macOS on a Mac patched with OpenCore Legacy Patcher

The launcher quits right after its window flashes, and the terminal says `invalid value 'metal3.2'`. Users reported that this command keeps it running on some Macs:

    SYSTEM_VERSION_COMPAT=1 /Applications/ArtCraft\ Launcher.app/Contents/MacOS/*

It did not help on one Mac with Intel Iris Pro graphics.

### Intel graphics on Windows

One user reported that updating the Intel Graphics driver fixed a crash with an error naming `igvk64.dll` in the Windows Event Viewer (Windows Logs, Application).

## Why this happens

On Windows the launcher draws with Direct3D 12. Before 0.4.0 it compiled its shaders with whatever copy of `dxcompiler.dll` Windows found. The launcher does not include one, so an old or unmatched copy made the shaders fail and the window never opened. Since 0.4.0 the launcher uses the shader compiler built into Windows instead ([pull request 173](https://github.com/storytold/craft-launcher/pull/173)).

Some graphics drivers still fail with Direct3D 12. OpenGL takes a different path, which is why it can help.

## Still stuck?

Ask in [Discord](https://discord.gg/artcraft) or open an issue in the [issue tracker](https://github.com/storytold/craft-launcher/issues). Include your launcher version, your system, your graphics card, and the error text if you can get it.

Since 0.4.0 the launcher writes a log, which helps too. On Windows it is `%LOCALAPPDATA%\ArtCraft Launcher\logs\artcraft-launcher.log` (in the portable version, `ArtCraftLauncherData\logs` next to the program). On macOS it is in `~/Library/Application Support/ArtCraft Launcher/logs`, on Linux in `~/.local/share/artcraft-launcher/logs`.
