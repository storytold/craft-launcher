# Notices come only from a signed file on getartcraft.com

Announcements and alerts are text that every launcher puts on the user's screen, an alert in a dialog they must acknowledge, so the launcher shows them only from `launcher-announcements.toml` on getartcraft.com, signed with the same Ed25519 key as the hosted catalog and checked against the key built into the launcher. The storyteller release feed's `important_notices`, which the launcher read before, carried them unsigned behind HTTPS alone, and its app renames are no longer turned into announcements; renames are announced in the signed file when wanted.

## Considered options

- **Keep the storyteller feed**: already working and quick to publish, but anyone who can change that API's answer can put text and a link in front of every user, and the web build cannot fetch it.
- **A separate key for notices**: limits what a leaked notices key could do, but means two secrets in the website build and two keys to rotate in the launcher. The catalog key can already change everything users see about each app.

## Consequences

- Publishing an announcement means a change to the website and its build signing the file. There is no unsigned fallback: a launcher that never fetched a valid file shows no notices, and one that did keeps its last verified copy when the file goes missing or its signature fails. Notices are withdrawn by publishing a signed file without them, not by deleting the file.
- A signature proves content, not freshness: an old signed file could be served again. Each notice can carry a signed `expires`, which bounds how long a replayed one is shown.
- The file is fetched at launch and at most every six hours after, from the pinned host, with redirects refused and nothing identifying in the request.
