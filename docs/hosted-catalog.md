# Signed hosted catalog

The launcher bundles `crates/core/catalog.toml`, containing the website's
`launcher-catalog.toml` data from storytold/artcraft-services#205 with the latest
compiled display order. The signed website fixture is kept separately under
`crates/core/tests/fixtures/` with byte-preserving Git attributes for Windows. The core build script
generates the existing static app/details types from it, keeping first launch
and offline operation independent of the website.

On the first release check of each process, desktop and web fetch:

- `https://getartcraft.com/launcher-catalog.toml`
- `https://getartcraft.com/launcher-catalog.toml.sig`

The signature is base64 encoding of a 64-byte Ed25519 signature over the exact
TOML bytes, with optional surrounding whitespace. Verification uses the SPKI
PEM public key in `crates/core/assets/launcher-catalog-public-key.pem` before
parsing. The corresponding private key belongs only in the website's Cloudflare
build secret `LAUNCHER_CATALOG_SIGNING_KEY`. It is never read or bundled by the
launcher. The checked-in signature fixture is public test data, not a secret.

Desktop stores the TOML and signature together in an atomic
`launcher-catalog.json` beside the release-feed cache. Web uses the browser's
local storage (`artcraft-launcher.signed-catalog.v1`). Every cache read verifies
the signature again. Network failures, missing signatures, invalid signatures,
invalid documents or unavailable storage preserve the verified cache, then the
bundled list. The bundled list is visible while the background check runs.

The active catalog is immutable for the process lifetime. Existing install jobs
keep their static handles; restarting refreshes the catalog again. Only one
bounded snapshot is interned per process. This avoids accumulating leaked strings
on repeated checks. Missing bundled apps are appended to the hosted list so an
installed copy remains manageable after the hosted entry disappears.

The signed file updates app copy, colors, details, desktop categories, window
class, icons and display order. Invalid rows are skipped, unknown fields ignored,
and a document with no valid rows is rejected. New official apps can be added
without a launcher release, within the existing source policy: a lowercase safe
ID, its matching `storytold/<id>` repository, ArtCraft publisher/team, default
bundle ID and no legacy aliases. Existing app names, aliases, bundle IDs,
repositories, publishers and macOS signing teams stay pinned to the bundled
entry. A signature cannot add an unapproved community app or change an existing
install identity. Community onboarding and identity changes still require
maintainer review and a launcher release.

Icons must use the website's app-ID/hash filenames and pass SHA-256 and PNG
size/dimension checks before decoding. Desktop caches them by hash; web uses
its HTTP cache. Missing icons do not block a valid catalog. Screenshots remain
JPEGs at the launcher's pinned assets tag, with existing byte/pixel checks.
The currently supported installer code still derives package/binary IDs from
app IDs; arbitrary installer identities in unknown manifest fields do not
change installation behavior.

Limits: 1 MiB catalog, 128-byte encoded signature, 64 hosted apps, 16 features
and screenshots per app, 256 KiB/512×512 PNG icons. Desktop HTTP uses its existing
bounded reader and timeouts. Browser catalog/icon requests stream with actual
byte limits and a 20-second abort signal; redirects are refused on both platforms.

This is a single pinned key. Rotation requires a launcher update; there is no
manifest-authorized next key, signed expiry or rollback counter in this schema.
The signature authenticates content, not freshness. The existing package checksum
and macOS/Windows publisher checks continue to apply to installations.

Validation includes a real signed website-format fixture, wrong-key/tampering
rejection, malformed rows, install-identity restrictions, new official app policy,
icon hashes/dimensions, and reverified cache fallback. The private signing key is
not required to run any test.

To validate the published catalog through the desktop HTTP/cache/activation path
without installing any app:

```sh
cargo test -p artcraft-launcher-native live_catalog_is_verified_cached_and_activated -- --ignored
```

This opt-in network test uses temporary storage and checks the pinned signature,
all accepted entries, activation and icon hashes. Normal CI stays offline.
