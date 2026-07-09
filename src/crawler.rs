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

use crate::rewriter;
use crate::manifest::Manifest;
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
struct ProcessResult {
    rtype: ResourceType,
    url: String,
    save_path: String,
    new_urls: Vec<Url>,
    bytes: Vec<u8>,
    mtime: Option<String>,
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

/// Convert URL path to filesystem path
fn url_to_path(url: &Url, output_base: &str) -> PathBuf {
    let mut path = url.path().trim_start_matches('/').to_string();

    if path.is_empty() || path.ends_with('/') {
        path.push_str("index.html");
    } else {
        let last_seg = path.rsplit('/').next().unwrap_or("");
        if !last_seg.contains('.') {
            path.push_str("/index.html");
        }
    }

    PathBuf::from(output_base).join(&path)
}

/// Resolve a potentially relative URL, skipping non-HTTP(S) protocols
fn resolve_url(base: &Url, href: &str) -> Option<Url> {
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

/// Extract same-domain URLs from an HTML document
fn extract_urls(doc: &Html, base_url: &Url, base_host: &str) -> Vec<Url> {
    let mut urls = Vec::new();
    let pairs = [
        ("a[href]", "href"),
        ("link[href]", "href"),
        ("script[src]", "src"),
        ("img[src]", "src"),
        ("source[src]", "src"),
        ("video[src]", "src"),
        ("audio[src]", "src"),
    ];

    for (sel_str, attr) in &pairs {
        if let Ok(sel) = Selector::parse(sel_str) {
            for elem in doc.select(&sel) {
                if let Some(val) = elem.value().attr(attr) {
                    if let Some(abs_url) = resolve_url(base_url, val) {
                        if is_same_domain(&abs_url, base_host) {
                            urls.push(abs_url);
                        }
                    }
                }
            }
        }
    }

    urls
}

fn is_same_domain(url: &Url, base_host: &str) -> bool {
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

/// Process a single URL: download, save, return discovered links
async fn process_one(
    client: &Client,
    url: &Url,
    output_base: &str,
    pb: &ProgressBar,
) -> Result<ProcessResult> {
    pb.set_message(format!("Fetching {}", url.path()));

    let response = client.get(url.as_str()).send().await?;

    if response.status() != StatusCode::OK {
        anyhow::bail!("HTTP {} for {}", response.status(), url);
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

    let body = response.bytes().await?;
    let rtype = classify(content_type.as_deref(), url);

    let save_path = url_to_path(url, output_base);
    let save_path_rel = save_path
        .strip_prefix(output_base)
        .unwrap_or(&save_path)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_string();

    if let Some(parent) = save_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let new_urls = if rtype == ResourceType::Page {
        let html_str = String::from_utf8_lossy(&body);
        let rewritten = rewriter::rewrite_html(&html_str, url);
        tokio::fs::write(&save_path, rewritten.as_bytes()).await?;

        let doc = Html::parse_document(&html_str);
        let host = url.host_str().unwrap_or("");
        extract_urls(&doc, url, host)
    } else {
        tokio::fs::write(&save_path, &body).await?;
        Vec::new()
    };

    let norm = normalize_url(url);
    Ok(ProcessResult {
        rtype,
        url: norm.as_str().to_string(),
        save_path: save_path_rel,
        new_urls,
        bytes: body.to_vec(),
        mtime,
    })
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

#[cfg(test)]
mod robots_tests {
    use super::*;

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
/// Run a full BFS crawl of a website.
pub async fn crawl(
    url: &Url,
    output_dir: &str,
    concurrency: usize,
    manifest: Option<tokio::sync::Mutex<Manifest>>,
    respect_robots: bool,
) -> Result<Stats> {
    let client = Arc::new(
        Client::builder()
            .user_agent("Mozilla/5.0 (compatible; SiteGrab/0.1)")
            .redirect(reqwest::redirect::Policy::limited(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()?,
    );

    let stats = Arc::new(AtomicStats::default());
    let visited = Arc::new(Mutex::new(HashSet::new()));
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let seed = normalize_url(url);
    let out_dir = output_dir.to_string();

    // Pre-populate visited set from manifest
    if let Some(ref mf) = manifest {
        let mf = mf.lock().await;
        for url_str in &mf.visited {
            if let Ok(u) = Url::parse(url_str) {
                visited.lock().await.insert(normalize_url(&u));
            }
        }
    }

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

    let permit = semaphore.clone().acquire_owned().await.unwrap();

    // Clone before first spawn
    let c1 = Arc::clone(&client);
    let s1 = Arc::clone(&stats);
    let pb1 = Arc::clone(&pb);
    let o1 = out_dir.clone();
    set.spawn(async move {
        let _permit = permit;
        let res = process_one(&c1, &seed, &o1, &pb1).await;
        match res {
            Ok(pr) => {
                s1.record(pr.rtype, pr.bytes.len() as u64);
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
                // Record in manifest
                if let Some(ref mf) = manifest {
                    let mut mf = mf.lock().await;
                    let rtype_str = rtype_to_str(pr.rtype);
                    mf.record(pr.url, pr.save_path, &pr.bytes, pr.mtime, rtype_str);
                }

                for new_url in pr.new_urls {
                    let norm = normalize_url(&new_url);
                    let is_new = {
                        let mut v = visited.lock().await;
                        v.insert(norm.clone())
                    };
                    if !is_new {
                        continue;
                    }

                    // Check if already downloaded (incremental / resume)
                    if let Some(ref mf) = manifest {
                        let mf = mf.lock().await;
                        if mf.is_fresh(norm.as_str(), &out_dir) {
                            let rtype = mf.rtype_of(norm.as_str());
                            if let Some(rt) = rtype {
                                record_skipped(&stats, rt);
                            }
                            continue;
                        }
                    }

                    // Check robots.txt
                    if let Some(ref robots) = robots {
                        if !robots.is_allowed(norm.path()) {
                            eprintln!("  🚫 robots.txt: skipped {}", norm.path());
                            continue;
                        }
                    }

                    let permit = semaphore.clone().acquire_owned().await.unwrap();
                    let c = Arc::clone(&client);
                    let s = Arc::clone(&stats);
                    let p = Arc::clone(&pb);
                    let o = out_dir.clone();
                    set.spawn(async move {
                        let _permit = permit;
                        let res = process_one(&c, &new_url, &o, &p).await;
                        match res {
                            Ok(pr) => {
                                s.record(pr.rtype, pr.bytes.len() as u64);
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
            if !mf.visited.contains(url) {
                mf.visited.push(url.clone());
            }
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

fn record_skipped(stats: &AtomicStats, rtype: &str) {
    match rtype {
        "page" => stats.pages.fetch_add(1, Ordering::Relaxed),
        "css" => stats.css.fetch_add(1, Ordering::Relaxed),
        "js" => stats.js.fetch_add(1, Ordering::Relaxed),
        "image" => stats.images.fetch_add(1, Ordering::Relaxed),
        _ => return,
    };
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

