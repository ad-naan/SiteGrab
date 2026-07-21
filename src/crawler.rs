use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{Client, StatusCode};
use scraper::{Html, Selector};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use url::Url;

use crate::manifest::Manifest;
use crate::pathmap;
use crate::rewriter;
use crate::util::format_bytes;

/// Crawl statistics
pub struct Stats {
    pub pages: usize,
    pub images: usize,
    pub css: usize,
    pub js: usize,
    pub total_bytes: u64,
    pub errors: usize,
}

/// Soft caps for a crawl run.
#[derive(Clone, Copy, Debug)]
pub struct CrawlLimits {
    pub max_pages: usize,
    /// 0 means unlimited.
    pub max_bytes: u64,
}

impl Default for CrawlLimits {
    fn default() -> Self {
        Self {
            max_pages: 10_000,
            max_bytes: 0,
        }
    }
}

impl CrawlLimits {
    fn reached(&self, stats: &AtomicStats) -> bool {
        if stats.pages.load(Ordering::Relaxed) >= self.max_pages {
            return true;
        }
        if self.max_bytes > 0 && stats.total_bytes.load(Ordering::Relaxed) >= self.max_bytes {
            return true;
        }
        false
    }
}

/// URL classification
#[derive(Clone, Copy, PartialEq)]
enum ResourceType {
    Page,
    Css,
    Js,
    Image,
    Other,
}

/// Result from processing one URL
pub(crate) struct ProcessResult {
    rtype: ResourceType,
    url: String,
    save_path: String,
    new_urls: Vec<Url>,
    /// Final on-disk bytes (for manifest hashing).
    bytes: Vec<u8>,
    mtime: Option<String>,
    etag: Option<String>,
    /// Content was reused via 304 or local hash match (no re-write).
    not_modified: bool,
}

/// Optional validators from a previous manifest entry.
#[derive(Clone, Default)]
pub(crate) struct PriorState {
    pub etag: Option<String>,
    pub mtime: Option<String>,
    pub local_fresh: bool,
    pub rtype: Option<String>,
    pub rel_path: Option<String>,
}

/// Determine resource type from content-type header and URL path
fn classify(content_type: Option<&str>, url: &Url) -> ResourceType {
    let path = url.path().to_lowercase();

    if let Some(ct) = content_type {
        let ct = ct.split(';').next().unwrap_or(ct).trim();
        return match ct {
            "text/html" => ResourceType::Page,
            "text/css" => ResourceType::Css,
            "application/javascript" | "text/javascript" | "application/x-javascript" => {
                ResourceType::Js
            }
            "image/svg+xml" => ResourceType::Image,
            _ if ct.starts_with("image/") => ResourceType::Image,
            _ if ct.starts_with("font/")
                || ct.contains("font")
                || ct == "application/x-font-woff" =>
            {
                ResourceType::Other
            }
            _ => {
                // fallback to extension
                classify_by_ext(&path)
            }
        };
    }

    classify_by_ext(&path)
}

fn classify_by_ext(path: &str) -> ResourceType {
    if path.ends_with(".html")
        || path.ends_with(".htm")
        || path.ends_with(".php")
        || path.ends_with("/")
        || path.is_empty()
        || !path.rsplit('/').next().unwrap_or("").contains('.')
    {
        return ResourceType::Page;
    }
    if path.ends_with(".css") {
        return ResourceType::Css;
    }
    if path.ends_with(".js") || path.ends_with(".mjs") {
        return ResourceType::Js;
    }
    if path.ends_with(".png")
        || path.ends_with(".jpg")
        || path.ends_with(".jpeg")
        || path.ends_with(".gif")
        || path.ends_with(".svg")
        || path.ends_with(".webp")
        || path.ends_with(".ico")
        || path.ends_with(".avif")
    {
        return ResourceType::Image;
    }
    ResourceType::Other
}

/// Convert URL to filesystem path under `output_base` (traversal-safe).
fn url_to_path(url: &Url, output_base: &str) -> PathBuf {
    pathmap::url_to_path(url, output_base)
}

/// Resolve a potentially relative URL, skipping non-HTTP(S) protocols
pub(crate) fn resolve_url(base: &Url, href: &str) -> Option<Url> {
    let href = href.trim();
    if href.starts_with('#')
        || href.starts_with("javascript:")
        || href.starts_with("mailto:")
        || href.starts_with("tel:")
        || href.starts_with("data:")
        || href.starts_with("blob:")
        || href.is_empty()
    {
        return None;
    }
    base.join(href).ok().filter(|u| u.scheme() == "http" || u.scheme() == "https")
}

