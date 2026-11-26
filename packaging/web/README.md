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
files on GitHub. Release data comes from ArtCraft's release feed
(`https://api.storyteller.ai/v1/craft_apps/release_info`), in one request; the page needs no
backend of its own. The feed answers cross-origin requests from `getartcraft.com`: a copy hosted
on another domain needs that origin allowed by the feed's server.

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
- **Content Security Policy:** if you send one, allow `connect-src https://api.storyteller.ai` (and
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
- Don't send `X-Frame-Options: DENY` or a `frame-ancestors` CSP that excludes the embedding page.
