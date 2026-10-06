//! A track's audio: yt-dlp resolves the signed googlevideo URL, and bounded
//! range requests fetch the file.
//!
//! The app keeps its own copy of yt-dlp, downloaded from the official
//! releases and refreshed every few days, because YouTube breaks extractors
//! often and yt-dlp ships the fixes within days. A copy on PATH covers the
//! time before the first download lands.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use reqwest::header::{CONTENT_RANGE, RANGE};

use crate::auth::Session;

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

/// Always the best audio YouTube offers: Opus 774 (~256 kbps) for Premium,
/// else Opus 251 or AAC 140, by yt-dlp's own ranking. Over plain HTTP, since
/// the HLS variants need a segment fetcher this app does not have.
const FORMAT: &str = "bestaudio[protocol^=http]";
/// Nor does it fetch the HLS manifests at all: about half a second a track.
const SKIP_HLS: &str = "youtube:skip=hls";
/// With the session, the TV client alone: it offers Premium's Opus 774 and
/// costs a second less than yt-dlp's default pair. The pair is the fallback,
/// for a track the TV client does not play.
const TV_CLIENT: &str = "youtube:skip=hls;player_client=tv_downgraded";

/// Warm-ups that run at once, and the asks waiting beyond them.
const WARMERS: usize = 2;
const WARM_QUEUE: usize = 4;

/// googlevideo throttles an open-ended range to a trickle (~30 KB/s) and
/// serves bounded ones at full speed, so every request names its end.
const CHUNK: u64 = 4 * 1024 * 1024;
/// The first chunk: a few seconds of audio, enough to start playing.
const FIRST_CHUNK: u64 = 512 * 1024;
const RESOLVED_FOR: Duration = Duration::from_secs(30 * 60);
const LANES: usize = 6;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(12);
const CHUNK_TRIES: u32 = 4;

/// Apps opened from the Dock or a launcher do not get the shell's PATH.
const EXTRA_DIRS: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

pub struct Ytdlp {
    bin_dir: PathBuf,
    /// Whether resolving with the session still works this run. YouTube may
    /// answer a signed-in yt-dlp as a bot with no audio at all; after that,
    /// tracks resolve anonymously, at the quality anyone gets.
    session_works: AtomicBool,
}

impl Ytdlp {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            bin_dir: data_dir.join("bin"),
            session_works: AtomicBool::new(true),
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

pub struct Source {
    url: String,
    headers: reqwest::header::HeaderMap,
}

/// Resolves with the session first: Premium's high-quality formats are only
/// offered to the signed-in account. Anonymously when that fails.
async fn resolve(
    ytdlp: &Ytdlp,
    video_id: &str,
    session: Option<&Session>,
    scratch: &Path,
) -> Result<Source> {
    if !is_video_id(video_id) {
        bail!("not a video id: {video_id}");
    }
    if let Some(session) = session.filter(|_| ytdlp.session_works.load(Ordering::Relaxed)) {
        let file = scratch.join(format!("cookies-{video_id}.txt"));
        let result = match write_private(&file, &session.to_netscape()).await {
            Ok(()) => match resolve_with(ytdlp, video_id, Some(&file), TV_CLIENT).await {
                Ok(source) => Ok(source),
                Err(error) => {
                    log::info!("{video_id}: not through the TV client ({error:#})");
                    resolve_with(ytdlp, video_id, Some(&file), SKIP_HLS).await
                }
            },
            Err(error) => Err(error.into()),
        };
        let _ = tokio::fs::remove_file(&file).await;
        match result {
            Ok(source) => return Ok(source),
            Err(error) => {
                log::info!(
                    "{video_id}: no audio with the session ({error:#}); resolving anonymously from now on"
                );
                ytdlp.session_works.store(false, Ordering::Relaxed);
            }
        }
    }
    resolve_with(ytdlp, video_id, None, SKIP_HLS).await
}

/// Writes a file only this user can read.
async fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await?;
    tokio::io::AsyncWriteExt::write_all(&mut file, text.as_bytes()).await
}

