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

Concurrently downloads HTML, CSS, JS, and images. **Auto-detects SPAs** (React, Vue, Angular, etc.) and renders them with a headless browser so client-side content is fully captured. Converts absolute links to relative paths for offline browsing. Automatically strips PWA artifacts (manifests, service workers, `crossorigin` attributes) that break `file://` access. Automatically exports a ZIP archive. **Incremental** — re-run to only download changed files.

---

## Install

### One-liner

```bash
curl -sSL https://raw.githubusercontent.com/kerwin2046/SiteGrab/main/install.sh | bash
```

### From source

```bash
cargo install --git https://github.com/kerwin2046/SiteGrab
```
### Pre-built binary (manual)

```bash
# Linux / macOS
curl -sSL https://github.com/kerwin2046/SiteGrab/releases/latest/download/sitegrab-linux-x86_64.tar.gz | tar xz
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
| `-j, --jobs <N>` | `8` | Concurrent downloads |
| `--no-zip` | — | Skip ZIP creation |
| `--fresh` | — | Force full re-download (ignore manifest) |
| `--robots` | — | Respect robots.txt |
| `--render <MODE>` | `auto` | SPA rendering: `auto` (detect), `on` (force), `off` (HTTP only) |
| `--wait <MS>` | `1500` | Settle time (ms) after page load for lazy/AJAX content |
| `-h, --help` | — | Show help |
| `-V, --version` | — | Show version |

### SPA rendering

`sitegrab` automatically detects whether a site is a client-rendered SPA
(React, Vue, Angular, SvelteKit, etc.) by checking framework markers and
content heuristics. When detected, it falls back to headless-browser
rendering so all dynamically loaded content is captured.

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

Rerunning the same URL detects previously downloaded files via SHA-256 hashes. Unchanged files are skipped. The manifest is stored in `<output-dir>/.sitegrab.json`.

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
└── images/logo.png

example.com.zip      # ← auto-generated
```

Links are rewritten for offline browsing:

| Original | After |
|---|---|
| `<a href="/about">` | `<a href="about/index.html">` |
| `<img src="/images/logo.png">` | `<img src="images/logo.png">` |
| `<a href="https://other.com">` | unchanged (external) |
| `<a href="#section">` | unchanged (anchor) |

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
    ├── CSS/JS/IMG → download → save
    ├── Hash → record in manifest (incremental)
    ├── Strip offline-breakers — manifest links, SW scripts, crossorigin
    │
    ├── Summary — pages, images, CSS, JS, size
    │
    └── ZIP — deflate-compressed archive (skips `.sitegrab.json`)
```

- **Language:** Rust — single binary, no runtime dependencies
- **SPA detection:** checks `__NEXT_DATA__`, `ng-version`, `__nuxt__` and more; falls back to heuristics (empty `<body>` + `#root`/`#app` div)
- **Rendering:** `chromiumoxide` headless Chrome (enabled by default, can be disabled with `--render off`)
- **Concurrency:** `tokio` `JoinSet` + bounded `Semaphore` (default 8 workers)
- **Compression:** gzip / brotli / deflate decoding for smaller transfers
- **HTML parsing:** `scraper` (CSS selectors for `<a>`, `<img>`, `<link>`, `<script>`)
- **Link rewriting:** regex-based attribute replacement, skips anchors/javascript/mailto/external
- **Incremental:** SHA-256 hashes stored in `.sitegrab.json`, compared on re-run
- **Offline safety:** strips PWA manifests, service workers, modulepreload/script preloads, and `crossorigin` attributes for `file://` compatibility
- **ZIP:** `zip` crate with deflate compression

---

## Limitations

- **SPA rendering requires a local Chrome/Chromium** installation. When `--render off` is used, client-rendered content won't be captured.
- **No cookie/auth** support.
- **No rate limiting** — use `-j` to reduce concurrency for polite crawling.
- **Interactive / behind-login content** not captured (no auth flow).

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
cargo test
cargo build --release
```

License: MIT
