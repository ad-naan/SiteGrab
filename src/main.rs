use std::process;

use clap::Parser;
use url::Url;

mod archiver;
mod crawler;
mod manifest;
mod rewriter;

#[derive(Parser)]
#[command(
    name = "sitegrab",
    version,
    long_about = "\
sitegrab — Download a website for offline browsing.

Simple one-command mirroring:
  sitegrab https://example.com

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

    // Load or create manifest
    let manifest = if args.fresh {
        let mf = manifest::Manifest::new(url.as_str());
        std::fs::create_dir_all(&output_dir).ok();
        let _ = mf.save_to(&output_dir);
        eprintln!("info: Fresh download, created new manifest");
        Some(tokio::sync::Mutex::new(mf))
    } else {
        match manifest::Manifest::load_from(&output_dir) {
            Ok(Some(mf)) => {
                eprintln!("info: Found existing manifest — incremental mode");
                Some(tokio::sync::Mutex::new(mf))
            }
            Ok(None) => {
                let mf = manifest::Manifest::new(url.as_str());
                std::fs::create_dir_all(&output_dir).ok();
                Some(tokio::sync::Mutex::new(mf))
            }
            Err(e) => {
                eprintln!("warning: Failed to load manifest: {e}, starting fresh");
                let mf = manifest::Manifest::new(url.as_str());
                std::fs::create_dir_all(&output_dir).ok();
                Some(tokio::sync::Mutex::new(mf))
            }
        }
    };

    println!("sitegrab v{}", env!("CARGO_PKG_VERSION"));
    println!("Mirroring: {}", url);
    println!("Output:    {}/", output_dir);
    println!("Workers:   {}", args.jobs);
    if !args.fresh {
        if manifest::Manifest::load_from(&output_dir).ok().flatten().is_some() {
            println!("Mode:     incremental (use --fresh for full re-download)");
        }
    }
    println!();

    match crawler::crawl(&url, &output_dir, args.jobs, manifest, args.robots).await {
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
