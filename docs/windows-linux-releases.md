# Windows and Linux releases

The repository's **Release** workflow builds Windows x64 and Linux x86_64 packages
on GitHub-hosted runners. It works without a code-signing certificate.

## Start a release

1. Set `[workspace.package] version` in `Cargo.toml` to the desired release version
   and commit the matching `Cargo.lock` update (`cargo xtask version set X.Y.Z`).
2. Push the source to GitHub.
3. Open **Actions > Release > Run workflow**, choose the branch, and run it.
   The optional version input checks that the selected source has the expected
   version; it does not change the source version.

Alternatively, push a matching `vX.Y.Z` tag or push to the `release` branch.
A tag's version must match `Cargo.toml`.

After both builds succeed, the workflow creates a **draft** GitHub release with:

| Platform | Packages |
| --- | --- |
| Windows x64 | `.msi` installer and `-portable.zip` |
| Linux x86_64 | `.AppImage`, `.AppImage.zsync`, `.deb`, `.rpm`, `.tar.gz` |
| All downloads | `SHA256SUMS.txt` |

Review the draft under **Releases**, then publish it to make it available to
users and the launcher's update checker. A rerun may update a draft, but the
workflow refuses to replace an already published version. Bump the source version
for the next release. Versions containing a prerelease suffix, such as `-rc.1`,
create prerelease drafts.

## Optional Windows signing

Without signing secrets, the packaging script produces unsigned Windows packages.
Windows may display an unknown-publisher warning when users run them.

To enable signing, configure either of these sets of **repository Actions secrets**:

- `WINDOWS_CERTIFICATE` (base64-encoded PFX) and `WINDOWS_CERTIFICATE_PASSWORD`.
- `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`,
  `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, and `AZURE_CERT_PROFILE`.

These are the two signing methods currently implemented by
`packaging/windows/sign.ps1`. SignPath integration requires its own setup and is
not enabled merely by logging into SignPath.
