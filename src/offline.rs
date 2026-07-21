//! Offline reference closure verification for mirrored sites.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;
use url::Url;

use crate::manifest::Manifest;
use crate::pathmap::{self, UrlKind};

#[derive(Debug, Default)]
pub struct ClosureReport {
    pub checked: usize,
    pub missing: Vec<String>,
    pub escaped: Vec<String>,
    pub duplicate_paths: Vec<String>,
}

impl ClosureReport {
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty() && self.escaped.is_empty() && self.duplicate_paths.is_empty()
    }
}

pub fn assert_offline_closure(output_dir: &str, base_host: &str) -> Result<()> {
    let report = verify_offline_closure(output_dir, base_host)?;
    if report.is_ok() {
        return Ok(());
    }
    for m in &report.missing {
        eprintln!("missing: {m}");
    }
    for e in &report.escaped {
        eprintln!("escaped: {e}");
    }
    for d in &report.duplicate_paths {
        eprintln!("duplicate: {d}");
    }
    bail!(
        "offline closure failed: {} missing, {} escaped, {} duplicate paths",
        report.missing.len(),
        report.escaped.len(),
        report.duplicate_paths.len()
    )
}

pub fn verify_offline_closure(output_dir: &str, _base_host: &str) -> Result<ClosureReport> {
    let mut report = ClosureReport::default();
    let root = PathBuf::from(output_dir);
    let manifest = Manifest::load_from(output_dir).ok().flatten();

    let mut path_to_urls: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(ref mf) = manifest {
        for (url, entry) in &mf.entries {
            path_to_urls
                .entry(entry.path.clone())
                .or_default()
                .push(url.clone());
        }
    }

    for (path, urls) in &path_to_urls {
        if urls.len() > 1 {
            report
                .duplicate_paths
                .push(format!("{path} ← {}", urls.join(", ")));
        }
    }

    let files = collect_html_css(&root);
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let content = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let refs = extract_local_refs(&content, &rel);
        for reference in refs {
            report.checked += 1;
            let target = root.join(&reference);
            if !pathmap::is_under_output(&target, output_dir) {
                report
                    .escaped
                    .push(format!("{rel} → {reference} (outside output dir)"));
                continue;
            }
            if !target.exists() {
                report.missing.push(format!("{rel} → {reference}"));
            }
        }
    }

    Ok(report)
}

fn collect_html_css(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_html_css_inner(root, &mut out);
    out
}

fn collect_html_css_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_html_css_inner(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("html") | Some("htm") | Some("css")
        ) {
            out.push(path);
        }
    }
}

fn extract_local_refs(content: &str, _from_rel: &str) -> Vec<String> {
    let mut refs = HashSet::new();

    let attr_re = Regex::new(
        r#"(?i)(?:href|src|action|poster|data-src|data-lazy-src)\s*=\s*["']([^"']+)["']"#,
    )
    .unwrap();
    for cap in attr_re.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            push_local_ref(m.as_str(), &mut refs);
        }
    }

    let url_re = Regex::new(r#"url\(\s*['"]?([^'")]+)['"]?\s*\)"#).unwrap();
    for cap in url_re.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            push_local_ref(m.as_str(), &mut refs);
        }
    }

    refs.into_iter().collect()
}

fn push_local_ref(raw: &str, out: &mut HashSet<String>) {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.starts_with('#')
        || raw.starts_with("data:")
        || raw.starts_with("javascript:")
        || raw.starts_with("mailto:")
        || raw.starts_with("http://")
        || raw.starts_with("https://")
    {
        return;
    }
    let normalized = raw.replace('\\', "/");
    out.insert(normalized);
}

/// Resolve a canonical online URL to an expected offline path (for tests).
pub fn expected_offline_path(url: &str, base_host: &str, kind: UrlKind) -> String {
    let parsed = Url::parse(url).unwrap();
    pathmap::url_to_offline_path_with(&parsed, base_host, None, kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn closure_passes_for_simple_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        fs::create_dir_all(out.join("css")).unwrap();
        fs::write(
            out.join("index.html"),
            r#"<html><link href="css/style.css"><img src="logo.png"></html>"#,
        )
        .unwrap();
        fs::write(out.join("css/style.css"), "body{}").unwrap();
        fs::write(out.join("logo.png"), b"x").unwrap();

        let report = verify_offline_closure(out.to_str().unwrap(), "example.com").unwrap();
        assert!(report.is_ok(), "{report:?}");
    }

    #[test]
    fn closure_fails_for_missing_asset() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        fs::write(out.join("index.html"), r#"<img src="missing.png">"#).unwrap();

        let report = verify_offline_closure(out.to_str().unwrap(), "example.com").unwrap();
        assert!(!report.is_ok());
        assert!(!report.missing.is_empty());
    }
}