async fn resolve_with(
    ytdlp: &Ytdlp,
    video_id: &str,
    cookies: Option<&Path>,
    extractor_args: &str,
) -> Result<Source> {
    let mut command = ytdlp.command();
    if let Some(cookies) = cookies {
        command.arg("--cookies").arg(cookies);
    }
    command
        .args(["-j", "-f", FORMAT, "--extractor-args", extractor_args])
        .args(["--no-playlist", "--no-warnings", "--"])
        .arg(format!("https://www.youtube.com/watch?v={video_id}"));
    let started = Instant::now();
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
    log::info!(
        "{video_id}: format {} ({} at {} kbps){}, resolved in {:.1}s",
        json["format_id"].as_str().unwrap_or("?"),
        json["acodec"].as_str().unwrap_or("?"),
        json["abr"].as_f64().unwrap_or(0.0).round(),
        if cookies.is_some() { ", signed in" } else { "" },
        started.elapsed().as_secs_f64(),
    );
    let url = json["url"]
        .as_str()
        .context("yt-dlp gave no URL")?
        .to_string();
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
    Ok(Source { url, headers })
}

/// Resolved streams, kept for a while: a googlevideo URL stays good for
/// hours, and a fresh yt-dlp run costs about two seconds of the wait before
/// a track sounds.
pub struct Resolver {
    ytdlp: Arc<Ytdlp>,
    scratch: PathBuf,
    resolved: Mutex<HashMap<String, (Instant, Arc<Source>)>>,
    /// One resolve per track at a time; a second asker waits for the first.
    gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Tracks to resolve ahead of a play, the newest ask first.
    warm_queue: Mutex<VecDeque<(String, Option<Session>)>>,
    warmers: AtomicUsize,
}

impl Resolver {
    pub fn new(ytdlp: Arc<Ytdlp>, scratch: PathBuf) -> Self {
        Self {
            ytdlp,
            scratch,
            resolved: Mutex::default(),
            gates: Mutex::default(),
            warm_queue: Mutex::default(),
            warmers: AtomicUsize::new(0),
        }
    }

    pub async fn resolve(&self, video_id: &str, session: Option<&Session>) -> Result<Arc<Source>> {
        let gate = lock(&self.gates)
            .entry(video_id.to_string())
            .or_default()
            .clone();
        let _turn = gate.lock().await;
        if let Some((at, source)) = lock(&self.resolved).get(video_id)
            && at.elapsed() < RESOLVED_FOR
        {
            return Ok(source.clone());
        }
        let source = Arc::new(resolve(&self.ytdlp, video_id, session, &self.scratch).await?);
        lock(&self.resolved).insert(video_id.to_string(), (Instant::now(), source.clone()));
        Ok(source)
    }

    /// Drops a URL googlevideo no longer takes.
    fn forget(&self, video_id: &str) {
        lock(&self.resolved).remove(video_id);
    }

    fn is_resolved(&self, video_id: &str) -> bool {
        lock(&self.resolved)
            .get(video_id)
            .is_some_and(|(at, _)| at.elapsed() < RESOLVED_FOR)
    }

    /// Resolves ahead of a likely play: the track under the pointer or just
    /// pressed, the top of a list just opened. The newest ask goes first and
    /// the oldest fall off, and only `WARMERS` run at once, so a sweep down a
    /// list does not start a yt-dlp per row.
    pub fn warm(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        video_id: String,
        session: Option<Session>,
    ) {
        if self.is_resolved(&video_id) {
            return;
        }
        {
            let mut queue = lock(&self.warm_queue);
            queue.retain(|(id, _)| *id != video_id);
            queue.push_front((video_id, session));
            queue.truncate(WARM_QUEUE);
        }
        if !self.claim_warmer() {
            return;
        }
        let resolver = self.clone();
        runtime.spawn(async move {
            loop {
                while let Some((video_id, session)) = resolver.next_warm() {
                    if let Err(error) = resolver.resolve(&video_id, session.as_ref()).await {
                        log::info!("{video_id}: warm-up failed: {error:#}");
                    }
                }
                resolver.warmers.fetch_sub(1, Ordering::AcqRel);
                // An ask that came in just as this warmer stopped.
                if lock(&resolver.warm_queue).is_empty() || !resolver.claim_warmer() {
                    break;
                }
            }
        });
    }

    fn next_warm(&self) -> Option<(String, Option<Session>)> {
        lock(&self.warm_queue).pop_front()
    }

    fn claim_warmer(&self) -> bool {
        self.warmers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < WARMERS).then_some(n + 1)
            })
            .is_ok()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where chunk `index` starts and ends (exclusive). The first is small, so
