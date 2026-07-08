use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use indicatif::{ProgressBar, ProgressStyle};

/// Create a ZIP archive of a directory.
pub fn create_zip(source_dir: &str, zip_path: &str) -> Result<()> {
    let source = Path::new(source_dir);
    if !source.is_dir() {
        anyhow::bail!("Source directory '{}' does not exist", source_dir);
    }

    let file = File::create(zip_path)
        .with_context(|| format!("Failed to create zip file '{}'", zip_path))?;

    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    // Count files first for progress bar
    let total = count_files(source);
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} files {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    pb.set_message("Zipping...");

    add_dir(&mut zip, source, source, &options, &pb)?;

    pb.finish_with_message("Done");
    zip.finish().context("Failed to finalize zip file")?;

    // Show zip size
    let zip_size = std::fs::metadata(zip_path).map(|m| m.len()).unwrap_or(0);
    println!("✓ Zip exported: {} ({})", zip_path, format_bytes(zip_size));

    Ok(())
}

fn count_files(dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files(&path);
            } else {
                count += 1;
            }
        }
    }
    count
}

fn add_dir<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    base: &Path,
    dir: &Path,
    options: &SimpleFileOptions,
    pb: &ProgressBar,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).context("Failed to read directory")? {
        let entry = entry.context("Failed to read entry")?;
        let path = entry.path();

        if path.is_dir() {
            add_dir(zip, base, &path, options, pb)?;
        } else {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            zip.start_file(&relative, *options)
                .with_context(|| format!("Failed to add '{}' to zip", relative))?;

            let mut f = File::open(&path)
                .with_context(|| format!("Failed to open '{}'", path.display()))?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)
                .with_context(|| format!("Failed to read '{}'", path.display()))?;
            zip.write_all(&buf)
                .with_context(|| format!("Failed to write '{}' to zip", relative))?;

            pb.inc(1);
        }
    }
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size > 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}
