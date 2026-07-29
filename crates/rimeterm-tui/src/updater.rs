//! Windows GitHub Release checker and verified MSI downloader.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

pub const RELEASES_API: &str =
    "https://api.github.com/repos/caozisheng/rimeterm/releases?per_page=30";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub body: Option<String>,
    pub html_url: String,
    pub published_at: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableRelease {
    pub version: Version,
    pub notes: String,
    pub html_url: String,
    pub published_at: Option<String>,
    pub msi: ReleaseAsset,
    pub checksums: ReleaseAsset,
}

pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("rimeterm/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .context("build GitHub HTTP client")
}

pub async fn check(client: &reqwest::Client, api_url: &str) -> Result<Option<AvailableRelease>> {
    let response = client
        .get(api_url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .context("request GitHub Releases")?
        .error_for_status()
        .context("GitHub Releases returned an error")?;
    let releases = response
        .json::<Vec<GitHubRelease>>()
        .await
        .context("decode GitHub Releases response")?;
    select_release(env!("CARGO_PKG_VERSION"), &releases)
}

pub fn select_release(
    current: &str,
    releases: &[GitHubRelease],
) -> Result<Option<AvailableRelease>> {
    let current = Version::parse(current).context("parse current rimeterm version")?;
    let accepts_prerelease = !current.pre.is_empty();
    let mut candidates: Vec<(Version, &GitHubRelease)> = releases
        .iter()
        .filter(|release| !release.draft && (accepts_prerelease || !release.prerelease))
        .filter_map(|release| {
            Version::parse(release.tag_name.trim_start_matches('v'))
                .ok()
                .filter(|version| version > &current)
                .map(|version| (version, release))
        })
        .collect();
    candidates.sort_unstable_by(|left, right| right.0.cmp(&left.0));
    let Some((version, release)) = candidates.into_iter().next() else {
        return Ok(None);
    };

    let msi_name = format!("rimeterm-{version}-x86_64.msi");
    let msi = exact_asset(release, &msi_name)?.clone();
    let checksums = exact_asset(release, "SHA256SUMS")?.clone();
    Ok(Some(AvailableRelease {
        version,
        notes: release.body.clone().unwrap_or_default(),
        html_url: release.html_url.clone(),
        published_at: release.published_at.clone(),
        msi,
        checksums,
    }))
}

fn exact_asset<'a>(release: &'a GitHubRelease, name: &str) -> Result<&'a ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| anyhow!("release {} is missing asset {name}", release.tag_name))
}

pub fn checksum_for<'a>(manifest: &'a str, asset_name: &str) -> Result<&'a str> {
    for line in manifest.lines() {
        let mut fields = line.split_whitespace();
        let Some(digest) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else { continue };
        if name.trim_start_matches('*') == asset_name
            && digest.len() == 64
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(digest);
        }
    }
    bail!("SHA256SUMS has no valid entry for {asset_name}")
}

pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    verify_digest(format!("{:x}", Sha256::digest(bytes)), expected)
}

fn verify_digest(actual: String, expected: &str) -> Result<()> {
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        bail!("SHA-256 mismatch: expected {expected}, got {actual}")
    }
}

pub async fn download_verified<F>(
    client: &reqwest::Client,
    release: &AvailableRelease,
    dest_dir: &Path,
    mut on_progress: F,
) -> Result<PathBuf>
where
    F: FnMut(u64, u64),
{
    tokio::fs::create_dir_all(dest_dir)
        .await
        .with_context(|| format!("create update directory {}", dest_dir.display()))?;
    let manifest = client
        .get(&release.checksums.browser_download_url)
        .send()
        .await
        .context("download SHA256SUMS")?
        .error_for_status()
        .context("SHA256SUMS download returned an error")?
        .text()
        .await
        .context("read SHA256SUMS")?;
    let expected = checksum_for(&manifest, &release.msi.name)?.to_owned();

    let final_path = dest_dir.join(&release.msi.name);
    let part_path = final_path.with_extension("msi.part");
    let response = client
        .get(&release.msi.browser_download_url)
        .send()
        .await
        .context("download MSI")?
        .error_for_status()
        .context("MSI download returned an error")?;
    let total = response.content_length().unwrap_or(release.msi.size);
    let mut file = tokio::fs::File::create(&part_path)
        .await
        .with_context(|| format!("create {}", part_path.display()))?;
    let mut stream = response.bytes_stream();
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read MSI download")?;
        file.write_all(&chunk).await.context("write MSI download")?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        on_progress(downloaded, total);
    }
    file.flush().await.context("flush MSI download")?;
    drop(file);

    if let Err(error) = verify_digest(format!("{:x}", hasher.finalize()), &expected) {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(error);
    }
    if tokio::fs::try_exists(&final_path).await.unwrap_or(false) {
        tokio::fs::remove_file(&final_path)
            .await
            .with_context(|| format!("replace {}", final_path.display()))?;
    }
    tokio::fs::rename(&part_path, &final_path)
        .await
        .with_context(|| format!("finalize {}", final_path.display()))?;
    Ok(final_path)
}

