use serde::Deserialize;

const SINGBOX_RELEASES_API: &str = "https://api.github.com/repos/SagerNet/sing-box/releases";

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

pub async fn fetch_releases(client: &reqwest::Client) -> Result<Vec<Release>, String> {
    let resp = client
        .get(SINGBOX_RELEASES_API)
        .header("User-Agent", "box-ui")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch releases: {e}"))?;

    resp.json::<Vec<Release>>()
        .await
        .map_err(|e| format!("Failed to parse releases: {e}"))
}

/// Download an asset with progress reporting via `AtomicU32` (permille: 1..=1000).
pub async fn download_asset_with_progress(
    client: &reqwest::Client,
    url: &str,
    dest: &std::path::Path,
    progress: &std::sync::atomic::AtomicU32,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncWriteExt;
    use tokio_stream::StreamExt;

    let resp = client
        .get(url)
        .header("User-Agent", "box-ui")
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("Download failed: HTTP {}", resp.status()));
    }

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| format!("Failed to create file: {e}"))?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download interrupted: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Failed to write: {e}"))?;

        downloaded += chunk.len() as u64;
        if total > 0 {
            let permille = ((downloaded as f64 / total as f64) * 1000.0) as u32;
            progress.store(permille.clamp(1, 1000), Ordering::Relaxed);
        } else {
            // Unknown total: pulse between 1 and 500
            let pulse = ((downloaded / 1024) % 500) as u32 + 1;
            progress.store(pulse, Ordering::Relaxed);
        }
    }

    file.flush()
        .await
        .map_err(|e| format!("Failed to flush: {e}"))?;

    progress.store(1000, Ordering::Relaxed);
    Ok(())
}

/// Extract the `sing-box` binary from a downloaded archive into `dest_dir/sing-box-<tag>`.
///
/// Supports `.tar.gz` and `.zip` archives. The binary is expected to be at
/// `<top-level-dir>/sing-box` (or `sing-box.exe` on Windows) inside the archive.
/// The temporary archive file is removed after successful extraction.
pub fn extract_kernel(
    archive_path: &std::path::Path,
    dest_dir: &std::path::Path,
    tag: &str,
) -> Result<std::path::PathBuf, String> {
    let name = archive_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    let dest = dest_dir.join(format!("sing-box-{tag}"));

    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        extract_tar_gz(archive_path, &dest)?;
    } else if name.ends_with(".zip") {
        extract_zip(archive_path, &dest)?;
    } else {
        return Err(format!("Unsupported archive format: {name}"));
    }

    // Set executable permission on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set permissions: {e}"))?;
    }

    // Remove the archive
    std::fs::remove_file(archive_path).ok();

    Ok(dest)
}

fn extract_tar_gz(
    archive_path: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), String> {
    let file =
        std::fs::File::open(archive_path).map_err(|e| format!("Failed to open archive: {e}"))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let target_name = if cfg!(target_os = "windows") {
        "sing-box.exe"
    } else {
        "sing-box"
    };

    for entry in archive.entries().map_err(|e| format!("Failed to read archive: {e}"))? {
        let mut entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("Failed to read path: {e}"))?;

        if path.file_name().and_then(|n| n.to_str()) == Some(target_name) {
            // Check it's a direct child of the top-level directory (depth == 2)
            if path.components().count() == 2 {
                let mut out = std::fs::File::create(dest)
                    .map_err(|e| format!("Failed to create file: {e}"))?;
                std::io::copy(&mut entry, &mut out)
                    .map_err(|e| format!("Failed to extract: {e}"))?;
                return Ok(());
            }
        }
    }

    Err(format!("Could not find {target_name} in archive"))
}

fn extract_zip(
    archive_path: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), String> {
    let file =
        std::fs::File::open(archive_path).map_err(|e| format!("Failed to open archive: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read zip: {e}"))?;

    let target_name = if cfg!(target_os = "windows") {
        "sing-box.exe"
    } else {
        "sing-box"
    };

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read entry: {e}"))?;
        let path = std::path::PathBuf::from(entry.name());

        if path.file_name().and_then(|n| n.to_str()) == Some(target_name)
            && path.components().count() == 2
        {
            let mut out =
                std::fs::File::create(dest).map_err(|e| format!("Failed to create file: {e}"))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| format!("Failed to extract: {e}"))?;
            return Ok(());
        }
    }

    Err(format!("Could not find {target_name} in archive"))
}

pub async fn fetch_remote_config(
    client: &reqwest::Client,
    url: &str,
    dest: &std::path::Path,
) -> Result<(), String> {
    let resp = client
        .get(url)
        .header("User-Agent", "box-ui")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch config: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Failed to fetch config: HTTP {}",
            resp.status()
        ));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response: {e}"))?;

    std::fs::write(dest, &bytes).map_err(|e| format!("Failed to write config: {e}"))?;

    Ok(())
}
