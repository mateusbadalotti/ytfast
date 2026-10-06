//! A track's audio: yt-dlp resolves the signed googlevideo URL, and bounded
//! range requests fetch the file.
//!
//! The app keeps its own copy of yt-dlp, downloaded from the official
//! releases and refreshed every few days, because YouTube breaks extractors
//! often and yt-dlp ships the fixes within days. A copy on PATH covers the
//! time before the first download lands.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, TryStreamExt};
use reqwest::header::RANGE;

#[cfg(target_os = "macos")]
const DOWNLOAD_URL: &str =
    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos.zip";
#[cfg(windows)]
const DOWNLOAD_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
#[cfg(all(unix, not(target_os = "macos")))]
const DOWNLOAD_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";

/// macOS takes the directory build: the one-file build unpacks ~100 dylibs
/// into a new temporary directory on every launch, and the signature checks
/// that follow cost about 30 seconds per track.
#[cfg(target_os = "macos")]
const MANAGED: &str = "yt-dlp_macos/yt-dlp_macos";
#[cfg(windows)]
const MANAGED: &str = "yt-dlp.exe";
#[cfg(all(unix, not(target_os = "macos")))]
const MANAGED: &str = "yt-dlp";

const REFRESH_AFTER: Duration = Duration::from_secs(72 * 60 * 60);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);

/// Symphonia decodes AAC, not Opus, so only the M4A formats will do.
const FORMAT: &str = "bestaudio[ext=m4a][protocol^=http]/bestaudio[acodec^=mp4a][protocol^=http]";

/// googlevideo throttles an open-ended range to a trickle (~30 KB/s) and
/// serves bounded ones at full speed, so every request names its end.
const CHUNK: u64 = 4 * 1024 * 1024;
const LANES: usize = 6;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(12);
const CHUNK_TRIES: u32 = 4;

/// Apps opened from the Dock or a launcher do not get the shell's PATH.
const EXTRA_DIRS: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

pub struct Ytdlp {
    bin_dir: PathBuf,
}

impl Ytdlp {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            bin_dir: data_dir.join("bin"),
        }
    }

    fn managed(&self) -> PathBuf {
        self.bin_dir.join(MANAGED)
    }

    fn program(&self) -> PathBuf {
        let managed = self.managed();
        if managed.exists() {
            return managed;
        }
        find_on_path("yt-dlp").unwrap_or_else(|| PathBuf::from("yt-dlp"))
    }

    /// yt-dlp with the JavaScript runtimes it needs for YouTube's player.
    pub fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(self.program());
        for runtime in ["deno", "node", "bun"] {
            if let Some(path) = find_on_path(runtime) {
                command
                    .arg("--js-runtimes")
                    .arg(format!("{runtime}:{}", path.display()));
            }
        }
        command.kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        command
    }

    /// Downloads the managed copy when it is missing or a few days old.
    pub async fn ensure(&self, http: &reqwest::Client) -> Result<()> {
        let age = std::fs::metadata(self.managed())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok());
        if age.is_some_and(|age| age < REFRESH_AFTER) {
            return Ok(());
        }
        log::info!("downloading yt-dlp");
        let bytes = http
            .get(DOWNLOAD_URL)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        tokio::fs::create_dir_all(&self.bin_dir).await?;
        let part = self.bin_dir.join("download.part");
        tokio::fs::write(&part, &bytes).await?;
        install(&part, &self.bin_dir).await?;
        let _ = tokio::fs::remove_file(&part).await;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
