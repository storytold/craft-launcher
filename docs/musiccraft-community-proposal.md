# MusicCraft community application proposal

Related request: [#1247](https://github.com/storytold/craft-launcher/issues/1247).
This document requests maintainer consideration; it does not approve a new
repository, catalog entry, installer or download source.

## Publisher and review material

MusicCraft is independently maintained by **@ckiikc** in
[ckiikc/musiccraft](https://github.com/ckiikc/musiccraft). The repository was
created on 2026-11-05 and has a short public history beginning with a sanitized
0.10.37 source snapshot. It is not an official Craft App Suite application.
Its own source is MIT; third-party notices are retained.

- [Desktop 0.10.49 experimental release](https://github.com/ckiikc/musiccraft/releases/tag/v0.10.49): Apple silicon macOS ZIP and `SHA256SUMS.txt`.
- [Android 0.3.0 test release](https://github.com/ckiikc/musiccraft/releases/tag/android-v0.3.0): APK and `SHA256SUMS.txt`.
- [Public source](https://github.com/ckiikc/musiccraft), including the separate `android/` project.
- [Validation](https://github.com/ckiikc/musiccraft/blob/main/docs/VALIDATION.md), [release limitations](https://github.com/ckiikc/musiccraft/blob/main/docs/RELEASING.md) and [third-party components](https://github.com/ckiikc/musiccraft/blob/main/THIRD_PARTY.md).

**Distribution status:** macOS uses an ad-hoc signature, with no Developer ID
team and no notarization. The Android APK is a locally signed debug/test build,
not a Play Store release or a long-term signing channel. These are review/test
packages, not approved installation sources for ArtCraft Launcher.

## Scope

The desktop is a native Rust/egui listening application for local folders,
mounted NAS libraries, FTP/FTPS and the MusicCraft Android companion. It has
faceted browsing, ordered file-browser opening, incremental sync, progressive
network playback, selectable shared/exclusive CoreAudio output, persistent
player visuals and native settings/artwork windows. The interface and guide
have 23 languages; native-language editorial review remains pending.

The Android companion is a separate native Kotlin/Media3 application: local
library, background playback, media browsing for Android Auto, distinct search,
playlist and standalone portrait/landscape player views. Saved and dynamic
playlists use local metadata, ratings and listening history. A user-started
read-only HTTPS music server advertises over NSD/Bonjour. Pairing uses four
short-lived digits and explicit phone approval; the desktop verifies the
phone identity and stores the credential in Keychain. Sharing pauses while
the phone plays audio. No separate FTP application is required.

## Validation and privacy

The public desktop tree passed 197 ordinary tests, Clippy with warnings denied,
formatting, twelve synthetic loopback FTP integrations, Mach-O identity and
document-association tests. The public Android tree passed its debug build,
eleven unit tests and lint with zero errors. Native emulator checks cover
playback while rotating the Player, automatic advance, dynamic queue handling
and paused restoration after process death. The validation document identifies
which native checks preceded the publication snapshot.

Public sources and packages exclude user libraries, configured endpoints,
credentials, signing keys, local SDK paths and raw diagnostic outputs. Each
phone generates its own identity and credentials. Only hash-pinned synthetic
audio fixtures belong to the instrumentation tests; the normal APK contains
no test music. The standard desktop package excludes the optional LMS runtime.

Hardware bit-perfect output, acoustic gaplessness, Intel/other desktop systems
and automobile/DHU integration are not certified by these source tests.

## Requested maintainer decision

Would MusicCraft's listening workflow fit the community-app catalog, and would
source review be useful while distribution signing is addressed? Android is
presented as a companion project for review, not an Android installation target
for the current desktop launcher.

Any later installer/catalog change must follow the launcher's existing
repository approval, checksum and signing-team requirements. This proposal
changes no application code, allowlist, installation behavior or trust rule.
