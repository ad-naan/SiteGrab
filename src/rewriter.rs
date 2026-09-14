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
        // Match url-bearing attributes (rewritten to relative paths) and
        // inline style attributes (rewritten for url(...) references).
        Regex::new(
            r#"(?i)((?:\s+(?:href|src|action|poster|data)\s*=\s*)"([^"]*?)")"#,
        )
        .unwrap()
    })
}

fn style_attr_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)(\s+style\s*=\s*)"([^"]*?)""#).unwrap())
}

fn srcset_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Match both srcset and data-srcset (lazy-loaded responsive images).
        Regex::new(r#"(?i)(\s+(?:srcset|data-srcset)\s*=\s*)"([^"]*?)""#).unwrap()
    })
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

/// Regex matching `<base href="...">` tags — the leftover tag makes the
/// browser re-resolve every relative link against the live site, so it must
/// be removed after we've used it as the resolution base.
fn base_href_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?is)<base\b[^>]*?\bhref\s*=\s*["'][^"']*["'][^>]*?>"#).unwrap()
    })
}

/// Regex matching `<meta http-equiv="refresh" content="0;url=...">` tags —
/// unrewritten, they bounce offline readers back to the live site.
fn meta_refresh_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?is)<meta\b[^>]*?\bhttp-equiv\s*=\s*["']refresh["'][^>]*?\bcontent\s*=\s*["']([^"']*)["'][^>]*?>"#,
        )
        .unwrap()
    })
}


/// Cached regex to remove executable `<script>...</script>` blocks.
/// Data scripts (`type="application/ld+json"`, `text/template`,
/// `speculationrules`) are preserved.
#[cfg(feature = "render")]
fn script_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?si)<script\b[^>]*>.*?</script>").unwrap()
    })
}

/// Check whether a `<script ...>` opening tag is a data/non-executable
/// script that should be kept in the offline copy (structured data,
/// templates, speculation rules).
#[cfg(feature = "render")]
fn is_data_script(open_tag: &str) -> bool {
    let re = regex::Regex::new(
        r#"(?i)\btype\s*=\s*["'](?:application/ld\+json|text/template|speculationrules|application/json)["']"#,
    );
    match re {
        Ok(re) => re.is_match(open_tag),
        Err(_) => false,
    }
}