/// Extract same-domain URLs from an HTML document.
///
/// Handles:
///   - Regular `href`/`src` attributes
///   - `srcset` multi-URL attributes
///   - Lazy-load `data-src`, `data-lazy-src` attributes
///   - `<base href>` resolution
fn extract_urls(doc: &Html, page_url: &Url, base_host: &str) -> Vec<Url> {
    let mut urls = Vec::new();

    // Resolve <base href> for relative URLs.
    let base_url = {
        let base_sel = Selector::parse("base[href]").ok();
        let base_href = base_sel
            .and_then(|sel| doc.select(&sel).next())
            .and_then(|el| el.value().attr("href"))
            .and_then(|href| page_url.join(href).ok());

        base_href.unwrap_or_else(|| page_url.clone())
    };

    let pairs = [
        ("a[href]", "href"),
        ("link[href]", "href"),
        ("area[href]", "href"),
        ("script[src]", "src"),
        ("img[src]", "src"),
        ("source[src]", "src"),
        ("video[src]", "src"),
        ("audio[src]", "src"),
        // Lazy-load attributes
        ("img[data-src]", "data-src"),
        ("img[data-lazy-src]", "data-lazy-src"),
        ("source[data-src]", "data-src"),
        ("source[data-lazy-src]", "data-lazy-src"),
        ("video[poster]", "poster"),
    ];

    for (sel_str, attr) in &pairs {
        if let Ok(sel) = Selector::parse(sel_str) {
            for elem in doc.select(&sel) {
                if let Some(val) = elem.value().attr(attr) {
                    if let Some(abs_url) = resolve_url(&base_url, val) {
                        if is_same_domain(&abs_url, base_host) {
                            urls.push(abs_url);
                        }
                    }
                }
            }
        }
    }

    // Parse srcset attributes (img[srcset], source[srcset])
    for sel_str in &["img[srcset]", "source[srcset]"] {
        if let Ok(sel) = Selector::parse(sel_str) {
            for elem in doc.select(&sel) {
                if let Some(srcset) = elem.value().attr("srcset") {
                    for url_part in extract_srcset_urls(srcset) {
                        if let Some(abs_url) = resolve_url(&base_url, &url_part) {
                            if is_same_domain(&abs_url, base_host) {
                                urls.push(abs_url);
                            }
                        }
                    }
                }
            }
        }
    }

    urls
}

/// Extract individual URLs from a srcset string: "a.jpg 1x, b.jpg 2x" → ["a.jpg", "b.jpg"]
fn extract_srcset_urls(srcset: &str) -> Vec<String> {
    srcset
        .split(',')
        .filter_map(|part| {
            let trimmed = part.trim();
            trimmed.split_whitespace().next().map(|s| s.to_string())
        })
        .collect()
}

pub(crate) fn is_same_domain(url: &Url, base_host: &str) -> bool {
    let host = match url.host_str() {
        Some(h) => h,
        None => return false,
    };
    let host = host.strip_prefix("www.").unwrap_or(host);
    let base = base_host.strip_prefix("www.").unwrap_or(base_host);
    host == base
}

/// Normalize URL for dedup: strip fragments, lowercase scheme+host.
fn normalize_url(url: &Url) -> Url {
    let mut u = url.clone();
    u.set_fragment(None);
    u
}

/// Maximum retry attempts for transient errors (5xx, connection failures).
const MAX_RETRIES: u32 = 2;

/// Reuse a locally fresh file: extract links for pages/CSS, skip download.
async fn reuse_local(
    url: &Url,
    output_base: &str,
    prior: &PriorState,
) -> Result<ProcessResult> {
    let save_path = url_to_path(url, output_base);
    let save_path_rel = prior
        .rel_path
        .clone()
        .unwrap_or_else(|| {
            save_path
                .strip_prefix(output_base)
                .unwrap_or(&save_path)
                .to_string_lossy()
                .trim_start_matches('/')
                .to_string()
        });

    let body = tokio::fs::read(&save_path).await?;
    let rtype = match prior.rtype.as_deref() {
        Some("page") => ResourceType::Page,
        Some("css") => ResourceType::Css,
        Some("js") => ResourceType::Js,
        Some("image") => ResourceType::Image,
        Some(_) => ResourceType::Other,
        None => classify_by_ext(url.path()),
    };

    let host = url.host_str().unwrap_or("");
    let new_urls = match rtype {
        ResourceType::Page => {
            let html_str = String::from_utf8_lossy(&body);
            let doc = Html::parse_document(&html_str);
            extract_urls(&doc, url, host)
        }
        ResourceType::Css => {
            let css_str = String::from_utf8_lossy(&body);
            extract_css_urls(&css_str, url, host)
        }
        _ => Vec::new(),
    };

    let norm = normalize_url(url);
    Ok(ProcessResult {
        rtype,
        url: norm.as_str().to_string(),
        save_path: save_path_rel,
        new_urls,
        bytes: body,
        mtime: prior.mtime.clone(),
        etag: prior.etag.clone(),
        not_modified: true,
    })
}

