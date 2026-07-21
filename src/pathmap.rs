//! URL → offline filesystem path mapping (shared by crawler and rewriter).

use std::path::{Component, Path, PathBuf};

use url::Url;

/// Sanitise a string for safe use in a file path component.
fn sanitize_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

/// Convert a URL path segment into a safe filesystem component.
/// Rejects `.` and `..` so joins cannot escape the output directory.
fn safe_segment(seg: &str) -> String {
    if seg.is_empty() || seg == "." || seg == ".." {
        return "_".to_string();
    }
    let sane = sanitize_component(seg);
    if sane == "." || sane == ".." || sane.is_empty() {
        "_".to_string()
    } else {
        sane
    }
}

/// Convert URL to its offline filesystem relative path.
///   /about        → "about/index.html"
///   /img/a.png    → "img/a.png"
///   /post?id=1    → "post@id=1/index.html"
///   /../../x      → "_/_/x/index.html"  (no traversal)
pub fn url_to_offline_path(url: &Url) -> String {
    let raw_path = url.path().trim_start_matches('/');
    let query = url.query();

    let query_suffix = match query {
        Some(q) if !q.is_empty() => format!("@{}", sanitize_component(q)),
        _ => String::new(),
    };

    let segments: Vec<&str> = raw_path.split('/').filter(|s| !s.is_empty()).collect();

    if segments.is_empty() {
        return if query_suffix.is_empty() {
            "index.html".to_string()
        } else {
            format!("{}/index.html", query_suffix)
        };
    }

    let last = segments[segments.len() - 1];
    let has_ext = last.contains('.');
    let ends_with_slash = url.path().ends_with('/');

    let mut parts: Vec<String> = segments[..segments.len().saturating_sub(1)]
        .iter()
        .map(|s| safe_segment(s))
        .collect();

    if ends_with_slash || !has_ext {
        // Directory-style page → .../last/index.html or .../last@q/index.html
        parts.push(if query_suffix.is_empty() {
            safe_segment(last)
        } else {
            format!("{}{}", safe_segment(last), query_suffix)
        });
        parts.push("index.html".to_string());
    } else {
        // File with extension
        let safe_last = safe_segment(last);
        if query_suffix.is_empty() {
            parts.push(safe_last);
        } else {
            let dot_pos = safe_last.rfind('.').unwrap_or(safe_last.len());
            let (name, ext) = safe_last.split_at(dot_pos);
            parts.push(format!("{}{}{}", name, query_suffix, ext));
        }
    }

    parts.join("/")
}

/// Join `output_base` with the offline relative path, ensuring the result
/// stays under `output_base` (no path traversal).
pub fn url_to_path(url: &Url, output_base: &str) -> PathBuf {
    let rel = url_to_offline_path(url);
    let base = PathBuf::from(output_base);
    let joined = base.join(&rel);

    // Defense in depth: if any component still looks like parent-dir, flatten.
    let mut clean = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::ParentDir => {}
            Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }

    // Ensure we still start with output_base when both are relative-ish.
    let result = if let Ok(stripped) = clean.strip_prefix(&base) {
        base.join(stripped)
    } else if clean.starts_with(&base) {
        clean
    } else {
        // Fallback: re-join only safe relative parts under base
        base.join(rel.replace("..", "_"))
    };

    // Defense in depth — never return a path outside the output directory.
    if !is_under_output(&result, output_base) {
        base.join(sanitize_component(&rel))
    } else {
        result
    }
}

/// Returns true if `candidate` is inside `output_base` (after lexical cleanup).
pub fn is_under_output(candidate: &Path, output_base: &str) -> bool {
    let base = PathBuf::from(output_base);
    let mut clean_base = PathBuf::new();
    for comp in base.components() {
        match comp {
            Component::ParentDir => {
                clean_base.pop();
            }
            Component::CurDir => {}
            other => clean_base.push(other.as_os_str()),
        }
    }
    let mut clean_cand = PathBuf::new();
    for comp in candidate.components() {
        match comp {
            Component::ParentDir => {
                clean_cand.pop();
            }
            Component::CurDir => {}
            other => clean_cand.push(other.as_os_str()),
        }
    }
    clean_cand.starts_with(&clean_base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_root_index() {
        let u = Url::parse("https://example.com/").unwrap();
        assert_eq!(url_to_offline_path(&u), "index.html");
    }

    #[test]
    fn test_directory_page() {
        let u = Url::parse("https://example.com/about").unwrap();
        assert_eq!(url_to_offline_path(&u), "about/index.html");
    }

    #[test]
    fn test_file_with_ext() {
        let u = Url::parse("https://example.com/img/a.png").unwrap();
        assert_eq!(url_to_offline_path(&u), "img/a.png");
    }

    #[test]
    fn test_query_on_page() {
        let u = Url::parse("https://example.com/article?id=1").unwrap();
        assert_eq!(url_to_offline_path(&u), "article@id=1/index.html");
    }

    #[test]
    fn test_query_on_file() {
        let u = Url::parse("https://example.com/img/photo.png?v=2").unwrap();
        assert_eq!(url_to_offline_path(&u), "img/photo@v=2.png");
    }

    #[test]
    fn test_rejects_parent_dir_segments() {
        let u = Url::parse("https://example.com/../../etc/passwd").unwrap();
        let rel = url_to_offline_path(&u);
        assert!(!rel.contains(".."), "path must not contain '..': {rel}");
        assert!(rel.contains("passwd") || rel.contains("index.html"));
        let full = url_to_path(&u, "out");
        assert!(
            is_under_output(&full, "out"),
            "path escaped output dir: {}",
            full.display()
        );
    }

    #[test]
    fn test_dot_segment_sanitized() {
        let u = Url::parse("https://example.com/foo/./bar").unwrap();
        let rel = url_to_offline_path(&u);
        // URL parsers may collapse `.`; either way no raw `.` segment escape
        assert!(!rel.split('/').any(|s| s == ".."));
        let full = url_to_path(&u, "mirror");
        assert!(is_under_output(&full, "mirror"));
    }

    #[test]
    fn test_distinct_queries() {
        let u1 = Url::parse("https://example.com/post?id=1").unwrap();
        let u2 = Url::parse("https://example.com/post?id=2").unwrap();
        assert_ne!(url_to_offline_path(&u1), url_to_offline_path(&u2));
    }
}