pub fn update_directory(version: &Version) -> PathBuf {
    std::env::temp_dir()
        .join("rimeterm-update")
        .join(version.to_string())
}

pub fn installer_command(path: &Path) -> (&'static str, Vec<String>) {
    (
        "msiexec.exe",
        vec!["/i".to_string(), path.display().to_string()],
    )
}

pub fn launch_installer(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;

    let (program, args) = installer_command(path);
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .with_context(|| format!("launch Windows Installer for {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.invalid/{name}"),
            size: 42,
        }
    }

    fn release(tag: &str, prerelease: bool, draft: bool) -> GitHubRelease {
        let version = tag.trim_start_matches('v');
        GitHubRelease {
            tag_name: tag.to_string(),
            body: Some(format!("notes for {tag}")),
            html_url: format!("https://github.com/caozisheng/rimeterm/releases/tag/{tag}"),
            published_at: Some("2026-07-29T00:00:00Z".to_string()),
            draft,
            prerelease,
            assets: vec![
                asset(&format!("rimeterm-{version}-x86_64.msi")),
                asset("SHA256SUMS"),
            ],
        }
    }

    #[test]
    fn stable_build_selects_highest_newer_stable_release() {
        let releases = vec![
            release("v0.3.0-rc.1", true, false),
            release("v0.2.4", false, false),
            release("v0.2.3", false, false),
        ];

        let selected = select_release("0.2.2", &releases).unwrap().unwrap();

        assert_eq!(selected.version.to_string(), "0.2.4");
    }

    #[test]
    fn prerelease_build_follows_prerelease_channel() {
        let releases = vec![
            release("v0.3.0-rc.2", true, false),
            release("v0.2.4", false, false),
        ];

        let selected = select_release("0.3.0-rc.1", &releases).unwrap().unwrap();

        assert_eq!(selected.version.to_string(), "0.3.0-rc.2");
    }

    #[test]
    fn ignores_drafts_malformed_tags_and_non_newer_versions() {
        let releases = vec![
            release("nightly", false, false),
            release("v9.0.0", false, true),
            release("v0.2.2", false, false),
        ];

        assert!(select_release("0.2.2", &releases).unwrap().is_none());
    }

    #[test]
    fn requires_exact_msi_and_checksum_assets() {
        let mut candidate = release("v0.2.3", false, false);
        candidate.assets[0].name = "rimeterm-0.2.3-x86_64-pc-windows-msvc.zip".into();

        let error = select_release("0.2.2", &[candidate]).unwrap_err();

        assert!(error.to_string().contains("rimeterm-0.2.3-x86_64.msi"));
    }

    #[test]
    fn parses_common_sha256sums_formats_for_exact_asset() {
        let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let manifest = format!("{digest}  other.zip\n{digest} *rimeterm-0.2.3-x86_64.msi\n");

        assert_eq!(
            checksum_for(&manifest, "rimeterm-0.2.3-x86_64.msi").unwrap(),
            digest
        );
        assert!(checksum_for(&manifest, "missing.msi").is_err());
    }

    #[test]
    fn verifies_sha256_and_rejects_mismatch() {
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

        assert!(verify_sha256(b"hello", expected).is_ok());
        assert!(verify_sha256(b"tampered", expected).is_err());
    }

    #[tokio::test]
    async fn downloads_msi_stream_and_reports_progress() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let msi = b"verified installer bytes".to_vec();
        let digest = format!("{:x}", Sha256::digest(&msi));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");
        let server_msi = msi.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 2048];
                let read = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let (content_type, body) = if request.starts_with("GET /SHA256SUMS ") {
                    (
                        "text/plain",
                        format!("{digest}  rimeterm-9.9.9-x86_64.msi\n").into_bytes(),
                    )
                } else {
                    ("application/octet-stream", server_msi.clone())
                };
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
            }
        });
        let release = AvailableRelease {
            version: Version::parse("9.9.9").unwrap(),
            notes: String::new(),
            html_url: base.clone(),
            published_at: None,
            msi: ReleaseAsset {
                name: "rimeterm-9.9.9-x86_64.msi".into(),
                browser_download_url: format!("{base}/installer.msi"),
                size: msi.len() as u64,
            },
            checksums: ReleaseAsset {
                name: "SHA256SUMS".into(),
                browser_download_url: format!("{base}/SHA256SUMS"),
                size: 0,
            },
        };
        let temp = tempfile::tempdir().unwrap();
        let mut progress = Vec::new();

        let path = download_verified(&client().unwrap(), &release, temp.path(), |done, total| {
            progress.push((done, total));
        })
        .await
        .unwrap();

        assert_eq!(tokio::fs::read(path).await.unwrap(), msi);
        assert_eq!(
            progress.last().copied(),
            Some((msi.len() as u64, msi.len() as u64))
        );
        server.await.unwrap();
    }

    #[test]
    fn installer_command_uses_quiet_free_interactive_msi_arguments() {
        let path = std::path::Path::new(r"C:\Temp Folder\rimeterm.msi");

        let (program, args) = installer_command(path);

        assert_eq!(program, "msiexec.exe");
        assert_eq!(args, vec!["/i".to_string(), path.display().to_string()]);
    }
}