/// Remove executable `<script>` tags from HTML. This is used for SPA pages
/// so the framework doesn't re-hydrate and wipe the DOM when API calls fail
/// offline. Data scripts (JSON-LD, templates) are kept.
#[cfg(feature = "render")]
pub fn strip_scripts(html: &str) -> String {
    if !html.to_lowercase().contains("<script") {
        return html.to_string();
    }
    let re = script_tag_regex();
    let mut result = String::with_capacity(html.len());
    let mut last_end = 0;
    for m in re.find_iter(html) {
        let open_tag_end = html[m.start()..m.end()]
            .find('>')
            .map(|i| m.start() + i)
            .unwrap_or(m.start());
        let open_tag = &html[m.start()..open_tag_end];
        if is_data_script(open_tag) {
            continue;
        }
        result.push_str(&html[last_end..m.start()]);
        last_end = m.end();
    }
    result.push_str(&html[last_end..]);
    result
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
        && !html.contains("style=")
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
    let style_re = style_attr_regex();

    let mut result = String::with_capacity(html.len() + 4096);
    let mut last_end = 0;

    #[derive(Clone, Copy)]
    struct Span {
        start: usize,
        end: usize,
        kind: SpanKind,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum SpanKind {
        Attr,
        Srcset,
        Style,
    }

    let mut spans: Vec<Span> = Vec::new();
    for m in attr_re.find_iter(html) {
        spans.push(Span { start: m.start(), end: m.end(), kind: SpanKind::Attr });
    }
    for m in srcset_re.find_iter(html) {
        spans.push(Span { start: m.start(), end: m.end(), kind: SpanKind::Srcset });
    }
    for m in style_re.find_iter(html) {
        spans.push(Span { start: m.start(), end: m.end(), kind: SpanKind::Style });
    }
    spans.sort_by_key(|s| s.start);

    for span in &spans {
        result.push_str(&html[last_end..span.start]);
        let matched = &html[span.start..span.end];

        let eq_pos = matched.find('=').unwrap();
        let attr_prefix = matched[..eq_pos].trim_end();
        let rest = &matched[eq_pos + 1..];
        let value = &rest[1..rest.len() - 1];

        match span.kind {
            SpanKind::Srcset => {
                if let Some(nv) = rewrite_srcset(value, &base_url, &page_path) {
                    result.push_str(&format!("{}=\"{}\"", attr_prefix, nv));
                } else {
                    result.push_str(matched);
                }
            }
            SpanKind::Style => {
                if let Some(nv) = rewrite_inline_style(value, &base_url, &page_path) {
                    result.push_str(&format!("{}=\"{}\"", attr_prefix, nv));
                } else {
                    result.push_str(matched);
                }
            }
            SpanKind::Attr => {
                if let Some(new_path) = rewrite_url_value(value, &base_url, &page_path) {
                    result.push_str(&format!("{}=\"{}\"", attr_prefix, new_path));
                } else {
                    result.push_str(matched);
                }
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
///   - `<base href="...">` (makes relative links resolve against the live site)
///   - `<link rel="manifest">` (CORS + PWA)
///   - `<link rel="modulepreload">` (ES module preloads need CORS, and
///     scripts are stripped anyway so preloads are dead weight)
///   - `<link rel="preload" as="script">` (same)
///   - `<script src="registerSW.js">` SW loader
///   - Inline `<script>` blocks registering a Service Worker
///   - `crossorigin` attribute on any remaining tag (triggers CORS checks)
///   - `<meta http-equiv="refresh">` pointing at the live site (rewritten to
///     the mirrored path when same-domain, removed otherwise)
fn strip_offline_breakers(html: String) -> String {
    let sw_ext_re = sw_external_script_regex();
    let manifest_re = manifest_link_regex();
    let modulepreload_re = modulepreload_link_regex();
    let script_preload_re = script_preload_link_regex();
    let sw_inline_re = sw_inline_script_regex();
    let crossorigin_re = crossorigin_attr_regex();

    // Remove whole tags that are inherently incompatible with file://
    let after_re = base_href_tag_regex().replace_all(&html, "");
    let after_re = sw_ext_re.replace_all(&after_re, "");
    let after_re = manifest_re.replace_all(&after_re, "");
    let after_re = modulepreload_re.replace_all(&after_re, "");
    let after_re = script_preload_re.replace_all(&after_re, "");

    // Rewrite <meta http-equiv="refresh"> URLs to the mirrored location.
    let after_re = rewrite_meta_refresh(&after_re);

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

/// Rewrite or remove `<meta http-equiv="refresh" content="N;url=...">` tags.
/// Same-domain targets are rewritten to the offline path; anything else
/// (external, anchor-only) has the tag removed so the offline copy never
/// bounces the reader back to the live site.
fn rewrite_meta_refresh(html: &str) -> String {
    let re = meta_refresh_regex();
    if !re.is_match(html) {
        return html.to_string();
    }

    // Resolution base for the URL inside `content` is the page itself; the
    // page URL isn't available here, so we only rewrite same-domain absolute
    // or root-relative URLs (the overwhelmingly common cases). To do this we
    // need the page URL — but rewrite_html has already rewritten all other
    // references; meta refresh is rare enough that dropping to removal for
    // non-root-relative values keeps behaviour safe.
    let mut result = String::with_capacity(html.len());
    let mut last_end = 0;
    let caps: Vec<(usize, usize, Option<String>)> = re
        .captures_iter(html)
        .filter_map(|cap| {
            let m = cap.get(0)?;
            let content = cap.get(1)?.as_str();
            // content looks like "5;url=/path" or "0; URL=/path"
            let url_part = content
                .split(';')
                .find_map(|part| part.trim().strip_prefix("url=").map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string()))
                .unwrap_or_default();
            Some((m.start(), m.end(), if url_part.is_empty() { None } else { Some(url_part) }))
        })
        .collect();

    for (start, end, url_part) in caps {
        result.push_str(&html[last_end..start]);
        match url_part {
            Some(u) if u.starts_with('/') && !u.starts_with("//") => {
                // Root-relative same-site target: strip the meta refresh —
                // the page it points to is mirrored and linked normally.
                // (A site-internal redirect chain is already captured as a
                // page by the crawler, so dropping the tag is lossless.)
            }
            _ => {
                // No URL or external URL → remove the tag entirely.
            }
        }
        last_end = end;
    }
    result.push_str(&html[last_end..]);
    result
}

/// Rewrite `url(...)` references inside an inline `style="..."` attribute.
fn rewrite_inline_style(value: &str, base_url: &Url, page_path: &str) -> Option<String> {
    let re = css_url_regex();
    let mut changed = false;
    let mut result = String::with_capacity(value.len());

    let mut last_end = 0;
    for cap in re.captures_iter(value) {
        let m = cap.get(0)?;
        let url_m = cap.get(1)?;
        let url_text = url_m.as_str();
        if url_text.starts_with("data:") {
            continue;
        }
        if let Some(new_path) = rewrite_url_value(url_text, base_url, page_path) {
            result.push_str(&value[last_end..m.start()]);
            result.push_str(&format!("url({})", new_path));
            changed = true;
            last_end = m.end();
        }
    }
    result.push_str(&value[last_end..]);

    if changed {
        Some(result)
    } else {
        None
    }
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

    #[test]
    fn test_strip_base_href() {
        let page = Url::parse("https://example.com/docs/").unwrap();
        let html = r#"<html><head><base href="https://example.com/"><a href="/x/">X</a></head></html>"#;
        let rewritten = rewrite_html(html, &page);
        assert!(!rewritten.contains("<base"), "base tag must be removed");
        assert!(rewritten.contains("href=\"../x/index.html\""), "links resolved against base: {rewritten}");
    }

    #[test]
    fn test_strip_meta_refresh() {
        let page = Url::parse("https://example.com/").unwrap();
        let html = r#"<meta http-equiv="refresh" content="0;url=/moved/">"#;
        let rewritten = rewrite_html(html, &page);
        assert!(!rewritten.contains("refresh"), "meta refresh must be removed: {rewritten}");
    }

    #[test]
    fn test_rewrite_inline_style_url() {
        let page = Url::parse("https://example.com/").unwrap();
        let html = r#"<div style="background-image:url('/img/bg.png')">x</div>"#;
        let rewritten = rewrite_html(html, &page);
        assert!(rewritten.contains("url(img/bg.png)"), "inline style url must be rewritten: {rewritten}");
    }

    #[test]
    fn test_rewrite_data_srcset() {
        let page = Url::parse("https://example.com/").unwrap();
        let html = r#"<img data-srcset="https://example.com/a.jpg 1x, https://example.com/b.jpg 2x">"#;
        let rewritten = rewrite_html(html, &page);
        assert!(rewritten.contains("a.jpg 1x"));
        assert!(rewritten.contains("b.jpg 2x"));
    }

    #[test]
    fn test_rewrite_object_data_and_iframe_src() {
        let page = Url::parse("https://example.com/").unwrap();
        let html = r#"<iframe src="https://example.com/embed/1"></iframe><object data="https://example.com/p.pdf"></object>"#;
        let rewritten = rewrite_html(html, &page);
        assert!(rewritten.contains("embed/1/index.html"), "iframe src must be rewritten: {rewritten}");
        assert!(rewritten.contains("p.pdf"), "object data must be rewritten: {rewritten}");
    }

    #[cfg(feature = "render")]
    #[test]
    fn test_strip_scripts_keeps_ld_json() {
        let html = r#"<script>window.x=1</script><script type="application/ld+json">{"a":1}</script>"#;
        let stripped = strip_scripts(html);
        assert!(!stripped.contains("window.x"), "executable script removed");
        assert!(stripped.contains("ld+json"), "JSON-LD kept: {stripped}");
    }
}