/// the first sound needs only a fraction of a second of download.
fn chunk_bounds(index: usize, size: u64) -> (u64, u64) {
    let start = if index == 0 {
        0
    } else {
        FIRST_CHUNK + (index as u64 - 1) * CHUNK
    };
    let end = if index == 0 {
        FIRST_CHUNK
    } else {
        start + CHUNK
    };
    (start.min(size), end.min(size))
}

fn chunk_of(position: u64) -> usize {
    if position < FIRST_CHUNK {
        0
    } else {
        1 + ((position - FIRST_CHUNK) / CHUNK) as usize
    }
}

fn chunk_count(size: u64) -> usize {
    if size == 0 { 1 } else { chunk_of(size - 1) + 1 }
}

#[derive(Default)]
struct Bytes {
    /// Known once the first chunk answers with the file's length.
    size: Option<u64>,
    data: Vec<u8>,
    have: Vec<bool>,
    claimed: Vec<bool>,
    /// The chunk the decoder is waiting on, fetched next.
    wanted: usize,
    error: Option<String>,
    cancelled: bool,
}

/// A track's audio file arriving in bounded chunks over parallel requests,
/// readable while it arrives. A read waits only for the chunk it needs, and
/// a seek ahead makes that chunk the next one fetched.
#[derive(Default)]
pub struct Download {
    bytes: Mutex<Bytes>,
    arrived: Condvar,
}

impl Download {
    pub fn start(
        runtime: &tokio::runtime::Handle,
        http: reqwest::Client,
        resolver: Arc<Resolver>,
        video_id: String,
        session: Option<Session>,
    ) -> Arc<Self> {
        let download = Arc::new(Self::default());
        let task = download.clone();
        runtime.spawn(async move {
            if let Err(error) = task
                .run(&http, &resolver, &video_id, session.as_ref())
                .await
            {
                log::warn!("{video_id}: {error:#}");
                task.update(|bytes| bytes.error = Some(format!("{error:#}")));
            }
        });
        download
    }

    async fn run(
        self: &Arc<Self>,
        http: &reqwest::Client,
        resolver: &Resolver,
        video_id: &str,
        session: Option<&Session>,
    ) -> Result<()> {
        let mut source = resolver.resolve(video_id, session).await?;
        let (size, first) = match fetch_range(http, &source, 0, FIRST_CHUNK).await {
            Err(error) if format!("{error:#}").contains("403") => {
                // The signed URL lapsed or belongs to another client: ask again.
                resolver.forget(video_id);
                source = resolver.resolve(video_id, session).await?;
                fetch_range(http, &source, 0, FIRST_CHUNK).await?
            }
            other => other?,
        };
        let size = size.unwrap_or(first.len() as u64);
        self.update(|bytes| {
            let count = chunk_count(size);
            bytes.size = Some(size);
            bytes.data = vec![0; size as usize];
            bytes.have = vec![false; count];
            bytes.claimed = vec![false; count];
            bytes.claimed[0] = true;
            let len = first.len().min(bytes.data.len());
            bytes.data[..len].copy_from_slice(&first[..len]);
            bytes.have[0] = true;
        });
        let lanes = (0..LANES).map(|_| self.lane(http, &source));
        futures_util::future::try_join_all(lanes).await?;
        Ok(())
    }

    async fn lane(&self, http: &reqwest::Client, source: &Source) -> Result<()> {
        while let Some((index, start, end)) = self.claim() {
            let (_, data) = fetch_range(http, source, start, end).await?;
            self.update(|bytes| {
                let end = (start as usize + data.len()).min(bytes.data.len());
                bytes.data[start as usize..end].copy_from_slice(&data[..end - start as usize]);
                bytes.have[index] = true;
            });
        }
        Ok(())
    }

    /// The next chunk to fetch: the one the decoder waits on, then what
    /// follows it, then whatever is left.
    fn claim(&self) -> Option<(usize, u64, u64)> {
        let mut bytes = lock(&self.bytes);
        if bytes.cancelled || bytes.error.is_some() {
            return None;
        }
        let size = bytes.size?;
        let count = bytes.have.len();
        let wanted = bytes.wanted.min(count - 1);
        let index = (wanted..count)
            .chain(0..wanted)
            .find(|&i| !bytes.claimed[i])?;
        bytes.claimed[index] = true;
        let (start, end) = chunk_bounds(index, size);
        Some((index, start, end))
    }

    fn update(&self, change: impl FnOnce(&mut Bytes)) {
        change(&mut lock(&self.bytes));
        self.arrived.notify_all();
    }

