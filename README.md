# sitegrab

> Mirror any website — SPA or not — for offline browsing. One command.

```bash
sitegrab https://example.com

📄 Pages: 245
🖼  Images: 1,932
🎨 CSS: 17
📦 JS: 42
📁 Size: 380 MB

✓ Mirror completed
✓ Offline ready
✓ Zip exported: example.com.zip
```

Concurrently downloads HTML, CSS, JS, images, fonts, and media. **Auto-detects SPAs** (React, Vue, Angular, etc.) and can render them with a headless browser. Browser requests are classified by resource type, so static assets are mirrored while Next.js RSC, Fetch/XHR, API, analytics, and WebSocket traffic are excluded. Same-origin and cross-origin static links are rewritten for offline browsing. Automatically exports a ZIP archive. **Incremental** — re-run to only download changed files.

---

## Install

### One-liner

```bash
curl -sSL https://raw.githubusercontent.com/kerwin2046/SiteGrab/main/install.sh | bash
```

On Linux x86_64 the installer prefers the fully static **musl** build
(`sitegrab-linux-x86_64-musl.tar.gz`), which runs on any distribution
regardless of glibc version. It falls back to the glibc build (built on
Ubuntu 22.04 / glibc 2.35) and finally to building from source.

### From source

```bash
cargo install --git https://github.com/kerwin2046/SiteGrab --force
```

### Pre-built binary (manual)

```bash
# Linux x86_64 (static musl — works on any distro)
curl -sSL https://github.com/kerwin2046/SiteGrab/releases/latest/download/sitegrab-linux-x86_64-musl.tar.gz | tar xz
sudo mv sitegrab /usr/local/bin/

# macOS
curl -sSL https://github.com/kerwin2046/SiteGrab/releases/latest/download/sitegrab-macos-x86_64.tar.gz | tar xz
sudo mv sitegrab /usr/local/bin/

# Windows (PowerShell)
# Download sitegrab-windows-x86_64.zip from the Releases page and unzip
```

Requires Rust 1.75+ for source builds.

---

## Usage

### Mirror a site

```bash
sitegrab https://example.com
```

Creates `example.com/` directory with mirrored content + `example.com.zip`.

### Options

| Flag | Default | Description |
|---|---|---|
| `-o, --output <DIR>` | domain name | Output directory |
| `-j, --jobs <N>` | `8` | Concurrent downloads (must be ≥ 1) |
| `--no-zip` | — | Skip ZIP creation |
| `--fresh` | — | Force full re-download (ignore manifest) |
| `--robots` | — | Respect robots.txt |
| `--render <MODE>` | `auto` | SPA rendering: `auto` (detect), `on` (force), `off` (HTTP only) |
| `--wait <MS>` | `1500` | Settle time (ms) after page load for lazy/AJAX content |
| `--max-pages <N>` | `10000` | Stop enqueueing after this many HTML pages |
| `--max-bytes <N>` | `0` | Stop after this many downloaded bytes (`0` = unlimited) |
| `--no-sandbox` | — | Disable Chromium sandbox (containers/root only) |
| `-h, --help` | — | Show help |
| `-V, --version` | — | Show version |

### SPA rendering

`sitegrab` automatically detects whether a site is a client-rendered SPA
(React, Vue, Angular, SvelteKit, etc.) by checking framework markers and
content heuristics. When detected, it falls back to headless-browser
rendering and saves the rendered DOM.

The renderer records typed browser resources rather than treating every
network request as a page. Documents, stylesheets, scripts, images, fonts,
media, and manifests are eligible for mirroring; RSC (`?_rsc=`), Fetch/XHR,
API, analytics, EventSource, and WebSocket requests are ignored.

```bash
# Default — auto-detect and render SPAs
sitegrab https://my-react-spa.com

# Force headless-browser rendering for every page
sitegrab --render on https://example.com

# Plain HTTP crawling only (faster, no browser)
sitegrab --render off https://example.com

# Give lazy-loaded content more time (3 seconds)
sitegrab --wait 3000 https://example.com
```

### Incremental updates

```bash
# First run — full download
sitegrab https://example.com

# Second run — only new/changed files
sitegrab https://example.com
# → info: Found existing manifest — incremental mode

# Force re-download everything
sitegrab --fresh https://example.com
```

Rerunning the same URL re-crawls from the seed URL. Unchanged files are skipped via conditional requests (`ETag` / `Last-Modified`) when available, otherwise by comparing SHA-256 of the on-disk (rewritten) content stored in `<output-dir>/.sitegrab.json`. The manifest also stores canonical URL identity, original URLs, resource types, final paths, and page outlinks, so incremental discovery does not need to reverse-engineer already rewritten HTML.

### Custom output

```bash
sitegrab -o my-mirror https://example.com
sitegrab --no-zip https://example.com
```

---

## Output structure

```
example.com/
├── index.html
├── about/index.html
├── blog/post-1/index.html
├── css/style.css
├── js/app.js
├── images/logo.png
└── _external/
    ├── fonts.googleapis.com/css2@family=Roboto/index.html
    └── fonts.gstatic.com/s/roboto.woff2

example.com.zip      # ← auto-generated
```

