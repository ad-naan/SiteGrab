use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use url::Url;

fn relative_path(page_path: &str, target_url: &Url) -> String {
    let (target_path, _) = url_to_offline_path(target_url);

    let page_dir = Path::new(page_path)
        .parent()
        .unwrap_or(Path::new(""));

    if page_dir.as_os_str().is_empty() && target_path == "index.html" {
        return target_path;
    }

    let dir_comps: Vec<&str> = page_dir
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    let target_comps: Vec<&str> = target_path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    let common = dir_comps
        .iter()
        .zip(target_comps.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut result = String::new();
    for _ in common..dir_comps.len() {
        result.push_str("../");
    }

    for (i, comp) in target_comps.iter().enumerate().skip(common) {
        if i > common {
            result.push('/');
        }
        result.push_str(comp);
    }

    if result.is_empty() {
        ".".to_string()
    } else {
        result
    }
}

/// Convert URL to its offline filesystem path + optional extension override.
///   /about        → ("about/index.html", None)
///   /img/a.png    → ("img/a.png", None)
///   /post?id=1    → ("post@id=1/index.html", None)
fn url_to_offline_path(url: &Url) -> (String, Option<String>) {
    let raw_path = url.path().trim_start_matches('/');
    let query = url.query();

    let query_suffix = match query {
        Some(q) if !q.is_empty() => {
            let sane: String = q
                .chars()
                .map(|c| match c {
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                    _ => c,
                })
                .collect();
            format!("@{}", sane)
        }
        _ => String::new(),
    };

    if raw_path.is_empty() || raw_path.ends_with('/') {
        if query_suffix.is_empty() {
            return (format!("{}index.html", raw_path), None);
        }
        return (format!("{}{}/index.html", raw_path, query_suffix), None);
    }

    let last_seg = raw_path.rsplit('/').next().unwrap_or("");
    let has_dot = last_seg.contains('.');

    if has_dot {
        if query_suffix.is_empty() {
            return (raw_path.to_string(), None);
        }
        let dot_pos = last_seg.rfind('.').unwrap_or(last_seg.len());
        let (name, ext) = last_seg.split_at(dot_pos);
        let new_last = format!("{}{}{}", name, query_suffix, ext);
        if let Some(prefix) = raw_path.strip_suffix(last_seg) {
            return (
                format!("{}{}", prefix, new_last),
                Some(ext.trim_start_matches('.').to_string()),
            );
        }
        return (new_last, Some(ext.trim_start_matches('.').to_string()));
    }

    if query_suffix.is_empty() {
        (format!("{}/index.html", raw_path), None)
    } else {
        (format!("{}{}/index.html", raw_path, query_suffix), None)
    }
}

fn attr_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)((?:\s+(?:href|src|action|poster)\s*=\s*)"([^"]*?)")"#).unwrap()
    })
}

fn srcset_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)(\s+srcset\s*=\s*)"([^"]*?)""#).unwrap())
}

fn css_url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"url\(\s*['"]?([^'")]+)['"]?\s*\)"#).unwrap())
}

fn css_import_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"@import\s+['"]([^'"]+)['"]"#).unwrap())
}

fn extract_base_href(html: &str, page_url: &Url) -> Option<Url> {
    let base_re = regex::Regex::new(r#"(?i)<base\s+[^>]*href\s*=\s*["']([^"']+)["']"#).ok()?;
    let cap = base_re.captures(html)?;
    page_url.join(cap.get(1)?.as_str()).ok()
}


/// Cached regex to remove `<script>...</script>` blocks (including self-closing).
#[cfg(feature = "render")]
fn script_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?si)<script[^>]*>.*?</script>").unwrap()
    })
}

/// Remove all `<script>` tags from HTML. This is used for SPA pages so the
/// framework doesn't re-hydrate and wipe the DOM when API calls fail offline.
#[cfg(feature = "render")]
pub fn strip_scripts(html: &str) -> String {
    if !html.to_lowercase().contains("<script") {
        return html.to_string();
    }
    script_tag_regex().replace_all(html, "").to_string()
}

