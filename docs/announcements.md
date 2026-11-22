# Announcements and alerts

The launcher shows notices only from a file on the website, signed like the hosted catalog
([adr/0002](adr/0002-notices-come-only-from-a-signed-website-file.md)):

- `https://getartcraft.com/launcher-announcements.toml`
- `https://getartcraft.com/launcher-announcements.toml.sig`: a base64 Ed25519 signature over the
  exact TOML bytes, made with the website build's `LAUNCHER_CATALOG_SIGNING_KEY`
  (see [hosted-catalog.md](hosted-catalog.md)).

Announcements are listed in the bell in the header; alerts open as a dialog the user must
acknowledge. A launcher that has never fetched a valid file shows nothing. One that has keeps its
verified copy: a missing file or a bad signature leaves the last good notices in place, so deleting
the file does not withdraw them. To withdraw notices, publish a signed file without them (a file
with no rows is valid); launchers drop them at their next successful fetch.

```toml
[[announcements]]
id = "anicraft-launch"            # letters, digits, - _ . (at most 80); never reuse an id
title = "AniCraft joins the Craft App Suite"
body = "Plain text, no markup."
link = "https://getartcraft.com/" # optional, https only
published = "2026-11-21T10:00:00Z" # optional; a future date waits until then
expires = "2026-12-21T00:00:00Z"   # optional; hidden from then on

[[alerts]]
id = "security-2026-11"
title = "Security update"
body = "Update PdfCraft to 1.4.2."
expires = "2026-12-01T00:00:00Z"
```

A row that breaks a rule (missing or bad id, unreadable date, no title and no body) is skipped;
the rest still show. At most 20 of each kind, 64 KiB in all, 4000 characters of text each.

The launcher fetches the file at launch and at most every six hours after, from the pinned host,
with redirects refused, and caches the verified copy. A signature proves who wrote the file, not
when, so give anything time-limited an `expires`: it bounds how long an old copy served again
could show it. An `expires` takes effect at that moment, also in a launcher that stays open.
Removing a row from the published, signed file removes the notice at the next successful fetch;
the user's read and dismissed marks for it are then forgotten.
