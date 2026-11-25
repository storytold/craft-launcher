# ArtCraft Launcher

How ArtCraft Launcher installs, updates and launches the Craft App Suite on each platform.

## Announcements

**Notice**:
A message from ArtCraft to every launcher user, signed by ArtCraft: either an announcement or an alert.
_Avoid_: notification, message

**Announcement**:
A notice the user reads when they choose to, in the bell.
_Avoid_: news item, notification

**Alert**:
A notice the user must acknowledge before carrying on.
_Avoid_: warning, popup

**Bell**:
The icon in the launcher's header that lists announcements and shows how many are unread.
_Avoid_: notification centre, inbox

**Unread**:
An announcement the user has not yet seen in the bell.

**Expired**:
A notice past the end date ArtCraft gave it; it is no longer shown.

**Dismissed**:
A notice the user has removed for good; it is not shown again.
_Avoid_: deleted, hidden

**Notification**:
A message shown by the operating system, outside the launcher's window. Never used for notices.

## Checking for releases

**Release feed**:
ArtCraft's own list of each app's latest stable release, asked before GitHub.
_Avoid_: release info, ArtCraft API

**Update check**:
One pass that finds the release each app should be on.
_Avoid_: poll, refresh, sweep

**Forced check**:
An update check the user asked for with "Check for updates".
_Avoid_: manual refresh

**Version track**:
What an app follows: the latest stable release, the nightly, or one pinned version.
_Avoid_: channel (that is Stable versus Developer Builds)

**Pinned version**:
A version track that stays on one exact release until the user changes it.
_Avoid_: locked version, kept version

**Pre-release**:
A beta or release candidate published before a stable release. Never a nightly.
_Avoid_: preview, nightly

**GitHub rate limit**:
GitHub's own ceiling on API requests from one network address, shared by everything on that network.
_Avoid_: quota

**GitHub budget**:
The ceiling one launcher sets itself on GitHub API requests per hour, below the GitHub rate limit.
_Avoid_: rate limit, allowance