fn rewrite_url_value(value: &str, base_url: &Url, page_path: &str) -> Option<String> {
    if value.starts_with('#')
        || value.starts_with("javascript:")
        || value.starts_with("mailto:")
        || value.starts_with("tel:")
        || value.starts_with("data:")
        || value.starts_with("blob:")
    {
        return None;
    }

    let resolved = base_url.join(value).ok()?;

    if resolved.scheme() != "http" && resolved.scheme() != "https" {
        return None;
    }

    let resolved_host = resolved.host_str().unwrap_or("");
    let page_host = base_url.host_str().unwrap_or("");
    let resolved_host_norm = resolved_host.strip_prefix("www.").unwrap_or(resolved_host);
    let page_host_norm = page_host.strip_prefix("www.").unwrap_or(page_host);

    if resolved_host_norm != page_host_norm {
        return None;
    }

    let new_path = relative_path(page_path, &resolved);
    if new_path == value || new_path == "." || new_path == page_path {
        return None;
    }

    Some(new_path)
}

pub fn rewrite_html(html: &str, page_url: &Url) -> String {
    if !html.contains("href=")
        && !html.contains("src=")
        && !html.contains("srcset=")
        && !html.contains("data-src")
    {
        return strip_offline_breakers(html.to_string());
    }

    let base_url = extract_base_href(html, page_url).unwrap_or_else(|| page_url.clone());
    let page_path = {
        let (p, _) = url_to_offline_path(page_url);
        p
    };

    let attr_re = attr_regex();
    let srcset_re = srcset_regex();

    let mut result = String::with_capacity(html.len() + 4096);
    let mut last_end = 0;

    #[derive(Clone, Copy)]
    struct Span {
        start: usize,
        end: usize,
        is_srcset: bool,
    }

    let mut spans: Vec<Span> = Vec::new();
    for m in attr_re.find_iter(html) {
        spans.push(Span { start: m.start(), end: m.end(), is_srcset: false });
    }
    for m in srcset_re.find_iter(html) {
        spans.push(Span { start: m.start(), end: m.end(), is_srcset: true });
    }
    spans.sort_by_key(|s| s.start);

    for span in &spans {
        result.push_str(&html[last_end..span.start]);
        let matched = &html[span.start..span.end];

        if span.is_srcset {
            let eq_pos = matched.find('=').unwrap();
            let attr_prefix = matched[..eq_pos].trim_end();
            let rest = &matched[eq_pos + 1..];
            let value = &rest[1..rest.len() - 1];

            if let Some(nv) = rewrite_srcset(value, &base_url, &page_path) {
                result.push_str(&format!("{}=\"{}\"", attr_prefix, nv));
            } else {
                result.push_str(matched);
            }
        } else {
            let eq_pos = matched.find('=').unwrap();
            let attr_prefix = matched[..eq_pos].trim_end();
            let quoted_value = &matched[eq_pos + 1..];
            let value = &quoted_value[1..quoted_value.len() - 1];

            if let Some(new_path) = rewrite_url_value(value, &base_url, &page_path) {
                result.push_str(&format!("{}=\"{}\"", attr_prefix, new_path));
            } else {
                result.push_str(matched);
            }
        }

        last_end = span.end;
    }

    result.push_str(&html[last_end..]);
    strip_offline_breakers(result)
}


/// Regex matching `<link rel="manifest">` tags — causes CORS errors when
/// the mirror is opened from `file://`.
fn manifest_link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?is)<link\b[^>]*?\brel\s*=\s*["']manifest["'][^>]*?>"#).unwrap()
    })
}

/// Regex matching `<link rel="modulepreload">` tags — ES module preloads
/// fail with CORS on `file://` and are useless once `<script>` tags are stripped.
fn modulepreload_link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?is)<link\b[^>]*?\brel\s*=\s*["']modulepreload["'][^>]*?>"#).unwrap()
    })
}

/// Regex matching `<link rel="preload" ... as="script" ...>` — preloads JS
/// modules that can't run on `file://`.
fn script_preload_link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?is)<link\b[^>]*?\brel\s*=\s*["']preload["'][^>]*?\bas\s*=\s*["']script["'][^>]*?>"#).unwrap()
    })
}