Page URLs remove fragments, Next.js `_rsc`, and common tracking parameters
such as `ref`, `utm_*`, `gclid`, and `fbclid`. Semantic query parameters are
preserved and sorted. Asset queries are preserved for cache-busting, with
long queries shortened using a stable hash.

Links are rewritten for offline browsing:

| Original | After |
|---|---|
| `<a href="/about">` | `<a href="about/index.html">` |
| `<img src="/images/logo.png">` | `<img src="images/logo.png">` |
| `<link href="https://fonts.example/font.css">` | `<link href="_external/fonts.example/font.css">` |
| `<a href="https://other.com">` | unchanged (external) |
| `<a href="#section">` | unchanged (anchor) |

Cross-origin static resources are stored under `_external/<host>/...`.
External HTML navigation remains unchanged and is not recursively crawled.

---

## How it works

```
sitegrab <URL>
    │
    ├── SPA detection — heuristic check for framework markers
    │   ├── HTML pages → concurrent workers (JoinSet + Semaphore)
    │   ├── SPA? → headless browser render → save HTML
    │   └── otherwise → plain HTTP fetch → parse/rewrite → save
    │
    ├── Typed resources → filter dynamic requests → download static assets
    ├── URL canonicalization → deduplicate pages/assets
    ├── Cross-origin assets → _external/<host>/...
    ├── Manifest → canonical URL + original URL + type + path + outlinks
    ├── Strip offline-breakers — manifest links, SW scripts, crossorigin
    ├── Offline closure check — verify rewritten local references exist
    │
    ├── Summary — pages, images, CSS, JS, size
    │
    └── ZIP — deflate-compressed archive (skips `.sitegrab.json`)
```

- **Language:** Rust — single static binary for HTTP crawl mode
- **SPA detection:** checks `__NEXT_DATA__`, `__next_f`, `ng-version`, `__nuxt__` and more; falls back to heuristics (empty `<body>` + `#root`/`#app` div)
- **Rendering:** `chromiumoxide` headless Chrome/Chromium/Edge (default feature; requires a local browser, or use `--render off`)
- **Resource filtering:** CDP resource types separate static assets from RSC, Fetch/XHR, APIs, analytics, and WebSockets
- **Concurrency:** `tokio` `JoinSet` + bounded `Semaphore` (default 8 workers)
- **Compression:** gzip / brotli / deflate decoding for smaller transfers
- **HTML parsing:** `scraper` (CSS selectors for `<a>`, `<img>`, `<link>`, `<script>`)
- **Link rewriting:** HTML, CSS `url()` / `@import`, `srcset`, fonts, poster, and lazy-load attributes share the same path mapping
- **Incremental:** conditional GET plus SHA-256 and manifest-backed dependency recovery
- **Offline safety:** strips PWA manifests, service workers, modulepreload/script preloads, and `crossorigin` attributes for `file://` compatibility
- **Completion status:** `complete`, `partial`, or `failed`; incomplete mirrors return a non-zero exit code
- **ZIP:** `zip` crate with deflate compression

---

## Limitations

- **SPA rendering requires a local Chrome/Chromium/Edge** installation (or set `$CHROME`). When `--render off` is used, client-rendered content won't be captured.
- **No cookie/auth** support.
- **Use `--max-pages` / `--max-bytes` / `-j`** to bound large sites; there is no per-request rate limiter beyond concurrency.
- **`--no-sandbox`** disables Chromium's sandbox — only use in trusted containers.
- **Interactive / behind-login content** not captured (no auth flow).
- **SPA offline pages** strip `<script>` tags so frameworks don't wipe the rendered DOM without APIs.
- **Dynamic APIs are intentionally not mirrored.** The offline result is a static snapshot, not a working backend.
- **External HTML pages are not recursively crawled.** Only referenced static assets are stored under `_external/`.

### Completion status

- **Complete** — crawl and manifest save succeeded; prints `Offline ready`.
- **Partial** — one or more resources failed, or the manifest could not be saved; exits with code `2`.
- **Failed** — no usable mirror was produced; exits with code `1`.

Missing local references are reported by the offline closure check instead of
silently claiming the mirror is ready.

---

## Why not wget?

| | `wget --mirror ...` (6 flags) | `sitegrab` |
|---|---|---|
| Command | `wget --mirror --convert-links --adjust-extension --page-requisites --no-parent <URL>` | `sitegrab <URL>` |
| Concurrency | Single-threaded | 8 concurrent workers |
| Link conversion | Post-process (2x time) | On save (one pass) |
| Progress | Silent / verbose only | Spinner + status per file |
| ZIP | `zip -r out.zip out/` | Auto-generated |
| Incremental | `--mirror` re-downloads everything | SHA-256 skip unchanged files |

---

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo test --no-default-features
```

### Local testing

```bash
cargo build --release --all-features
./target/release/sitegrab https://example.com
```

Creates `example.com/` and `example.com.zip` in the current directory. Open `example.com/index.html` to verify offline browsing.

To exercise browser rendering and Next.js/RSC filtering:

```bash
./target/release/sitegrab "https://canopyseo.com/?ref=onepagelove" --render on
```

The output should not contain `_rsc` directories or a duplicate
`@ref=onepagelove` homepage. Referenced CDN assets such as Google Fonts should
appear under `canopyseo.com/_external/`.

License: MIT