/// Process a single URL: download (with retry), save, return discovered links.
pub(crate) async fn process_one(
    client: &Client,
    url: &Url,
    output_base: &str,
    pb: &ProgressBar,
    prior: Option<PriorState>,
) -> Result<ProcessResult> {
    pb.set_message(format!("Fetching {}", url.path()));

    let save_path = url_to_path(url, output_base);
    let file_exists = save_path.exists();

    // Local hash match and no validators → reuse without network.
    // If validators exist, prefer conditional GET to detect remote changes.
    if let Some(ref p) = prior {
        let has_validators = p.etag.is_some() || p.mtime.is_some();
        if p.local_fresh && file_exists && !has_validators {
            return reuse_local(url, output_base, p).await;
        }
    }

    let conditional = prior.as_ref().filter(|_| file_exists);
    let response = fetch_with_retry(client, url, conditional).await?;

    if response.status() == StatusCode::NOT_MODIFIED {
        if let Some(ref p) = prior {
            return reuse_local(url, output_base, p).await;
        }
        anyhow::bail!("HTTP 304 for {} but no local entry", url);
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let mtime = response
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let body = response.bytes().await?;
    let rtype = classify(content_type.as_deref(), url);

    let save_path_rel = save_path
        .strip_prefix(output_base)
        .unwrap_or(&save_path)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_string();

    if let Some(parent) = save_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // For pages and CSS, rewrite links and extract sub-resources.
    // Manifest hashes the final on-disk bytes (rewritten when applicable).
    let (new_urls, written) = match rtype {
        ResourceType::Page => {
            let html_str = String::from_utf8_lossy(&body);
            let rewritten = rewriter::rewrite_html(&html_str, url);
            let written = rewritten.into_bytes();
            tokio::fs::write(&save_path, &written).await?;

            let doc = Html::parse_document(&html_str);
            let host = url.host_str().unwrap_or("");
            (extract_urls(&doc, url, host), written)
        }
        ResourceType::Css => {
            let css_str = String::from_utf8_lossy(&body);
            let rewritten = rewriter::rewrite_css(&css_str, url);
            let written = rewritten.into_bytes();
            tokio::fs::write(&save_path, &written).await?;

            let host = url.host_str().unwrap_or("");
            (extract_css_urls(&css_str, url, host), written)
        }
        _ => {
            let written = body.to_vec();
            tokio::fs::write(&save_path, &written).await?;
            (Vec::new(), written)
        }
    };

    let norm = normalize_url(url);
    Ok(ProcessResult {
        rtype,
        url: norm.as_str().to_string(),
        save_path: save_path_rel,
        new_urls,
        bytes: written,
        mtime,
        etag,
        not_modified: false,
    })
}

/// Fetch a URL with retry on transient errors (5xx, rate-limit, network).
/// When `prior` is set, sends conditional validators and accepts 304.
async fn fetch_with_retry(
    client: &Client,
    url: &Url,
    prior: Option<&PriorState>,
) -> Result<reqwest::Response> {
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(500 * 2u64.pow(attempt - 1))).await;
        }

        let mut req = client.get(url.as_str());
        if let Some(p) = prior {
            if let Some(ref etag) = p.etag {
                req = req.header(reqwest::header::IF_NONE_MATCH, etag.as_str());
            }
            if let Some(ref mtime) = p.mtime {
                req = req.header(reqwest::header::IF_MODIFIED_SINCE, mtime.as_str());
            }
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status == StatusCode::OK || status == StatusCode::NOT_MODIFIED {
                    return Ok(resp);
                }
                if (status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS)
                    && attempt < MAX_RETRIES
                {
                    eprintln!(
                        "  ↻ {} for {} (attempt {}/{})",
                        status,
                        url,
                        attempt + 1,
                        MAX_RETRIES + 1
                    );
                    last_err = Some(anyhow::anyhow!("HTTP {} for {}", status, url));
                    continue;
                }
                anyhow::bail!("HTTP {} for {}", status, url);
            }
            Err(e) => {
                if attempt < MAX_RETRIES {
                    eprintln!(
                        "  ↜ network error for {} (attempt {}/{}): {}",
                        url,
                        attempt + 1,
                        MAX_RETRIES + 1,
                        e
                    );
                    last_err = Some(e.into());
                    continue;
                }
                return Err(e.into());
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("retry exhausted for {}", url)))
}

/// Extract same-domain sub-resource URLs from CSS content (url(), @import).
pub(crate) fn extract_css_urls(css: &str, css_url: &Url, base_host: &str) -> Vec<Url> {
    let mut urls = Vec::new();

    if let Ok(re) = regex::Regex::new(r#"url\(\s*['"]?([^'")]+)['"]?\s*\)"#) {
        for cap in re.captures_iter(css) {
            let url_text = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if url_text.starts_with("data:") {
                continue;
            }
            if let Some(abs_url) = resolve_url(css_url, url_text) {
                if is_same_domain(&abs_url, base_host) {
                    urls.push(abs_url);
                }
            }
        }
    }

    if let Ok(re) = regex::Regex::new(r#"@import\s+['\"]([^'\"]+)['\"]"#) {
        for cap in re.captures_iter(css) {
            let url_text = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Some(abs_url) = resolve_url(css_url, url_text) {
                if is_same_domain(&abs_url, base_host) {
                    urls.push(abs_url);
                }
            }
        }
    }

    urls
}
/// Simple robots.txt checker. Handles `User-agent: *` sections.
struct RobotsChecker {
    disallows: Vec<String>,
    allows: Vec<String>,
}

impl RobotsChecker {
    /// Fetch and parse robots.txt from the target URL.
    async fn fetch(client: &Client, base_url: &Url) -> Self {
        let robots_url = {
            let mut u = base_url.clone();
            u.set_path("/robots.txt");
            u
        };
        let (disallows, allows) = match client.get(robots_url.as_str()).send().await {
            Ok(resp) if resp.status() == StatusCode::OK => {
                let body = resp.text().await.unwrap_or_default();
                Self::parse(&body)
            }
            _ => (vec![], vec![]), // no robots.txt → allow everything
        };
        RobotsChecker { disallows, allows }
    }

