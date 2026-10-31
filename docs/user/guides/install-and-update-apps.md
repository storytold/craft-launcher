---
title: Install and update Craft apps
description: Install a Craft app from the launcher, update it, uninstall it, and what "Not available here" means.
order: 10
verified: 0.4.1
---

Click **Install** on an app's card to install it. Click **Update to** followed by a version number to update it. Use the **…** menu on the card to uninstall.

## Install an app

1. Open the launcher. Each app has a card, or a row in the table view. Click a card to open the app's page, with a description and its features.
2. Find the app. The search box is labelled **Search apps**. The **Not installed** tab lists apps you can install.
3. Click **Install**.
4. Wait. The bar shows **Downloading**, then **Verifying checksum…**, then **Installing…**. The launcher checks every download against the release's published checksum before it installs it.
5. When it is done, the button changes to **Open**.

On Linux, your system may ask for your password. If another install is running, the bar says **Waiting for another app…** and starts when the other one finishes.

To stop a download, click the pause button or the cancel button next to the bar. A paused download shows **Resume**. **Discard download** throws it away.

If an install fails but the download is saved, the button reads **Retry install**. The launcher checks the saved file again and then installs it.

## Update an app

1. Open the launcher. Apps with a newer version show **Update available**, and the button reads **Update to** and the new version.
2. Click that button. To update every app at once, click **Update all** at the top, or **Update 1 app** when only one has an update.

The launcher updates an app with the same package format it was installed with. It never replaces an app that is running.

To have updates installed without asking, open **Settings** and tick **Automatically download and install updates**. The same section has **Check for updates**, which sets how often the launcher looks, and **Notify me when an installed app has an update**.

## Uninstall an app

1. Click the **…** button on the app's card.
2. Click **Uninstall…**.
3. Confirm with **Uninstall**.

Your documents and settings stay. On Windows, Windows removes the app like any installed program. On macOS the app goes to the Trash. On Linux, your system's package manager removes it and may ask for your password.

The same **…** menu has **Open**, a command to show the app in your file manager (**Show in Explorer** on Windows, **Show in Finder** on macOS, **Show in Files** on Linux), **Reinstall**, **Release notes**, **Website**, **GitHub repository** and **All releases**.

## Hide an app

To take an app you do not use off the list, click **Hide App** in its **…** menu. Hidden apps move to the **Hidden Apps** tab. Click **Unhide App** there to bring one back.

## What "Not available here" means

The card shows **Not available here** when the app is not installed and the launcher cannot offer it on this computer. There are two causes.

- The button reads **Not available**. The app has a release, but no download for your system.
- The button reads **Offline**. The launcher could not check the app's releases. Hover over the label to see the reason. If every app shows this, see [Every app shows "Offline"](../troubleshooting/apps-show-offline.md).

An app with no release yet shows **Coming soon**.

## Still stuck?

Ask in [Discord](https://discord.gg/artcraft) or open an issue in the [issue tracker](https://github.com/storytold/craft-launcher/issues). Say which system you use and which app you were installing.