async fn install(part: &Path, bin_dir: &Path) -> Result<()> {
    let staging = bin_dir.join("staging");
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let status = tokio::process::Command::new("/usr/bin/unzip")
        .arg("-q")
        .arg(part)
        .arg("-d")
        .arg(&staging)
        .status()
        .await?;
    if !status.success() || !staging.join("yt-dlp_macos").exists() {
        bail!("the yt-dlp archive did not unpack");
    }
    let target = bin_dir.join("yt-dlp_macos");
    let _ = tokio::fs::remove_dir_all(&target).await;
    tokio::fs::rename(&staging, &target).await?;
    // touch: the directory's age is what `ensure` reads.
    let entry = target.join("yt-dlp_macos");
    std::fs::File::options()
        .write(true)
        .open(&entry)?
        .set_modified(SystemTime::now())?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
async fn install(part: &Path, bin_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(part, std::fs::Permissions::from_mode(0o755)).await?;
    }
    tokio::fs::rename(part, bin_dir.join(MANAGED)).await?;
    Ok(())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(EXTRA_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

/// A YouTube video id: eleven URL-safe base64 characters. Checked before an
/// id reaches yt-dlp's command line.
pub fn is_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

struct Source {
    url: String,
    size: u64,
    headers: reqwest::header::HeaderMap,
}

async fn resolve(ytdlp: &Ytdlp, video_id: &str) -> Result<Source> {
    if !is_video_id(video_id) {
        bail!("not a video id: {video_id}");
    }
    let mut command = ytdlp.command();
    command
        .args(["-j", "-f", FORMAT, "--no-playlist", "--no-warnings", "--"])
        .arg(format!("https://www.youtube.com/watch?v={video_id}"));
    let output = tokio::time::timeout(RESOLVE_TIMEOUT, command.output())
        .await
        .context("yt-dlp took too long")?
        .context("could not run yt-dlp; is it installed?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("failed");
        bail!("yt-dlp: {reason}");
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let url = json["url"]
        .as_str()
        .context("yt-dlp gave no URL")?
        .to_string();
    let size = json["filesize"]
        .as_u64()
        .or(json["filesize_approx"].as_u64())
        .unwrap_or(0);
    // googlevideo binds the signed URL to the client that asked for it, so
    // the download sends what yt-dlp says the URL expects.
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in json["http_headers"].as_object().into_iter().flatten() {
        if let (Ok(name), Some(Ok(value))) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            value.as_str().map(reqwest::header::HeaderValue::from_str),
        ) {
            headers.insert(name, value);
        }
    }
    Ok(Source { url, size, headers })
}

/// The whole audio file of a track, in memory (a few megabytes of AAC).
pub async fn fetch(http: &reqwest::Client, ytdlp: &Ytdlp, video_id: &str) -> Result<Vec<u8>> {
    let source = resolve(ytdlp, video_id).await?;
    match download(http, &source).await {
        Err(error) if format!("{error:#}").contains("403") => {
            // The signed URL lapsed or belongs to another client: ask again.
            let source = resolve(ytdlp, video_id).await?;
            download(http, &source).await
        }
        other => other,
    }
}

async fn download(http: &reqwest::Client, source: &Source) -> Result<Vec<u8>> {
    if source.size == 0 {
        return download_sequential(http, source).await;
    }
    let ranges = (0..source.size)
        .step_by(CHUNK as usize)
        .map(|start| (start, (start + CHUNK - 1).min(source.size - 1)));
    let chunks: Vec<Vec<u8>> = futures_util::stream::iter(ranges)
        .map(|(start, end)| chunk(http, source, start, end))
        .buffered(LANES)
        .try_collect()
        .await?;
    Ok(chunks.concat())
}

/// Without a size there is nothing to split, so bounded ranges go one after
/// another until one comes back short.
async fn download_sequential(http: &reqwest::Client, source: &Source) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let start = out.len() as u64;
        let part = chunk(http, source, start, start + CHUNK - 1).await?;
        let short = (part.len() as u64) < CHUNK;
        out.extend_from_slice(&part);
        if short {
            return Ok(out);
        }
    }
}

async fn chunk(http: &reqwest::Client, source: &Source, start: u64, end: u64) -> Result<Vec<u8>> {
    let mut last = None;
    for attempt in 0..CHUNK_TRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
        }
        let request = http
            .get(&source.url)
            .headers(source.headers.clone())
            .header(RANGE, format!("bytes={start}-{end}"))
            .timeout(CHUNK_TIMEOUT);
        match request.send().await.and_then(|r| r.error_for_status()) {
            Ok(response) => match response.bytes().await {
                Ok(bytes) => return Ok(bytes.to_vec()),
                Err(error) => last = Some(error),
            },
            Err(error) if error.status().is_some_and(|s| s.as_u16() == 403) => {
                bail!("googlevideo answered 403")
            }
            Err(error) => last = Some(error),
        }
    }
    Err(last
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow::anyhow!("download failed")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_ids_are_checked_before_yt_dlp_sees_them() {
        assert!(is_video_id("dQw4w9WgXcQ"));
        assert!(!is_video_id("--exec=rm -rf"));
        assert!(!is_video_id("short"));
    }
}