    /// Parse raw robots.txt body. Only processes `User-agent: *` sections.
    fn parse(body: &str) -> (Vec<String>, Vec<String>) {
        let mut disallows = Vec::new();
        let mut allows = Vec::new();
        let mut in_universal = false;

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(ua) = line.strip_prefix("User-agent:").map(|s| s.trim()) {
                in_universal = ua.eq_ignore_ascii_case("*");
                continue;
            }
            if !in_universal {
                continue;
            }
            if let Some(path) = line.strip_prefix("Disallow:").map(|s| s.trim()) {
                if !path.is_empty() {
                    disallows.push(path.to_string());
                }
            }
            if let Some(path) = line.strip_prefix("Allow:").map(|s| s.trim()) {
                if !path.is_empty() {
                    allows.push(path.to_string());
                }
            }
        }

        (disallows, allows)
    }

    /// Check if a URL path is allowed by robots.txt.
    ///
    /// Per RFC 9309, the most specific (longest) matching rule wins.
    /// If allow and disallow match with the same length, allow wins.
    fn is_allowed(&self, path: &str) -> bool {
        let mut best_len = 0usize;
        let mut best_allowed = true;

        for rule in &self.disallows {
            if path.starts_with(rule.as_str()) && rule.len() > best_len {
                best_len = rule.len();
                best_allowed = false;
            }
        }
        for rule in &self.allows {
            if path.starts_with(rule.as_str()) && rule.len() >= best_len {
                best_len = rule.len();
                best_allowed = true;
            }
        }
        best_allowed
    }
}

/// True when an intercepted network URL is the HTML document itself
/// (should not be re-downloaded as an asset after SPA render).
pub(crate) fn is_spa_document_url(asset: &Url, page_url: &Url, final_url: &Url) -> bool {
    let a = normalize_url(asset);
    a == normalize_url(page_url) || a == normalize_url(final_url)
}

#[cfg(test)]
mod robots_tests {
    use super::*;

    #[test]
    fn test_spa_document_url_excludes_page_and_final() {
        let page = Url::parse("https://example.com/app").unwrap();
        let final_url = Url::parse("https://example.com/app/").unwrap();
        let doc = Url::parse("https://example.com/app").unwrap();
        let doc_slash = Url::parse("https://example.com/app/").unwrap();
        let asset = Url::parse("https://example.com/app.js").unwrap();

        assert!(is_spa_document_url(&doc, &page, &final_url));
        assert!(is_spa_document_url(&doc_slash, &page, &final_url));
        assert!(!is_spa_document_url(&asset, &page, &final_url));
    }

    #[test]
    fn test_robots_no_rules() {
        let checker = RobotsChecker {
            disallows: vec![],
            allows: vec![],
        };
        assert!(checker.is_allowed("/anything"));
    }

    #[test]
    fn test_robots_disallow() {
        let checker = RobotsChecker {
            disallows: vec!["/private".to_string()],
            allows: vec![],
        };
        assert!(!checker.is_allowed("/private/secret"));
        assert!(checker.is_allowed("/public"));
    }

    #[test]
    fn test_robots_longest_match_wins() {
        // /private is disallowed, but /private/public is explicitly allowed.
        // The longer (more specific) Allow rule should win.
        let checker = RobotsChecker {
            disallows: vec!["/private".to_string()],
            allows: vec!["/private/public".to_string()],
        };
        assert!(checker.is_allowed("/private/public/page"));
        assert!(!checker.is_allowed("/private/secret"));
    }

    #[test]
    fn test_robots_equal_length_allow_wins() {
        // When Allow and Disallow rules match with equal length, Allow wins.
        let checker = RobotsChecker {
            disallows: vec!["/path".to_string()],
            allows: vec!["/path".to_string()],
        };
        assert!(checker.is_allowed("/path/page"));
    }
}
/// Fetch the first page and determine whether it's a SPA (Single Page
/// Application) that requires headless-browser rendering.
///
/// Heuristics:
///   1. Framework-specific markers (`__NEXT_DATA__`, `__NUXT__`, `ng-version`, etc.)
///   2. ESM `<script type="module">` entry + almost-empty `<body>`
///   3. Root/app div (#root, #app, #__next) with very little visible text
pub async fn detect_spa(url: &Url) -> bool {
    let client = match Client::builder()
        .user_agent("Mozilla/5.0 (compatible; SiteGrab/0.1)")
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    let resp = match client.get(url.as_str()).send().await {
        Ok(r) if r.status() == StatusCode::OK => r,
        _ => return false,
    };

    let html = match resp.text().await {
        Ok(t) => t,
        Err(_) => return false,
    };

    analyze_spa_html(&html)
}

