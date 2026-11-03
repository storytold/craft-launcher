# Hosting ArtCraft Launcher for the web

`artcraft-launcher-web-<version>.zip` (from the GitHub release, or `packaging/web/package.sh`)
holds a static site in `artcraft-launcher-web-<version>/`:

| File | What it is |
|---|---|
| `index.html` | The page. It loads everything through relative URLs. |
| `artcraft-launcher-web-<hash>.js` | wasm-bindgen glue (generated, ES module) |
| `artcraft-launcher-web-<hash>_bg.wasm` | The app (egui, built with the `wasm-release` Cargo profile) |
| `_headers`, `.htaccess` | Sample header rules for Netlify/Cloudflare Pages and Apache |

There is no server-side code and no hand-written JavaScript. Upload the folder's contents anywhere
that serves static files.

## What the web version does

In the browser the launcher can't install anything: it shows the Craft App Suite catalog, the
latest release of each app and the right download for the visitor's platform, and links to the
files on GitHub. Release data comes straight from the GitHub REST API (`api.github.com` allows
cross-origin requests), so the page needs no backend. Unauthenticated visitors share GitHub's
limit of 60 API requests per hour per IP address.

## Any path works

All URLs in `index.html` are relative (`public_url = "./"` in `apps/artcraft-launcher-web/Trunk.toml`),
so the site works at a domain root (`https://example.com/`), under a prefix
(`https://example.com/apps/launcher/`) and from a CDN bucket. The asset names carry a content
hash, so they can be cached forever. Only `index.html` needs revalidation.

`packaging/web/package.sh` fails if the wasm grows past 24 MiB (Cloudflare Pages and Workers
reject any single file over 25 MiB).

## Required server settings

- **MIME type:** serve `.wasm` as `application/wasm`. Browsers refuse to stream-compile it under
  any other type. Serve `.js` as `text/javascript`. Most hosts already do both. For nginx, check
  that `mime.types` has `application/wasm wasm;`.
- **Compression:** turn on gzip or Brotli for `.wasm`, `.js` and `.html`.
- **Caching:** `Cache-Control: public, max-age=31536000, immutable` on the hashed `.wasm` and
  `.js` files, and `no-cache` on `index.html`.
- **Content Security Policy:** if you send one, allow `connect-src https://api.github.com` (and
  `https://raw.githubusercontent.com` for the app icons) plus `'wasm-unsafe-eval'` in `script-src`.
- **No special isolation headers:** the app doesn't use `SharedArrayBuffer`, so it doesn't need
  `Cross-Origin-Opener-Policy` or `Cross-Origin-Embedder-Policy`.

nginx example:

```nginx
location /launcher/ {
    types { application/wasm wasm; text/javascript js; text/html html; }
    gzip on;
    gzip_types application/wasm text/javascript text/html;
    location ~* \.(wasm|js)$ { add_header Cache-Control "public, max-age=31536000, immutable"; }
    location ~* index\.html$ { add_header Cache-Control "no-cache"; }
}
```

Local test: `python3 -m http.server 8765` inside the folder, then open http://localhost:8765/.

## Opening self-hosted web apps (optional)

A site that also hosts some Craft apps' web builds can make the launcher open them instead of
offering a download. Put a `craft-links.json` next to `index.html`, mapping app ids to addresses:

```json
{
  "photocraft": "https://example.com/apps/photocraft/",
  "wordcraft": "{proto}//{host}:3108/"
}
```

- An app listed there gets **Open** as its main button, which opens the address in a new tab;
  its downloads stay in the `⋯` menu. Apps not listed keep the download button.
- `{proto}` (`https:`), `{host}` (host name, no port) and `{app}` (the app id) are filled in from
  the page's own address.
- `https://` and `http://` addresses are accepted (home servers often have no certificate); other
  schemes, credentials in the address and shell metacharacters are refused.
- An address that fails that check is ignored, so the app keeps its download button.
- Without the file (or if it can't be read) nothing changes. Serve it with `Cache-Control:
  no-cache` so edits show up.

## Embedding in a page (iframe)

```html
<iframe
  src="https://example.com/launcher/"
  title="ArtCraft Launcher"
  style="width: 100%; height: 640px; border: 0;"
  allow="fullscreen"
  allowfullscreen>
</iframe>
```

- The app fills the iframe and follows its size, so size the iframe and not the app.
- **Sandboxed iframes** need at least `sandbox="allow-scripts allow-same-origin allow-popups"`;
  `allow-popups` lets download links open.
- With `craft-links.json`, **Open** starts the app in a new tab, and a tab opened from a sandboxed
  iframe inherits the sandbox: the app could lose downloads, form submission and more. When the
  linked apps are your own (trusted) sites, add `allow-popups-to-escape-sandbox` so they open
  unrestricted; otherwise expect them to run with the iframe's restrictions.
- Don't send `X-Frame-Options: DENY` or a `frame-ancestors` CSP that excludes the embedding page.
