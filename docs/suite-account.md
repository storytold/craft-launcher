# The suite account store

The contract every Craft App reads. The code is `crates/suite-account`
(`artcraft-suite-account`). This page is what another implementation has to match.

## Why there is one

GridCraft kept its session in `%APPDATA%\GridCraft\collabcraft-session.json`. Taken across the
whole suite that becomes a sign-in per application. It also becomes a copy of a bearer token on disk
per application. Every one of those is a separate place to revoke.
The launcher is the obvious place for a shared store. Somebody signs in there once. Every Craft App
then acts as them.

The store has two halves, kept deliberately apart. The list of accounts is a small JSON file in one
suite-level folder. It names who has signed in and at which deployment. It holds no credential. The
token lives in the operating system's keychain.

## Where the folder is

| Platform | Folder |
|---|---|
| Windows | `%APPDATA%\ArtCraft\` |
| macOS | `~/Library/Application Support/ArtCraft/` |
| Linux, the BSDs | `~/.config/artcraft/` |

That is `dirs::config_dir()` joined with `ArtCraft` on Windows and macOS. Elsewhere it joins with
`artcraft`. The launcher already spells its own folder that way per platform.

The folder is suite-level because an account belongs to the person. It does not belong to one
application. `craft-settings` gives each application `<platform config dir>/<app id>`, the right
home for that application's own preferences. An account placed under one of those would make every
other application read a folder it does not own. Removing that application would then sign everybody
out. The launcher's own folder fails for the same reason. An application must not have to know the
launcher exists.

It is the configuration folder. The record then follows a Windows profile the way somebody would
expect. The per-application file this replaces lived in `%APPDATA%` too. Nothing secret is in the
folder.

Rules for choosing it, first match winning:

1. `ARTCRAFT_SUITE_DIR` set and not empty: that folder.
2. `ARTCRAFT_SUITE_DIR` set and empty: nothing is read or written. One process can opt out entirely.
3. A `portable.txt` or `portable` file beside the executable: `ArtCraftSuite/` beside it. Each
   portable install therefore keeps its own. Point several at one folder with `ARTCRAFT_SUITE_DIR`.
4. The platform folder above.
5. No per-user configuration folder on this system: nothing is read or written. A file in the
   current directory would be the wrong home for a record of who is signed in.

## The file

`accounts.json` in that folder, written through a temporary file and a rename. Mode `0600` where the
platform has file modes.

```json
{
  "format": 1,
  "accounts": [
    {
      "id": "3f2a91c47e5b08d6a1c4f072",
      "service": "https://craft.example.com",
      "user": "a7f3c1e2-0000-4000-8000-000000000001",
      "display": "Ada Lovelace",
      "email": "ada@example.com",
      "added": 1760000000,
      "last_used": 1760003600
    }
  ],
  "active": "3f2a91c47e5b08d6a1c4f072"
}
```

* `format` is `1`. A reader that finds a higher number refuses the file. It never guesses.
* `service` is the CollabCraft deployment's base URL with no trailing slash.
* `user` is the deployment's own identifier for the account: the OIDC subject, never an address.
* `id` is the account handle. It is also the keychain entry's user name. It is the first 12 bytes of
  `SHA-256(service + "\n" + user)` in lower-case hex, giving 24 characters. Deriving it keeps one
  account on one keychain entry. A second sign-in as the same person at the same deployment then
  replaces the entry. No stale entry is left behind. The handle is also opaque. A credential manager
  window puts the handle on screen. Nobody's address belongs in that list.
* `active` names the account an application uses when it has no reason to prefer another. An `active`
  naming an account that is gone reads as "nobody is signed in".

## The keychain entry

```text
service  ai.storyteller.artcraft.suite-account
user     the account handle above
secret   the CollabCraft session token, as returned by POST /auth/device/poll
```

One service name covers the whole suite. Every entry the suite ever writes is therefore findable and
removable in one place.

| Platform | What it is |
|---|---|
| Windows | A generic credential whose target name is `<handle>.ai.storyteller.artcraft.suite-account`. Persistence `Local`, so a token minted for this machine stays on it. |
| macOS | A generic password in the user's login keychain. |
| Linux, the BSDs | A Secret Service item in the user's default collection, locked with the login session. |

Each is scoped to the one user by the platform itself. Each is removable one entry at a time from the
platform's own window. Removing one reads back as "nobody is signed in".

The crate uses [`keyring`](https://crates.io/crates/keyring) 4 with its default `v1` feature. That
feature picks the right store per platform. It also pins `crypto-rust` for the Secret Service. That
keeps OpenSSL and any C toolchain out of the build.

## What an application does

```rust
use artcraft_suite_account::{Shared, Store};

let store = Store::open();

// Once, for an application upgrading from its own session file. The file is removed only after the
// suite store holds the session. It is left alone where the suite store can keep nothing.
let _ = artcraft_suite_account::migrate::adopt(&store, &own_session_path, now);

match store.shared() {
    Shared::Session { account, token } => { /* bearer against account.service */ }
    Shared::None => { /* offer this application's own sign-in, exactly as before */ }
    Shared::Unreadable { account, why } => { /* offer a sign-in, and say why */ }
}
```

Nothing here is mandatory. A machine with no keychain, a locked keychain, a missing folder and an
empty suite all answer `Shared::None` or `Shared::Unreadable`. An application then behaves exactly as
it did before this crate existed. None of those answers is an error that stops it starting.

## Revocation

Signing out goes through `Store::sign_out`. It tells the deployment first. It then clears the
keychain entry and the row. `POST /auth/logout` drops every session the deployment holds for that
user. The one action therefore covers the suite and any other machine the person left a session on.

A deployment that cannot be reached does not block the local half. The entry still goes. The answer
is then `SignedOut::LocalOnly`. The caller can say the server was not told.

## Handling the token

No token value is logged, formatted into a message, or put on a command line. `Secret` has no
`Display` and no `Serialize`. Its `Debug` reports the length alone. Its bytes are overwritten when it
is dropped. `Secret::expose` is the one way out. The only caller that needs it is the code building
an `Authorization` header.