/// Pure-function SPA detector (also used by unit tests).
fn analyze_spa_html(html: &str) -> bool {
    let lower = html.to_lowercase();

    // --- Strong framework signals ---
    let strong_markers = [
        "__next_data__",
        "__nuxt__",
        "ng-version",
        "data-reactroot",
        "data-react-root",
        "data-server-rendered",
        "vite-plugin-pwa",
        "registersw",
        "data-v-app",
    ];
    for marker in &strong_markers {
        if lower.contains(marker) {
            return true;
        }
    }

    // --- Heuristic: ESM module scripts + near-empty body + SPA root div ---
    let module_count = lower
        .matches(r#"type="module""#)
        .count()
        + lower.matches("type='module'").count();

    let doc = Html::parse_document(html);

    // Count visible text length in <body>
    let mut body_text_len = 0usize;
    if let Ok(body_sel) = Selector::parse("body") {
        if let Some(body) = doc.select(&body_sel).next() {
            let text: String = body.text().collect();
            body_text_len = text.trim().len();
        }
    }

    // Check for common SPA root container ids
    let spa_root_ids = ["#root", "#app", "#__next", "#__nuxt", "#q-app", "#__vue"];
    let mut has_spa_root = false;
    for sel_str in &spa_root_ids {
        if let Ok(sel) = Selector::parse(sel_str) {
            if doc.select(&sel).next().is_some() {
                has_spa_root = true;
                break;
            }
        }
    }

    // SPA pattern: has ESM modules, a root div, but very little rendered text
    if has_spa_root && module_count >= 1 && body_text_len < 500 {
        return true;
    }

    // Another SPA pattern: lots of JS, very little body text even without
    // a classic root div (some frameworks use custom mount points)
    let script_count = lower.matches("<script").count();
    if script_count >= 3 && body_text_len < 200 && module_count >= 1 {
        return true;
    }

    false
}

/// Build conditional/local reuse state from an existing manifest entry.
fn prior_from_manifest(mf: &Manifest, url: &str, output_dir: &str) -> Option<PriorState> {
    let entry = mf.entry(url)?;
    Some(PriorState {
        etag: entry.etag.clone(),
        mtime: entry.mtime.clone(),
        local_fresh: mf.is_fresh(url, output_dir),
        rtype: Some(entry.rtype.clone()),
        rel_path: Some(entry.path.clone()),
    })
}

/// Run a full BFS crawl of a website.
pub async fn crawl(
    url: &Url,
    output_dir: &str,
    concurrency: usize,
    manifest: Option<tokio::sync::Mutex<Manifest>>,
    respect_robots: bool,
    limits: CrawlLimits,
) -> Result<Stats> {
    let client = Arc::new(
        Client::builder()
            .user_agent("Mozilla/5.0 (compatible; SiteGrab/0.1)")
            .redirect(reqwest::redirect::Policy::limited(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()?,
    );

    let stats = Arc::new(AtomicStats::default());
    // `visited` tracks URLs processed in *this* run only — never pre-seed from
    // the manifest, or incremental runs would skip link rediscovery.
    let visited = Arc::new(Mutex::new(HashSet::new()));
    let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
    let seed = normalize_url(url);
    let out_dir = output_dir.to_string();

    // Fetch robots.txt if requested
    let robots = if respect_robots {
        let checker = RobotsChecker::fetch(&client, url).await;
        if !checker.disallows.is_empty() || !checker.allows.is_empty() {
            eprintln!("info: robots.txt loaded ({} disallows, {} allows)",
                checker.disallows.len(), checker.allows.len());
        }
        Some(checker)
    } else {
        None
    };

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} [{elapsed_precise}] {msg}")
            .unwrap(),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    let pb = Arc::new(pb);

    let mut set: JoinSet<Result<ProcessResult>> = JoinSet::new();

    // Seed the first URL
    {
        let mut v = visited.lock().await;
        v.insert(seed.clone());
    }

    let seed_prior = if let Some(ref mf) = manifest {
        let mf = mf.lock().await;
        prior_from_manifest(&mf, seed.as_str(), &out_dir)
    } else {
        None
    };

    let permit = semaphore.clone().acquire_owned().await.unwrap();

    // Clone before first spawn
    let c1 = Arc::clone(&client);
    let s1 = Arc::clone(&stats);
    let pb1 = Arc::clone(&pb);
    let o1 = out_dir.clone();
    set.spawn(async move {
        let _permit = permit;
        let res = process_one(&c1, &seed, &o1, &pb1, seed_prior).await;
        match res {
            Ok(pr) => {
                if !pr.not_modified {
                    s1.record(pr.rtype, pr.bytes.len() as u64);
                } else {
                    s1.record(pr.rtype, 0);
                }
                Ok(pr)
            }
            Err(e) => {
                s1.record_err();
                Err(e)
            }
        }
    });

    // Main loop: collect results and spawn new tasks
    while let Some(result) = set.join_next().await {
        match result {
            Ok(Ok(pr)) => {
                // Record in manifest (hash of final on-disk bytes)
                if let Some(ref mf) = manifest {
                    let mut mf = mf.lock().await;
                    let rtype_str = rtype_to_str(pr.rtype);
                    mf.record(
                        pr.url,
                        pr.save_path,
                        &pr.bytes,
                        pr.mtime,
                        pr.etag,
                        rtype_str,
                    );
                }

                for new_url in pr.new_urls {
                    if limits.reached(&stats) {
                        eprintln!(
                            "info: crawl limit reached (max_pages={}, max_bytes={}) — stopping enqueue",
                            limits.max_pages, limits.max_bytes
                        );
                        break;
                    }

                    let norm = normalize_url(&new_url);
                    let is_new = {
                        let mut v = visited.lock().await;
                        v.insert(norm.clone())
                    };
                    if !is_new {
                        continue;
                    }

                    // Check robots.txt
                    if let Some(ref robots) = robots {
                        if !robots.is_allowed(norm.path()) {
                            eprintln!("  🚫 robots.txt: skipped {}", norm.path());
                            continue;
                        }
                    }

                    let prior = if let Some(ref mf) = manifest {
                        let mf = mf.lock().await;
                        prior_from_manifest(&mf, norm.as_str(), &out_dir)
                    } else {
                        None
                    };

                    let permit = semaphore.clone().acquire_owned().await.unwrap();
                    let c = Arc::clone(&client);
                    let s = Arc::clone(&stats);
                    let p = Arc::clone(&pb);
                    let o = out_dir.clone();
                    set.spawn(async move {
                        let _permit = permit;
                        let res = process_one(&c, &new_url, &o, &p, prior).await;
                        match res {
                            Ok(pr) => {
                                if !pr.not_modified {
                                    s.record(pr.rtype, pr.bytes.len() as u64);
                                } else {
                                    s.record(pr.rtype, 0);
                                }
                                Ok(pr)
                            }
                            Err(e) => {
                                s.record_err();
                                Err(e.context(format!("{}", new_url)))
                            }
                        }
                    });
                }
            }
            Ok(Err(e)) => {
                eprintln!("  ⚠ {e}");
            }
            Err(e) => {
                eprintln!("  ⚠ task panic: {e}");
            }
        }
    }

    // Collect all visited URLs
    let visited_urls: Vec<String> = {
        let v = visited.lock().await;
        v.iter().map(|u| u.as_str().to_string()).collect()
    };

    // Save manifest with visited URLs
    if let Some(ref mf) = manifest {
        let mut mf = mf.lock().await;
        for url in &visited_urls {
            mf.visited.insert(url.clone());
        }
        let _ = mf.save_to(&out_dir);
    }

    let s = stats.load();
    println!();
    println!("📄 Pages: {}", s.pages);
    println!("🖼  Images: {}", s.images);
    println!("🎨 CSS: {}", s.css);
    println!("📦 JS: {}", s.js);
    println!("📁 Size: {}", format_bytes(s.total_bytes));
    if s.errors > 0 {
        println!("⚠  Errors: {}", s.errors);
    }
    println!();
    println!("✓ Mirror completed");
    println!("✓ Offline ready");

    Ok(s)
}

fn rtype_to_str(r: ResourceType) -> &'static str {
    match r {
        ResourceType::Page => "page",
        ResourceType::Css => "css",
        ResourceType::Js => "js",
        ResourceType::Image => "image",
        ResourceType::Other => "other",
    }
}

// --- Atomic stats for concurrent updates ---
#[derive(Default)]
struct AtomicStats {
    pages: AtomicUsize,
    images: AtomicUsize,
    css: AtomicUsize,
    js: AtomicUsize,
    total_bytes: AtomicU64,
    errors: AtomicUsize,
}

impl AtomicStats {
    fn record(&self, rtype: ResourceType, bytes: u64) {
        self.total_bytes.fetch_add(bytes, Ordering::Relaxed);
        match rtype {
            ResourceType::Page => {
                self.pages.fetch_add(1, Ordering::Relaxed);
            }
            ResourceType::Css => {
                self.css.fetch_add(1, Ordering::Relaxed);
            }
            ResourceType::Js => {
                self.js.fetch_add(1, Ordering::Relaxed);
            }
            ResourceType::Image => {
                self.images.fetch_add(1, Ordering::Relaxed);
            }
            ResourceType::Other => {}
        }
    }

    fn record_err(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    fn load(&self) -> Stats {
        Stats {
            pages: self.pages.load(Ordering::Relaxed),
            images: self.images.load(Ordering::Relaxed),
            css: self.css.load(Ordering::Relaxed),
            js: self.js.load(Ordering::Relaxed),
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

// ============================================================================
// SPA rendering mode (Vue / React / Angular)
// Requires the `render` feature and a Chromium/Chrome binary at runtime.
// ============================================================================

#[cfg(feature = "render")]
mod spa {
    use super::*;
    use crate::manifest::Manifest;
    use crate::renderer;
    use futures::StreamExt;
    use std::collections::{HashSet, VecDeque};

    /// Crawl a SPA site: render every route with a headless browser,
    /// download all assets the browser fetched, then save pre-rendered HTML.
    ///
    /// - `wait_ms`: extra settle time after page load (for lazy-loaded content).
    pub async fn crawl_spa(
        url: &Url,
        output_dir: &str,
        concurrency: usize,
        manifest: Option<tokio::sync::Mutex<Manifest>>,
        respect_robots: bool,
        wait_ms: u64,
        limits: CrawlLimits,
        no_sandbox: bool,
    ) -> Result<Stats> {
        let client = Arc::new(
            Client::builder()
                .user_agent("Mozilla/5.0 (compatible; SiteGrab/0.1)")
                .redirect(reqwest::redirect::Policy::limited(10))
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
        );

        let stats = Arc::new(AtomicStats::default());
        let out_dir = output_dir.to_string();
        let base_host = url.host_str().unwrap_or("").to_string();
        let host_norm = base_host.strip_prefix("www.").unwrap_or(&base_host).to_string();

        // Launch headless browser.
        eprintln!("info: Launching headless browser for SPA rendering...");
        let (browser, mut handler) = renderer::launch_browser_async(no_sandbox).await?;
        let _handler_task = tokio::spawn(async move {
            while handler.next().await.is_some() {}
        });

        // robots.txt
        let robots = if respect_robots {
            let checker = RobotsChecker::fetch(&client, url).await;
            if !checker.disallows.is_empty() || !checker.allows.is_empty() {
                eprintln!(
                    "info: robots.txt loaded ({} disallows, {} allows)",
                    checker.disallows.len(),
                    checker.allows.len()
                );
            }
            Some(checker)
        } else {
            None
        };

        let pb = Arc::new(ProgressBar::new_spinner());
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {msg}")
                .unwrap(),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));

        // BFS frontier of routes to render.
        // `visited` is this-run only — do not pre-seed from the manifest.
        let mut visited: HashSet<Url> = HashSet::new();
        // Track assets already downloaded (or queued) this run.
        let mut done_assets: HashSet<Url> = HashSet::new();

        let mut queue: VecDeque<Url> = VecDeque::new();
        let seed = normalize_url(url);
        visited.insert(seed.clone());
        queue.push_back(seed);

        // Semaphore for asset downloads.
        let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));

        while let Some(route) = queue.pop_front() {
            if limits.reached(&stats) {
                eprintln!(
                    "info: crawl limit reached (max_pages={}, max_bytes={}) — stopping SPA crawl",
                    limits.max_pages, limits.max_bytes
                );
                break;
            }

            // robots.txt check for this route
            if let Some(ref r) = robots {
                if !r.is_allowed(route.path()) {
                    eprintln!("  🚫 robots.txt: skipped {}", route);
                    continue;
                }
            }

            // Incremental: reuse fresh rendered HTML without re-launching Chrome.
            let page_prior = if let Some(ref mf) = manifest {
                let mf = mf.lock().await;
                prior_from_manifest(&mf, route.as_str(), &out_dir)
            } else {
                None
            };
            if let Some(ref p) = page_prior {
                if p.local_fresh {
                    if let Ok(pr) = reuse_local(&route, &out_dir, p).await {
                        stats.record(ResourceType::Page, 0);
                        for link in pr.new_urls {
                            let norm = normalize_url(&link);
                            if visited.insert(norm.clone()) {
                                let path = norm.path().to_lowercase();
                                let last_seg = path.rsplit('/').next().unwrap_or("");
                                let has_ext = last_seg.contains('.');
                                if !has_ext || path.ends_with(".html") || path.ends_with(".htm") {
                                    queue.push_back(norm);
                                }
                            }
                        }
                        continue;
                    }
                }
            }

            pb.set_message(format!("Rendering {}", route));
            let render = match renderer::render_page(&browser, &route, &host_norm, wait_ms).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("  ⚠ render failed for {route}: {e}");
                    stats.record_err();
                    continue;
                }
            };

            // --- Save the rendered HTML ---
            let page_url = &render.final_url;
            // Strip <script> tags so the framework doesn't re-hydrate and
            // wipe the DOM when API calls fail offline.
            let stripped = crate::rewriter::strip_scripts(&render.html);
            let rewritten = crate::rewriter::rewrite_html(&stripped, page_url);

            let save_path = url_to_path(page_url, &out_dir);
            let save_path_rel = save_path
                .strip_prefix(&out_dir)
                .unwrap_or(&save_path)
                .to_string_lossy()
                .trim_start_matches('/')
                .to_string();

            if let Some(parent) = save_path.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    eprintln!("  ⚠ mkdir failed: {e}");
                }
            }
            let html_bytes = rewritten.into_bytes();
            if let Err(e) = tokio::fs::write(&save_path, &html_bytes).await {
                eprintln!("  ⚠ write failed for {}: {e}", save_path.display());
                stats.record_err();
            } else {
                stats.record(ResourceType::Page, html_bytes.len() as u64);

                // Record in manifest.
                if let Some(ref mf) = manifest {
                    let mut mf = mf.lock().await;
                    let norm = normalize_url(page_url);
                    mf.record(
                        norm.as_str().to_string(),
                        save_path_rel.clone(),
                        &html_bytes,
                        None,
                        None,
                        "page",
                    );
                }
            }

            // --- Download all assets the browser requested ---
            let mut assets: Vec<Url> = Vec::new();
            for asset_url in &render.resource_urls {
                let norm = normalize_url(asset_url);

                // Never re-download the document URL — it would overwrite the
                // rendered HTML with the raw SPA shell.
                if is_spa_document_url(&norm, &route, page_url) {
                    continue;
                }

                // Skip CDN-internal and analytics endpoints that can't be
                // meaningfully downloaded (Cloudflare RUM, ___cflb, etc.).
                let path = norm.path();
                if path.starts_with("/cdn-cgi/")
                    || path.contains("/__cf_")
                    || path.ends_with("/rum")
                    || path.contains("cloudflareinsights")
                {
                    continue;
                }

                if done_assets.insert(norm.clone()) {
                    assets.push(norm);
                }
            }

            // Download assets concurrently.
            download_assets(
                &assets,
                &client,
                &out_dir,
                &pb,
                &stats,
                &semaphore,
                &manifest,
            )
            .await;

            // --- Enqueue new internal links ---
            for link in &render.links {
                let norm = normalize_url(link);
                if visited.insert(norm.clone()) {
                    // Only enqueue paths that look like routes (not static assets).
                    let path = norm.path().to_lowercase();
                    let last_seg = path.rsplit('/').next().unwrap_or("");
                    let has_ext = last_seg.contains('.');
                    if !has_ext || path.ends_with(".html") || path.ends_with(".htm") {
                        queue.push_back(norm);
                    }
                }
            }
        }

        // Persist manifest.
        if let Some(ref mf) = manifest {
            let mut mf = mf.lock().await;
            for u in &visited {
                let s = u.to_string();
                mf.visited.insert(s);
            }
            let _ = mf.save_to(&out_dir);
        }

        drop(browser);

        let s = stats.load();
        println!();
        println!("📄 Pages rendered: {}", s.pages);
        println!("🖼  Images: {}", s.images);
        println!("🎨 CSS: {}", s.css);
        println!("📦 JS: {}", s.js);
        println!("📁 Size: {}", format_bytes(s.total_bytes));
        if s.errors > 0 {
            println!("⚠  Errors: {}", s.errors);
        }
        println!();
        println!("✓ SPA render completed");
        println!("✓ Offline ready");

        Ok(s)
    }

    /// Download a batch of asset URLs concurrently using process_one.
    async fn download_assets(
        urls: &[Url],
        client: &Arc<Client>,
        out_dir: &str,
        pb: &Arc<ProgressBar>,
        stats: &Arc<AtomicStats>,
        semaphore: &Arc<Semaphore>,
        manifest: &Option<tokio::sync::Mutex<Manifest>>,
    ) {
        let mut set: JoinSet<std::result::Result<ProcessResult, anyhow::Error>> = JoinSet::new();

        for url in urls {
            let prior = if let Some(mf) = manifest {
                let mf = mf.lock().await;
                prior_from_manifest(&mf, url.as_str(), out_dir)
            } else {
                None
            };
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let c = Arc::clone(client);
            let p = Arc::clone(pb);
            let url = url.clone();
            let o = out_dir.to_string();

            set.spawn(async move {
                let _permit = permit;
                process_one(&c, &url, &o, &p, prior).await
            });
        }

        while let Some(result) = set.join_next().await {
            match result {
                Ok(Ok(pr)) => {
                    if !pr.not_modified {
                        stats.record(pr.rtype, pr.bytes.len() as u64);
                    } else {
                        stats.record(pr.rtype, 0);
                    }
                    if let Some(mf) = manifest {
                        let mut mf = mf.lock().await;
                        let rtype_str = rtype_to_str(pr.rtype);
                        mf.record(
                            pr.url,
                            pr.save_path,
                            &pr.bytes,
                            pr.mtime,
                            pr.etag,
                            rtype_str,
                        );
                    }
                }
                Ok(Err(e)) => {
                    eprintln!("  ⚡ asset download error: {e}");
                    stats.record_err();
                }
                Err(e) => {
                    eprintln!("  ⚡ task panic: {e}");
                    stats.record_err();
                }
            }
        }
    }
}

