---
title: Every app shows "Offline" (UnknownIssuer)
description: Fix the launcher showing "Not available here" and "Offline" for every app with the error "invalid peer certificate: UnknownIssuer" on Windows.
order: 20
verified: 0.4.1
---

Install launcher version 0.2.0 or newer. If you cannot, open PowerShell, run `Invoke-WebRequest https://api.github.com`, and restart the launcher.

## Symptoms

- Every app shows **Not available here** and its button reads **Offline**.
- Hover over the **Not available here** label. It says `Couldn't check for updates: invalid peer certificate: UnknownIssuer`.
- Restarting, reinstalling and switching networks does not help.
- GitHub opens fine in your browser, and the launcher works on other computers.

This was reported on some Windows PCs.

## Fix

1. Check your launcher version. Open **Settings**. The version is under the title.
2. If it is older than 0.2.0, download the latest installer from the [releases page](https://github.com/storytold/craft-launcher/releases/latest) in your browser and install it. See [Install ArtCraft Launcher](../getting-started/install.md).
3. Start the launcher. The apps should show their status within a few seconds.

If you are on 0.2.0 or newer and still see the error, or you cannot install a new version, use this workaround:

1. Open PowerShell.
2. Run this command:

        Invoke-WebRequest https://api.github.com

3. Restart the launcher.

A contributor confirmed this workaround on an affected PC.

## Why this happens

Older versions of the launcher read a copy of the Windows list of trusted certificate authorities when they started. Windows only adds many of them to that list when a program asks it to check a connection. Chrome and Edge do their own checks, so a PC that mostly uses them can be missing the one GitHub needs. Antivirus programs that scan HTTPS traffic can cause the same error.

Since version 0.2.0 the launcher asks Windows to check each connection, and Windows fetches the missing certificate itself. The change was merged on 10 October 2026 ([pull request #43](https://github.com/storytold/craft-launcher/pull/43)) and is in releases 0.2.0 and newer, including 0.4.1. The author tested it on Linux only and not on an affected Windows PC, so whether it clears every case is not confirmed yet.

The command in the workaround makes Windows check GitHub's connection, which downloads the missing certificate into the store.

## Still stuck?

Ask in [Discord](https://discord.gg/artcraft) or open an issue in the [issue tracker](https://github.com/storytold/craft-launcher/issues). Include your launcher version, your Windows version, whether you use antivirus software that scans web traffic, and the text from the hover message.