/// Regex matching external `<script src="registerSW.js">` style tags that
/// load a Service Worker bundle — fails on `file://` protocol.
fn sw_external_script_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?is)<script\b[^>]*?\bsrc\s*=\s*["'][^"']*(?:registerSW|workbox|sw-?register|sw\.js)[^"']*["'][^>]*?>\s*</script>"#,
        ).unwrap()
    })
}

fn sw_inline_script_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?is)<script\b[^>]*?>([\s\S]*?)</script>"#).unwrap())
}

/// Regex to strip the `crossorigin` attribute from any HTML tag.
/// Matches `crossorigin`, `crossorigin=""`, `crossorigin="anonymous"`,
/// `crossorigin="use-credentials"`, with single or double quotes.
fn crossorigin_attr_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)\s+crossorigin(?:\s*=\s*["'][^"']*["'])?"#).unwrap()
    })
}

/// Remove artifacts that break offline browsing under `file://`:
///   - `<link rel="manifest">` (CORS + PWA)
///   - `<link rel="modulepreload">` (ES module preloads need CORS, and
///     scripts are stripped anyway so preloads are dead weight)
///   - `<link rel="preload" as="script">` (same)
///   - `<script src="registerSW.js">` SW loader
///   - Inline `<script>` blocks registering a Service Worker
///   - `crossorigin` attribute on any remaining tag (triggers CORS checks)
fn strip_offline_breakers(html: String) -> String {
    let sw_ext_re = sw_external_script_regex();
    let manifest_re = manifest_link_regex();
    let modulepreload_re = modulepreload_link_regex();
    let script_preload_re = script_preload_link_regex();
    let sw_inline_re = sw_inline_script_regex();
    let crossorigin_re = crossorigin_attr_regex();

    // Remove whole tags that are inherently incompatible with file://
    let after_re = sw_ext_re.replace_all(&html, "");
    let after_re = manifest_re.replace_all(&after_re, "");
    let after_re = modulepreload_re.replace_all(&after_re, "");
    let after_re = script_preload_re.replace_all(&after_re, "");

    // Remove inline SW-registration scripts
    let mut result = String::with_capacity(after_re.len());
    let mut last_end = 0;
    for cap in sw_inline_re.captures_iter(&after_re) {
        let m = match cap.get(0) {
            Some(m) => m,
            None => continue,
        };
        let body = cap.get(1).map(|b| b.as_str()).unwrap_or("");
        if body.contains("serviceWorker")
            || body.contains("registerSW")
            || body.contains("workbox")
        {
            result.push_str(&after_re[last_end..m.start()]);
            last_end = m.end();
        }
    }
    result.push_str(&after_re[last_end..]);

    // Strip crossorigin attribute from all remaining tags
    crossorigin_re.replace_all(&result, "").to_string()
}

fn rewrite_srcset(srcset: &str, base_url: &Url, page_path: &str) -> Option<String> {
    let parts: Vec<&str> = srcset.split(',').collect();
    let mut rewritten_parts = Vec::new();
    let mut changed = false;

    for part in &parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut tokens = trimmed.split_whitespace();
        let url_token = match tokens.next() {
            Some(t) => t,
            None => continue,
        };
        let descriptor: String = tokens.collect::<Vec<_>>().join(" ");

        if let Some(new_path) = rewrite_url_value(url_token, base_url, page_path) {
            if !descriptor.is_empty() {
                rewritten_parts.push(format!("{} {}", new_path, descriptor));
            } else {
                rewritten_parts.push(new_path);
            }
            changed = true;
        } else {
            if !descriptor.is_empty() {
                rewritten_parts.push(format!("{} {}", url_token, descriptor));
            } else {
                rewritten_parts.push(url_token.to_string());
            }
        }
    }

    if changed {
        Some(rewritten_parts.join(", "))
    } else {
        None
    }
}

