use std::path::Path;

use regex::Regex;
use url::Url;

/// Compute a relative filesystem path from `page_path` to `target_url` for offline browsing.
///
/// `page_path` is the saved path like `"about/index.html"` or `"images/logo.png"`
/// `target_url` is the absolute URL being linked to
fn relative_path(page_path: &str, target_url: &Url) -> String {
    let target_path = url_to_offline_path(target_url);

    let page_dir = Path::new(page_path)
        .parent()
        .unwrap_or(Path::new(""));

    // If both are empty or root, just return target
    if page_dir.as_os_str().is_empty() && target_path == "index.html" {
        return target_path;
    }

    // Count components in page_dir and target_path
    let dir_comps: Vec<&str> = page_dir
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    let target_comps: Vec<&str> = target_path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    // Find common prefix length
    let common = dir_comps
        .iter()
        .zip(target_comps.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Go up from page_dir to common ancestor
    let mut result = String::new();
    for _ in common..dir_comps.len() {
        result.push_str("../");
    }

    // Go down from common ancestor to target
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

/// Convert URL to its offline filesystem path
///   /about   → about/index.html
///   /about/  → about/index.html
///   /img/a.png → img/a.png
fn url_to_offline_path(url: &Url) -> String {
    let path = url.path().trim_start_matches('/');

    if path.is_empty() || path.ends_with('/') {
        format!("{}index.html", path)
    } else {
        let last_seg = path.rsplit('/').next().unwrap_or("");
        if last_seg.contains('.') {
            path.to_string()
        } else {
            format!("{}/index.html", path)
        }
    }
}

/// Rewrite HTML for offline browsing: convert absolute paths to relative.
pub fn rewrite_html(html: &str, page_url: &Url) -> String {
    // Early exit for common pattern: no links at all
    if !html.contains("href=") && !html.contains("src=") {
        return html.to_string();
    }

    let page_path = url_to_offline_path(page_url);

    let attr_re = Regex::new(
        r#"(?i)((?:\s+(?:href|src|action|srcset|poster)\s*=\s*)"([^"]*?)")"#,
    )
    .unwrap();

    let mut result = String::with_capacity(html.len() + 4096);
    let mut last_end = 0;

    for cap in attr_re.find_iter(html) {
        // Push text before this match
        result.push_str(&html[last_end..cap.start()]);

        let matched = cap.as_str();
        let eq_pos = matched.find('=').unwrap();
        let attr_name = &matched[..eq_pos];
        let quoted_value = &matched[eq_pos + 1..];
        let value = &quoted_value[1..quoted_value.len() - 1];

        // Skip anchors, javascript:, mailto:, data: etc.
        if value.starts_with('#')
            || value.starts_with("javascript:")
            || value.starts_with("mailto:")
            || value.starts_with("tel:")
            || value.starts_with("data:")
        {
            result.push_str(matched);
            last_end = cap.end();
            continue;
        }

        let resolved = match page_url.join(value) {
            Ok(u) => u,
            Err(_) => {
                result.push_str(matched);
                last_end = cap.end();
                continue;
            }
        };

        // Only rewrite HTTP(S) URLs on the same host
        if resolved.scheme() != "http" && resolved.scheme() != "https" {
            result.push_str(matched);
            last_end = cap.end();
            continue;
        }

        let resolved_host = resolved.host_str().unwrap_or("");
        let page_host = page_url.host_str().unwrap_or("");

        let resolved_host_norm = resolved_host.strip_prefix("www.").unwrap_or(resolved_host);
        let page_host_norm = page_host.strip_prefix("www.").unwrap_or(page_host);

        if resolved_host_norm != page_host_norm {
            // External link — keep as-is
            result.push_str(matched);
            last_end = cap.end();
            continue;
        }

        // Skip if this would produce the same path or a degenerate "." (self-reference)
        let new_path = relative_path(&page_path, &resolved);
        if new_path == value || new_path == "." || new_path == page_path {
            result.push_str(matched);
            last_end = cap.end();
            continue;
        }

        // Same-domain link — rewrite to relative path
        let new_attr = format!("{}=\"{}\"", attr_name, new_path);
        result.push_str(&new_attr);
        last_end = cap.end();
    }

    result.push_str(&html[last_end..]);
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
    fn test_relative_path_subdir() {
        let target = Url::parse("https://example.com/css/style.css").unwrap();
        assert_eq!(
            relative_path("about/index.html", &target),
            "../css/style.css"
        );
    }

    #[test]
    fn test_relative_path_nested() {
        let target = Url::parse("https://example.com/index.html").unwrap();
        assert_eq!(
            relative_path("blog/post-1/index.html", &target),
            "../../index.html"
        );
    }

    #[test]
    fn test_relative_path_root() {
        let target = Url::parse("https://example.com/about").unwrap();
        assert_eq!(relative_path("index.html", &target), "about/index.html");
    }

    #[test]
    fn test_rewrite_html_simple() {
        let html = r#"<a href="/about">About</a>"#;
        let page = Url::parse("https://example.com/index.html").unwrap();
        let result = rewrite_html(html, &page);
        assert_eq!(result, r#"<a href="about/index.html">About</a>"#);
    }

    #[test]
    fn test_rewrite_image() {
        let html = r#"<img src="/images/logo.png">"#;
        let page = Url::parse("https://example.com/index.html").unwrap();
        let result = rewrite_html(html, &page);
        assert_eq!(result, r#"<img src="images/logo.png">"#);
    }

    #[test]
    fn test_rewrite_external_unchanged() {
        let html = r#"<a href="https://other.com/page">Link</a>"#;
        let page = Url::parse("https://example.com/index.html").unwrap();
        let result = rewrite_html(html, &page);
        assert_eq!(result, html);
    }

    #[test]
    fn test_rewrite_relative_unchanged() {
        let html = r##"<a href="#section">Anchor</a>"##;
        let page = Url::parse("https://example.com/index.html").unwrap();
        let result = rewrite_html(html, &page);
        assert_eq!(result, html);
    }

    #[test]
    fn test_rewrite_self_reference_no_dot() {
        // A link to the site root from a page at root should not become "."
        let html = r#"<a href="/">Home</a>"#;
        let page = Url::parse("https://example.com/").unwrap();
        let result = rewrite_html(html, &page);
        assert_eq!(result, html);
    }
}
