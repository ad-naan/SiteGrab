use std::process;

use clap::Parser;
use url::Url;

mod archiver;
mod crawler;
mod manifest;
mod rewriter;
mod util;

#[cfg(feature = "render")]
mod renderer;

#[derive(Parser)]
#[command(
    name = "sitegrab",
    version,
    long_about = "\
sitegrab — Download a website for offline browsing.

Simple one-command mirroring:
  sitegrab https://example.com

Automatically detects SPA (Vue/React/Angular) sites and renders them
with a headless browser so the content is fully captured.

Supports incremental updates — re-running mirrors only new/changed files.
Use --robots to obey robots.txt, --fresh for a full re-download."
)]
struct Args {
    /// URL of the website to mirror
    url: String,

    /// Output directory (defaults to domain name)
    #[arg(short, long)]
    output: Option<String>,

    /// Number of concurrent downloads
    #[arg(short, long, default_value = "8")]
    jobs: usize,

    /// Skip ZIP archive creation
    #[arg(long)]
    no_zip: bool,

    /// Force fresh download (ignore existing manifest)
    #[arg(long)]
    fresh: bool,

    /// Respect robots.txt (default: no)
    #[arg(long)]
    robots: bool,

    /// SPA rendering mode: auto (default), on, or off.
    /// "auto" detects whether the site is a SPA and renders if needed.
    /// "on" forces headless-browser rendering for every page.
    /// "off" uses plain HTTP crawling only.
    #[arg(long, default_value = "auto")]
    render: String,

    /// Settle time (ms) to wait after page load for lazy/AJAX content.
    /// Only relevant when rendering is active. Default: 1500
    #[arg(long, default_value = "1500")]
    wait: u64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let url = match Url::parse(&args.url) {
        Ok(u) => {
            if u.scheme() != "http" && u.scheme() != "https" {
                eprintln!("error: URL must start with http:// or https://");
                process::exit(1);
            }
            u
        }
        Err(e) => {
            eprintln!("error: Invalid URL '{}': {}", args.url, e);
            process::exit(1);
        }
    };

    let host = match url.host_str() {
        Some(h) => h.to_string(),
        None => {
            eprintln!("error: URL must have a host (e.g. https://example.com)");
            process::exit(1);
        }
    };

    let output_dir = args.output.unwrap_or_else(|| host.clone());

    // Normalise render option: "auto" (default) detects SPA automatically,
    // "on"/"yes" forces render, "off"/"no" forces plain HTTP crawl.
    let render_opt = args.render.to_lowercase();
    let force_static = render_opt == "off" || render_opt == "no" || render_opt == "false";
    let force_render = render_opt == "on" || render_opt == "yes" || render_opt == "true";

    // ── Determine crawl mode ──────────────────────────────────────────
    // If the user didn't explicitly choose, fetch the first page and
    // analyse it to decide whether headless-browser rendering is needed.
    let use_render = if force_render {
        true
    } else if force_static {
        false
    } else {
        // Auto-detect
        eprintln!("info: Detecting site type...");
        let is_spa = crawler::detect_spa(&url).await;
        if is_spa {
            eprintln!("info: SPA detected (React/Vue/Angular) — switching to render mode");
        } else {
            eprintln!("info: Static site detected — plain HTTP crawl");
        }
        is_spa
    };

    // Verify render support is compiled in
    #[cfg(not(feature = "render"))]
    if use_render {
        eprintln!("error: SPA rendering is needed but this binary was built without the `render` feature.");
        eprintln!("       Rebuild with: cargo build --features render");
        process::exit(1);
    }

    // Load or create manifest
    let (manifest, loaded_existing_manifest) = if args.fresh {
        let _ = std::fs::create_dir_all(&output_dir);
        let mf = manifest::Manifest::new(url.as_str());
        let _ = mf.save_to(&output_dir);
        eprintln!("info: Fresh download, created new manifest");
        (Some(tokio::sync::Mutex::new(mf)), false)
    } else {
        match manifest::Manifest::load_from(&output_dir) {
            Ok(Some(mf)) => {
                eprintln!("info: Found existing manifest — incremental mode");
                (Some(tokio::sync::Mutex::new(mf)), true)
            }
            Ok(None) => {
                let _ = std::fs::create_dir_all(&output_dir);
                (Some(tokio::sync::Mutex::new(manifest::Manifest::new(url.as_str()))), false)
            }
            Err(e) => {
                eprintln!("warning: Failed to load manifest: {e}, starting fresh");
                let _ = std::fs::create_dir_all(&output_dir);
                (Some(tokio::sync::Mutex::new(manifest::Manifest::new(url.as_str()))), false)
            }
        }
    };

    println!("sitegrab v{}", env!("CARGO_PKG_VERSION"));
    println!("Mirroring: {}", url);
    println!("Output:    {}/", output_dir);
    println!("Workers:   {}", args.jobs);
    if use_render {
        println!("Mode:      SPA render (headless browser)");
    } else {
        println!("Mode:      plain HTTP crawl");
    }
    if !args.fresh && loaded_existing_manifest {
        println!("           incremental (use --fresh for full re-download)");
    }
    println!();

    let crawl_result = if use_render {
        #[cfg(feature = "render")]
        {
            crawler::crawl_spa(&url, &output_dir, args.jobs, manifest, args.robots, args.wait).await
        }
        #[cfg(not(feature = "render"))]
        {
            // Unreachable — guarded above
            Err(anyhow::anyhow!("render feature not enabled"))
        }
    } else {
        crawler::crawl(&url, &output_dir, args.jobs, manifest, args.robots).await
    };

    match crawl_result {
        Ok(stats) => {
            if !args.no_zip {
                let zip_path = format!("{}.zip", output_dir);
                if let Err(e) = archiver::create_zip(&output_dir, &zip_path) {
                    eprintln!("warning: Failed to create zip: {e}");
                }
            }
            if stats.errors > 0 {
                println!("⚠  {} errors (see above)", stats.errors);
            }
        }
        Err(e) => {
            eprintln!("error: Crawl failed: {e}");
            process::exit(1);
        }
    }
}