#[cfg(feature = "render")]
pub use spa::crawl_spa;

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use crate::manifest::Manifest;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn e2e_crawl_downloads_page_and_image() {
        let server = MockServer::start().await;
        let base = server.uri();

        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string(format!(
                        r#"<html><body><a href="{base}/about">About</a><img src="{base}/logo.png"></body></html>"#
                    )),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/about"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string("<html><body>About page</body></html>"),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/logo.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(vec![0x89, 0x50, 0x4E, 0x47]),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_str().unwrap();
        let start = Url::parse(&format!("{base}/")).unwrap();
        let mf = tokio::sync::Mutex::new(Manifest::new(start.as_str()));

        let stats = crawl(&start, out, 4, Some(mf), false, CrawlLimits::default())
            .await
            .unwrap();
        assert!(stats.pages >= 2);
        assert!(stats.images >= 1);
        assert!(dir.path().join("index.html").exists());
        assert!(dir.path().join("about/index.html").exists());
        assert!(dir.path().join("logo.png").exists());

        let mf = Manifest::load_from(out).unwrap().unwrap();
        assert_eq!(
            mf.rtype_of(start.as_str()),
            Some("page"),
            "manifest should record page type"
        );
    }

    #[tokio::test]
    async fn e2e_incremental_skips_unchanged_with_etag() {
        let server = MockServer::start().await;
        let base = server.uri();
        let hits = AtomicUsize::new(0);
        let hits = std::sync::Arc::new(hits);

        let hits_clone = Arc::clone(&hits);
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(move |req: &wiremock::Request| {
                hits_clone.fetch_add(1, AtomicOrdering::SeqCst);
                if req
                    .headers
                    .get("if-none-match")
                    .map(|v| v.to_str().unwrap_or("") == "\"v1\"")
                    .unwrap_or(false)
                {
                    ResponseTemplate::new(304)
                } else {
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/html")
                        .insert_header("etag", "\"v1\"")
                        .set_body_string("<html><body>Hello</body></html>")
                }
            })
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_str().unwrap();
        let start = Url::parse(&format!("{base}/")).unwrap();

        let mf1 = tokio::sync::Mutex::new(Manifest::new(start.as_str()));
        crawl(&start, out, 2, Some(mf1), false, CrawlLimits::default())
            .await
            .unwrap();
        let first_hits = hits.load(AtomicOrdering::SeqCst);
        assert!(first_hits >= 1);
        assert!(dir.path().join("index.html").exists());

        // Second run should send If-None-Match and get 304.
        let mf2 = Manifest::load_from(out).unwrap().unwrap();
        let mf2 = tokio::sync::Mutex::new(mf2);
        crawl(&start, out, 2, Some(mf2), false, CrawlLimits::default())
            .await
            .unwrap();
        let second_hits = hits.load(AtomicOrdering::SeqCst);
        assert!(
            second_hits > first_hits,
            "expected a revalidation request on incremental run"
        );
    }
}
