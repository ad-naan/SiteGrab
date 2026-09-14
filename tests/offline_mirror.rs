//! Regression tests for reliable offline mirroring.

use sitegrab::crawler::{self, CrawlLimits};
use sitegrab::manifest::Manifest;
use sitegrab::offline;
use sitegrab::pathmap::{self, UrlKind};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn tracking_query_does_not_duplicate_homepage() {
    let server = MockServer::start().await;
    let base = server.uri();

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string("<html><body>Home</body></html>"),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().to_str().unwrap();
    let start = Url::parse(&format!("{base}/?ref=onepagelove&utm_source=x")).unwrap();
    let host = start.host_str().unwrap();
    let mf = tokio::sync::Mutex::new(Manifest::new(start.as_str()));

    crawler::crawl(&start, out, 2, Some(mf), false, CrawlLimits::default())
        .await
        .unwrap();

    assert!(dir.path().join("index.html").exists());
    assert!(
        !dir.path().join("@ref=onepagelove").exists(),
        "tracking query must not create duplicate homepage directory"
    );

    let canonical = pathmap::normalize_url(&start, UrlKind::Page, host);
    let loaded = Manifest::load_from(out).unwrap().unwrap();
    assert!(loaded.entries.contains_key(canonical.as_str()));
}

#[tokio::test]
async fn cross_domain_font_is_downloaded() {
    let site = MockServer::start().await;
    let cdn = MockServer::start().await;
    let site_base = site.uri();
    let cdn_base = cdn.uri();

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string(format!(
                    r#"<html><head><link rel="stylesheet" href="{cdn_base}/roboto.woff2"></head><body>Hi</body></html>"#
                )),
        )
        .mount(&site)
        .await;

    Mock::given(method("GET"))
        .and(path("/roboto.woff2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "font/woff2")
                .set_body_bytes(b"wOF2"),
        )
        .mount(&cdn)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().to_str().unwrap();
    let start = Url::parse(&format!("{site_base}/")).unwrap();
    let host = start.host_str().unwrap();
    let mf = tokio::sync::Mutex::new(Manifest::new(start.as_str()));

    crawler::crawl(&start, out, 2, Some(mf), false, CrawlLimits::default())
        .await
        .unwrap();

    let cdn_url = Url::parse(&format!("{cdn_base}/roboto.woff2")).unwrap();
    let cdn_host = cdn_url.host_str().unwrap();
    let cdn_port = cdn_url.port().unwrap();
    let external = dir
        .path()
        .join(format!("_external/{cdn_host}_{cdn_port}/roboto.woff2"));
    assert!(
        external.exists(),
        "expected {} to exist",
        external.display()
    );

    offline::assert_offline_closure(out, host).expect("offline closure");
}

#[test]
fn rsc_request_is_not_treated_as_page() {
    let u = Url::parse("https://example.com/articles?_rsc=abc").unwrap();
    assert!(pathmap::is_dynamic_request(&u));
    let norm = pathmap::normalize_url(&u, UrlKind::Page, "example.com");
    assert_eq!(norm.path(), "/articles");
    assert!(norm.query().is_none());
}
