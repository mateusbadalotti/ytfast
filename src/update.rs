//! Updates from the GitHub releases, on macOS. At launch the newest release's
//! app replaces this one in place, checked first against the release's
//! checksum, bundle id, version and signature; the app then reopens into it,
//! or, when something already plays, runs it the next time it opens.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

const LATEST: &str = "https://api.github.com/repos/mateusbadalotti/ytfast/releases/latest";
const ASSET: &str = "ytfast-macos.zip";
const BUNDLE_NAME: &str = "ytfast.app";
const BUNDLE_ID: &str = "com.github.mateusbadalotti.ytfast";
/// Every request names itself: GitHub's API refuses one that does not.
const USER_AGENT: &str = concat!("ytfast/", env!("CARGO_PKG_VERSION"));

/// A newer release, found but not yet installed.
pub struct Release {
    pub version: String,
    url: String,
    digest: Option<String>,
}

/// The newest release, when it is newer than this app. Nothing is offered
/// outside an app bundle, as under `cargo run`.
pub async fn newer_release(http: &reqwest::Client) -> Result<Option<Release>> {
    if running_bundle().is_none() {
        return Ok(None);
    }
    let release: Value = http
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let tag = release["tag_name"]
        .as_str()
        .context("a release without a tag")?;
    let version = tag.trim_start_matches('v');
    if !newer(version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let asset = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|asset| asset["name"] == ASSET)
        .with_context(|| format!("{tag} has no {ASSET}"))?;
    let url = asset["browser_download_url"]
        .as_str()
        .context("the asset has no download link")?;
    Ok(Some(Release {
        version: version.to_string(),
        url: url.to_string(),
        digest: asset["digest"].as_str().map(str::to_string),
    }))
}

/// Downloads `release` and puts it in place of this app; returns the bundle,
/// which then holds the new version.
pub async fn install_release(http: &reqwest::Client, release: &Release) -> Result<PathBuf> {
    let bundle = running_bundle().context("not running from an app bundle")?;
    let archive = http
        .get(&release.url)
        .header("User-Agent", USER_AGENT)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    // Beside the bundle, so the swap is a rename on one volume.
    let staging = bundle.with_file_name(format!(".ytfast-update-{}", std::process::id()));
    let result = install(
        &bundle,
        &staging,
        &archive,
        release.digest.as_deref(),
        &release.version,
    )
    .await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    result?;
    Ok(bundle)
}

async fn install(
    bundle: &Path,
    staging: &Path,
    archive: &[u8],
    digest: Option<&str>,
    version: &str,
) -> Result<()> {
    tokio::fs::create_dir_all(staging).await?;
    let zip = staging.join(ASSET);
    tokio::fs::write(&zip, archive).await?;
    if let Some(expected) = digest.and_then(|d| d.strip_prefix("sha256:")) {
        let sum = run("/usr/bin/shasum", &[os("-a"), os("256"), zip.as_os_str()]).await?;
        if sum.split_whitespace().next() != Some(expected) {
            bail!("the download does not match the release's checksum");
        }
    }
    run(
        "/usr/bin/ditto",
        &[os("-x"), os("-k"), zip.as_os_str(), staging.as_os_str()],
    )
    .await?;
    let new = staging.join(BUNDLE_NAME);
    let plist = new.join("Contents/Info.plist");
    let id = plist_value(&plist, "CFBundleIdentifier").await?;
    let shown = plist_value(&plist, "CFBundleShortVersionString").await?;
    if id.trim() != BUNDLE_ID || shown.trim() != version {
        bail!("the download is not ytfast {version}");
    }
    run(
        "/usr/bin/codesign",
        &[os("--verify"), os("--strict"), new.as_os_str()],
    )
    .await?;
    // The running copy carries on from memory; the staging folder, with the
    // old bundle in it, goes once the new one is in place.
    let previous = staging.join("previous.app");
    tokio::fs::rename(bundle, &previous)
        .await
        .context("could not move the app aside")?;
    if let Err(error) = tokio::fs::rename(&new, bundle).await {
        let _ = tokio::fs::rename(&previous, bundle).await;
        return Err(error).context("could not put the new app in place");
    }
    Ok(())
}

/// Opens `bundle` once this process has quit, so the new copy starts rather
/// than macOS bringing this one forward.
pub fn relaunch(bundle: &Path) -> Result<()> {
    std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open \"$2\"",
            "sh",
        ])
        .arg(std::process::id().to_string())
        .arg(bundle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("could not reopen the app")?;
    Ok(())
}

/// The `.app` this executable runs from, if it runs from one.
fn running_bundle() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let bundle = executable.parent()?.parent()?.parent()?;
    (bundle.extension()? == "app").then(|| bundle.to_path_buf())
}

fn os(text: &str) -> &OsStr {
    OsStr::new(text)
}

async fn plist_value(plist: &Path, key: &str) -> Result<String> {
    let print = format!("Print :{key}");
    run(
        "/usr/libexec/PlistBuddy",
        &[os("-c"), os(&print), plist.as_os_str()],
    )
    .await
}

async fn run(program: &str, args: &[&OsStr]) -> Result<String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("could not run {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether `candidate` is a later `major.minor.patch` than `current`. A
/// pre-release suffix is ignored; the latest release is never a pre-release.
fn newer(candidate: &str, current: &str) -> bool {
    let parts = |version: &str| -> Vec<u64> {
        version
            .split('-')
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    parts(candidate) > parts(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_later_version_is_newer() {
        assert!(newer("0.1.5", "0.1.4"));
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.1.10", "0.1.9"));
        assert!(!newer("0.1.4", "0.1.4"));
        assert!(!newer("0.1.3", "0.1.4"));
    }
}