    /// The start of the file is in: playback can begin at once.
    pub fn ready(&self) -> bool {
        lock(&self.bytes).have.first().copied().unwrap_or(false)
    }

    pub fn cancel(&self) {
        self.update(|bytes| bytes.cancelled = true);
    }

    pub fn reader(self: &Arc<Self>) -> Reader {
        Reader {
            download: self.clone(),
            position: 0,
        }
    }

    /// Waits until `ready` says what to do with the bytes in hand.
    fn wait<T>(
        &self,
        mut ready: impl FnMut(&mut Bytes) -> Option<std::io::Result<T>>,
    ) -> std::io::Result<T> {
        let mut bytes = lock(&self.bytes);
        loop {
            if let Some(error) = &bytes.error {
                return Err(std::io::Error::other(error.clone()));
            }
            if bytes.cancelled {
                return Err(std::io::Error::other("download cancelled"));
            }
            if let Some(result) = ready(&mut bytes) {
                return result;
            }
            bytes = self
                .arrived
                .wait_timeout(bytes, Duration::from_millis(500))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// The file as symphonia reads it: blocking until each part arrives. The
/// decoder runs on a thread of its own, so the wait never reaches the
/// device callback.
pub struct Reader {
    download: Arc<Download>,
    position: u64,
}

impl Reader {
    fn size(&self) -> std::io::Result<u64> {
        self.download.wait(|bytes| bytes.size.map(Ok))
    }
}

impl std::io::Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let position = self.position;
        let read = self.download.wait(|bytes| {
            let size = bytes.size?;
            if position >= size || buf.is_empty() {
                return Some(Ok(0));
            }
            let index = chunk_of(position);
            if !bytes.have[index] {
                bytes.wanted = index;
                return None;
            }
            let end = chunk_bounds(index, size).1;
            let len = buf.len().min((end - position) as usize);
            buf[..len].copy_from_slice(&bytes.data[position as usize..position as usize + len]);
            Some(Ok(len))
        })?;
        self.position += read as u64;
        Ok(read)
    }
}

impl std::io::Seek for Reader {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            std::io::SeekFrom::Start(at) => Some(at),
            std::io::SeekFrom::End(delta) => self.size()?.checked_add_signed(delta),
            std::io::SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
        };
        self.position = target.ok_or_else(|| std::io::Error::other("seek before the start"))?;
        Ok(self.position)
    }
}

impl symphonia::core::io::MediaSource for Reader {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.size().ok()
    }
}

/// Bytes `start..end` (exclusive), and the file's total length when the
/// answer names it.
async fn fetch_range(
    http: &reqwest::Client,
    source: &Source,
    start: u64,
    end: u64,
) -> Result<(Option<u64>, Vec<u8>)> {
    let mut last = None;
    for attempt in 0..CHUNK_TRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
        }
        let request = http
            .get(&source.url)
            .headers(source.headers.clone())
            .header(RANGE, format!("bytes={start}-{}", end.saturating_sub(1)))
            .timeout(CHUNK_TIMEOUT);
        match request.send().await.and_then(|r| r.error_for_status()) {
            Ok(response) => {
                let total = response
                    .headers()
                    .get(CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.rsplit('/').next())
                    .and_then(|v| v.parse().ok());
                match response.bytes().await {
                    Ok(bytes) => return Ok((total, bytes.to_vec())),
                    Err(error) => last = Some(error),
                }
            }
            Err(error) if error.status().is_some_and(|s| s.as_u16() == 403) => {
                bail!("googlevideo answered 403")
            }
            Err(error) => last = Some(error.without_url()),
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
    fn chunks_cover_the_file_once() {
        for size in [
            1,
            FIRST_CHUNK - 1,
            FIRST_CHUNK,
            FIRST_CHUNK + 1,
            3 * CHUNK + 17,
        ] {
            let mut next = 0;
            for index in 0..chunk_count(size) {
                let (start, end) = chunk_bounds(index, size);
                assert_eq!(start, next, "size {size} chunk {index}");
                assert!(end > start);
                assert_eq!(chunk_of(start), index);
                assert_eq!(chunk_of(end - 1), index);
                next = end;
            }
            assert_eq!(next, size);
        }
    }

    #[test]
    fn video_ids_are_checked_before_yt_dlp_sees_them() {
        assert!(is_video_id("dQw4w9WgXcQ"));
        assert!(!is_video_id("--exec=rm -rf"));
        assert!(!is_video_id("short"));
    }
}
