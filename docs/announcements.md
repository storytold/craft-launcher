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

## Publishing a notice

The file lives in storytold/artcraft-services at
`frontend/apps/artcraft-website-next/public/launcher-announcements.toml`. Nobody signs by hand:
the website's Cloudflare build signs it with `LAUNCHER_CATALOG_SIGNING_KEY` and deploys both
files. That build runs when a change is pushed to `main`. Publishing therefore means merging a PR.

1. On a branch of storytold/artcraft-services, add a row to `launcher-announcements.toml`. Use
   `[[announcements]]` for the bell, or `[[alerts]]` only for something every user must
   acknowledge (a dialog opens on their screen). Give it:
   - a new `id`. Never reuse an id, because read and dismissed marks are kept per id.
   - a `title` and a plain-text `body`. Neither is translated, so write them in English.
   - an optional https `link`.
   - a `published` time, in UTC with a `Z`. The bell sorts by this time, and rows without one
     come last. A future time holds the row back until then.
   - an `expires` for anything time-limited.
2. Check that the file parses. A row that breaks a rule is skipped without an error, so also read
   your row against the rules above.

   ```sh
   python3 -c "import tomllib; tomllib.load(open('launcher-announcements.toml','rb'))"
   ```

3. Open a PR, and merge it once it has been reviewed. The push to `main` triggers the Cloudflare
   deploy. The build fails rather than publish an unsigned file.
4. After the deploy, check that the live file is the one you merged and that it verifies against
   the key built into the launcher:

   ```sh
   curl -fsSO https://getartcraft.com/launcher-announcements.toml
   curl -fsSO https://getartcraft.com/launcher-announcements.toml.sig
   base64 -d launcher-announcements.toml.sig > notices.sig
   openssl pkeyutl -verify -pubin -rawin -sigfile notices.sig -in launcher-announcements.toml \
     -inkey crates/core/assets/launcher-catalog-public-key.pem
   ```

   Run the last command from a craft-launcher checkout. It prints `Signature Verified
   Successfully`.
5. Launchers pick the notice up at their next launch, or within six hours if they are already
   open. An announcement appears in the bell with an unread badge. An alert opens as a dialog.

To change or withdraw a notice, edit or remove its row and publish the same way. Do not change an
announcement's `id` to make users see it again: a new id is a new notice.
