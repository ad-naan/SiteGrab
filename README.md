# sitegrab

> Mirror a website for offline browsing — one command.

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

Concurrently downloads HTML, CSS, JS, and images. Converts absolute links to relative paths for offline browsing. Automatically exports a ZIP archive. **Incremental** — re-run to only download changed files.

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
| `-h, --help` | — | Show help |
| `-V, --version` | — | Show version |

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
    ├── BFS crawl — concurrent workers (JoinSet + Semaphore)
    │   ├── HTML → parse links → rewrite → save
    │   ├── CSS/JS/IMG → download → save
    │   └── Hash → record in manifest
    │
    ├── Summary — pages, images, CSS, JS, size
    │
    └── ZIP — deflate-compressed archive
```

- **Language:** Rust — single binary, no runtime dependencies
- **Concurrency:** `tokio` `JoinSet` + bounded `Semaphore` (default 8 workers)
- **HTML parsing:** `scraper` (CSS selectors for `<a>`, `<img>`, `<link>`, `<script>`)
- **Link rewriting:** regex-based attribute replacement, skips anchors/javascript/mailto/external
- **Incremental:** SHA-256 hashes stored in `.sitegrab.json`, compared on re-run
- **ZIP:** `zip` crate with deflate compression

---

## Limitations

- **Client-rendered sites** (React, Vue SPA) — content loaded via JavaScript won't be captured because `sitegrab` doesn't execute JS. Works with WordPress, traditional HTML sites, and server-rendered frameworks (Next.js SSG/SSR, Nuxt, Hugo, Jekyll, etc.).
- **No `robots.txt`** compliance yet.
- **No cookie/auth** support.
- **No rate limiting** — use `-j` to reduce concurrency for polite crawling.

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