/// Rewrite CSS content for offline browsing: convert absolute `url()` and
/// `@import` references to relative paths.
pub fn rewrite_css(css: &str, css_url: &Url) -> String {
    let css_path = {
        let (p, _) = url_to_offline_path(css_url);
        p
    };

    let url_re = css_url_regex();
    let import_re = css_import_regex();

    #[derive(Clone, Copy)]
    struct CssSpan {
        start: usize,
        end: usize,
        url_text: usize,
        url_end: usize,
    }

    let mut spans: Vec<CssSpan> = Vec::new();
    for cap in url_re.captures_iter(css) {
        if let (Some(full), Some(url_match)) = (cap.get(0), cap.get(1)) {
            spans.push(CssSpan {
                start: full.start(),
                end: full.end(),
                url_text: url_match.start(),
                url_end: url_match.end(),
            });
        }
    }
    for cap in import_re.captures_iter(css) {
        if let (Some(full), Some(url_match)) = (cap.get(0), cap.get(1)) {
            spans.push(CssSpan {
                start: full.start(),
                end: full.end(),
                url_text: url_match.start(),
                url_end: url_match.end(),
            });
        }
    }

    spans.sort_by_key(|s| s.start);
    spans.dedup_by_key(|s| s.start);

    let mut result = String::with_capacity(css.len() + 256);
    let mut last_end = 0;

    for span in &spans {
        result.push_str(&css[last_end..span.start]);
        let url_text = &css[span.url_text..span.url_end];

        if url_text.starts_with("data:") {
            result.push_str(&css[span.start..span.end]);
            last_end = span.end;
            continue;
        }

        if let Some(new_path) = rewrite_url_value(url_text, css_url, &css_path) {
            result.push_str(&format!("url(\"{}\")", new_path));
        } else {
            result.push_str(&css[span.start..span.end]);
        }
        last_end = span.end;
    }

    result.push_str(&css[last_end..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    #[test]
    fn test_relative_path_same_dir() {
        let target = Url::parse("https://example.com/images/logo.png").unwrap();
        assert_eq!(relative_path("index.html", &target), "images/logo.png");
    }

    #[test]
    fn test_relative_path_parent() {
        let target = Url::parse("https://example.com/index.html").unwrap();
        assert_eq!(relative_path("about/index.html", &target), "../index.html");
    }

    #[test]
    fn test_url_to_offline_path_with_query() {
        let u = Url::parse("https://example.com/article?id=1").unwrap();
        let (p, _) = url_to_offline_path(&u);
        assert_eq!(p, "article@id=1/index.html");
    }

    #[test]
    fn test_url_to_offline_path_file_with_query() {
        let u = Url::parse("https://example.com/img/photo.png?v=2").unwrap();
        let (p, _) = url_to_offline_path(&u);
        assert_eq!(p, "img/photo@v=2.png");
    }

    #[test]
    fn test_url_to_offline_path_distinct_queries() {
        let u1 = Url::parse("https://example.com/post?id=1").unwrap();
        let u2 = Url::parse("https://example.com/post?id=2").unwrap();
        let (p1, _) = url_to_offline_path(&u1);
        let (p2, _) = url_to_offline_path(&u2);
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_rewrite_html_same_domain() {
        let page = Url::parse("https://example.com/about/").unwrap();
        let html = r#"<a href="https://example.com/">Home</a>"#.to_string();
        let rewritten = rewrite_html(&html, &page);
        assert!(rewritten.contains("../index.html"));
    }

    #[test]
    fn test_rewrite_html_srcset() {
        let page = Url::parse("https://example.com/").unwrap();
        let html = r#"<img src="small.jpg" srcset="https://example.com/big.jpg 2x, https://example.com/huge.jpg 3x">"#.to_string();
        let rewritten = rewrite_html(&html, &page);
        assert!(rewritten.contains("big.jpg 2x"));
        assert!(rewritten.contains("huge.jpg 3x"));
    }

    #[test]
    fn test_rewrite_css_url() {
        let css_url = Url::parse("https://example.com/css/style.css").unwrap();
        let css = "body { background: url('/images/bg.png'); }";
        let rewritten = rewrite_css(css, &css_url);
        assert!(rewritten.contains("../images/bg.png"));
    }

    #[test]
    fn test_rewrite_css_preserves_data_uri() {
        let css_url = Url::parse("https://example.com/css/style.css").unwrap();
        let css = r#"body { background: url("data:image/png;base64,iVBOR="); }"#;
        let rewritten = rewrite_css(css, &css_url);
        assert!(rewritten.contains("data:image/png"));
    }
}
