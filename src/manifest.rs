use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MANIFEST_FILE: &str = ".sitegrab.json";

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// A single file entry in the manifest.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub hash: String,
    pub mtime: Option<String>,
    /// ETag from the origin response, used for conditional revalidation.
    #[serde(default)]
    pub etag: Option<String>,
    /// Resource type: "page", "css", "js", "image", "font", "media", "other"
    pub rtype: String,
    /// Original URL before canonical normalization (when different from map key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_url: Option<String>,
    /// Outbound canonical page links discovered when this page was crawled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlinks: Vec<String>,
}

/// The full manifest for a mirrored site.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub start_url: String,
    pub created_at: String,
    pub updated_at: String,
    pub entries: HashMap<String, Entry>,
    pub visited: HashSet<String>,
}

impl Manifest {
    pub fn new(start_url: &str) -> Self {
        let now = timestamp();
        Manifest {
            version: 2,
            start_url: start_url.to_string(),
            created_at: now.clone(),
            updated_at: now,
            entries: HashMap::new(),
            visited: HashSet::new(),
        }
    }

    pub fn load_from(dir: &str) -> Result<Option<Self>> {
        let path = Path::new(dir).join(MANIFEST_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read manifest at {}", path.display()))?;
        let mf: Manifest =
            serde_json::from_str(&content).with_context(|| "Failed to parse manifest")?;
        Ok(Some(mf))
    }

    pub fn save_to(&mut self, dir: &str) -> Result<()> {
        self.updated_at = timestamp();
        let path = Path::new(dir).join(MANIFEST_FILE);
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write manifest at {}", path.display()))?;
        Ok(())
    }

    /// Check if a URL's file on disk matches the recorded hash of written bytes.
    pub fn is_fresh(&self, url: &str, output_dir: &str) -> bool {
        let entry = match self.entries.get(url) {
            Some(e) => e,
            None => return false,
        };
        let file_path = Path::new(output_dir).join(&entry.path);
        if !file_path.exists() {
            return false;
        }
        let bytes = match std::fs::read(&file_path) {
            Ok(b) => b,
            Err(_) => return false,
        };
        hash_bytes(&bytes) == entry.hash
    }

    /// Look up a stored entry by canonical URL.
    pub fn entry(&self, url: &str) -> Option<&Entry> {
        self.entries.get(url)
    }

    /// Record a downloaded file. `bytes` must be the final on-disk content.
    pub fn record(
        &mut self,
        url: String,
        path: String,
        bytes: &[u8],
        mtime: Option<String>,
        etag: Option<String>,
        rtype: &str,
    ) {
        self.record_with_meta(url, None, path, bytes, mtime, etag, rtype, &[]);
    }

    /// Record with optional original URL and outbound page links.
    #[allow(clippy::too_many_arguments)]
    pub fn record_with_meta(
        &mut self,
        canonical_url: String,
        original_url: Option<String>,
        path: String,
        bytes: &[u8],
        mtime: Option<String>,
        etag: Option<String>,
        rtype: &str,
        outlinks: &[String],
    ) {
        let h = hash_bytes(bytes);
        self.entries.insert(
            canonical_url,
            Entry {
                path,
                size: bytes.len() as u64,
                hash: h,
                mtime,
                etag,
                rtype: rtype.to_string(),
                original_url: original_url.filter(|o| !o.is_empty()),
                outlinks: outlinks.to_vec(),
            },
        );
    }

    /// Get the resource type string for a URL, if known.
    pub fn rtype_of(&self, url: &str) -> Option<&str> {
        self.entries.get(url).map(|e| e.rtype.as_str())
    }

    pub fn outlinks(&self, url: &str) -> Option<&[String]> {
        self.entries.get(url).map(|e| e.outlinks.as_slice())
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    format!("sha256:{}", hex::encode(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::Duration;

    fn test_dir(name: &str) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{name}_{id}"));
        path.to_string_lossy().to_string()
    }

    #[test]
    fn test_hash_consistency() {
        let data = b"hello world";
        let h1 = hash_bytes(data);
        let h2 = hash_bytes(data);
        assert_eq!(h1, h2);
        assert!(h1.starts_with("sha256:"));
        assert_eq!(h1.len(), 7 + 64);
    }

    #[test]
    fn test_manifest_save_load_roundtrip() {
        let dir = test_dir("_test_manifest");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut mf = Manifest::new("https://example.com/");
        mf.record_with_meta(
            "https://example.com/".into(),
            Some("https://example.com/?ref=x".into()),
            "index.html".into(),
            b"<html>hello</html>",
            None,
            Some("\"abc\"".into()),
            "page",
            &["https://example.com/about".to_string()],
        );
        std::fs::write(Path::new(&dir).join("index.html"), b"<html>hello</html>").unwrap();
        mf.visited.insert("https://example.com/".into());
        mf.save_to(&dir).unwrap();

        let loaded = Manifest::load_from(&dir).unwrap().unwrap();
        assert_eq!(loaded.start_url, "https://example.com/");
        assert_eq!(loaded.entries.len(), 1);
        assert!(loaded.entries.contains_key("https://example.com/"));
        assert!(loaded.visited.contains("https://example.com/"));
        assert!(loaded.is_fresh("https://example.com/", &dir));
        assert_eq!(loaded.rtype_of("https://example.com/"), Some("page"));
        assert_eq!(loaded.outlinks("https://example.com/").unwrap().len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_is_fresh_changed_file() {
        let dir = test_dir("_test_manifest_fresh");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut mf = Manifest::new("https://example.com/");
        mf.record(
            "https://example.com/".into(),
            "index.html".into(),
            b"original",
            None,
            None,
            "page",
        );
        std::fs::write(Path::new(&dir).join("index.html"), b"original").unwrap();
        assert!(mf.is_fresh("https://example.com/", &dir));

        std::fs::write(Path::new(&dir).join("index.html"), b"changed").unwrap();
        assert!(!mf.is_fresh("https://example.com/", &dir));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_manifest_concurrent_writes() {
        let dir = test_dir("_test_manifest_concurrent");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut mf = Manifest::new("https://example.com/");
        mf.record(
            "https://example.com/".into(),
            "index.html".into(),
            b"<html></html>",
            None,
            None,
            "page",
        );
        mf.save_to(&dir).unwrap();

        thread::sleep(Duration::from_millis(10));
        let loaded = Manifest::load_from(&dir).unwrap().unwrap();
        assert_eq!(loaded.entries.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
