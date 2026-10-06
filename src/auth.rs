//! Signing in with the YouTube session of a browser on this computer.
//!
//! Without a browser engine there is no Google sign-in page to show. yt-dlp
//! (or, for Helium on macOS, `helium.rs`) reads the youtube.com cookies out of
//! a browser the person is signed in to; the session then lives in the
//! platform's credential store, never in a settings file, and every request
//! carries it with the SAPISIDHASH signature the web client sends.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use sha1::{Digest, Sha1};

use crate::stream::Ytdlp;

pub const BROWSERS: [&str; 9] = [
    "chrome", "helium", "firefox", "safari", "brave", "edge", "chromium", "opera", "vivaldi",
];

const SERVICE: &str = "ytfast";
const ACCOUNT: &str = "youtube-session";
const ORIGIN: &str = "https://music.youtube.com";

/// The cookies of a signed-in YouTube session.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    cookies: Vec<(String, String)>,
}

impl Session {
    /// The youtube.com cookies of a Netscape cookie file, or `None` when the
    /// browser has no signed-in session.
    pub fn from_netscape(text: &str) -> Option<Session> {
        let cookies = text.lines().filter_map(|line| {
            let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
            if line.starts_with('#') {
                return None;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            let [domain, _, _, _, _, name, value] = fields[..] else {
                return None;
            };
            Some((domain.to_string(), name.to_string(), value.to_string()))
        });
        Self::from_cookies(cookies)
    }

    /// The youtube.com cookies among `(domain, name, value)`, or `None` when
    /// they hold no signed-in session.
    pub fn from_cookies(
        all: impl IntoIterator<Item = (String, String, String)>,
    ) -> Option<Session> {
        let mut cookies: Vec<(String, String)> = Vec::new();
        for (domain, name, value) in all {
            if !(domain == "youtube.com" || domain.ends_with(".youtube.com")) {
                continue;
            }
            // A cookie set on both .youtube.com and music.youtube.com is sent
            // once; the more specific one is listed last and wins.
            cookies.retain(|(n, _)| *n != name);
            cookies.push((name, value));
        }
        let session = Session { cookies };
        session.sapisid().is_some().then_some(session)
    }

    /// Reads a stored `Cookie` header back.
    pub fn from_header(header: &str) -> Session {
        let cookies = header
            .split(';')
            .filter_map(|pair| {
                let (name, value) = pair.trim().split_once('=')?;
                Some((name.to_string(), value.to_string()))
            })
            .collect();
        Session { cookies }
    }

    /// The cookies as a Netscape cookie file, for yt-dlp. They expire a day
    /// on: the file lives only for one resolve.
    pub fn to_netscape(&self) -> String {
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + 24 * 60 * 60;
        let mut file = String::from("# Netscape HTTP Cookie File\n");
        for (name, value) in &self.cookies {
            file.push_str(&format!(
                ".youtube.com\tTRUE\t/\tTRUE\t{expires}\t{name}\t{value}\n"
            ));
        }
        file
    }

    pub fn header(&self) -> String {
        let pairs: Vec<String> = self
            .cookies
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect();
        pairs.join("; ")
    }

    fn sapisid(&self) -> Option<&str> {
        let get = |name: &str| {
            self.cookies
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        get("__Secure-3PAPISID").or_else(|| get("SAPISID"))
    }

    /// The `Authorization` header InnerTube wants beside the cookies.
    pub fn authorization(&self) -> Option<String> {
        let sapisid = self.sapisid()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        Some(sapisidhash(now, sapisid))
    }

    /// Takes a rotated value from a `Set-Cookie` header. Google rotates the
    /// session's security cookies and expects the fresh values back; replaying
    /// the old ones reads as a stolen session. Returns whether anything changed.
    pub fn merge(&mut self, set_cookie: &str) -> bool {
        let Some((name, rest)) = set_cookie.split_once('=') else {
            return false;
        };
        let value = rest.split(';').next().unwrap_or_default();
        match self.cookies.iter_mut().find(|(n, _)| n == name.trim()) {
            Some((_, old)) if old != value && !value.is_empty() => {
                *old = value.to_string();
                true
            }
            _ => false,
        }
    }
}

fn sapisidhash(now: u64, sapisid: &str) -> String {
    let digest = Sha1::digest(format!("{now} {sapisid} {ORIGIN}"));
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("SAPISIDHASH {now}_{hex}")
}

/// Reads the session out of `browser`. Chrome on macOS asks for the
/// Keychain password of "Chrome Safe Storage" the first time.
pub async fn import(ytdlp: &Ytdlp, browser: &str, scratch: &Path) -> Result<Session> {
    tokio::fs::create_dir_all(scratch).await?;
    if browser == "helium" {
        #[cfg(target_os = "macos")]
        return crate::helium::import(scratch).await;
        #[cfg(not(target_os = "macos"))]
        bail!("Signing in through Helium works on macOS only for now");
    }
    let file = scratch.join("cookies.txt");
    // yt-dlp writes the cookie file on exit even though, without a URL, it
    // then reports a usage error; so the file, not the status, is the answer.
    let output = ytdlp
        .command()
        .arg("--cookies-from-browser")
        .arg(browser)
        .arg("--cookies")
        .arg(&file)
        .output()
        .await
        .context("could not run yt-dlp")?;
    let text = tokio::fs::read_to_string(&file).await.unwrap_or_default();
    let _ = tokio::fs::remove_file(&file).await;
    if text.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .find(|l| l.contains("ERROR"))
            .unwrap_or("no cookies were read");
        bail!("{browser}: {reason}");
    }
    Session::from_netscape(&text).with_context(|| format!("{browser} is not signed in to YouTube"))
}

/// Picks the platform's credential store. Called once at startup.
pub fn init_store() {
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new();
    #[cfg(windows)]
    let store = windows_native_keyring_store::Store::new();
    #[cfg(target_os = "linux")]
    let store = zbus_secret_service_keyring_store::Store::new();
    match store {
        Ok(store) => keyring_core::set_default_store(store),
        Err(_) => log::warn!("no credential store; sign-in will not be remembered"),
    }
}

fn entry() -> Result<keyring_core::Entry> {
    keyring_core::Entry::new(SERVICE, ACCOUNT).context("credential store unavailable")
}

pub fn load() -> Option<Session> {
    let header = entry().ok()?.get_password().ok()?;
    Some(Session::from_header(&header))
}

pub fn save(session: &Session) -> Result<()> {
    entry()?
        .set_password(&session.header())
        .context("could not store the session")
}

pub fn clear() {
    if let Ok(entry) = entry() {
        let _ = entry.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_youtube_cookies_and_signs() {
        let file = "# Netscape HTTP Cookie File\n\
            .youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tabc\n\
            #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\tSID\tsid1\n\
            .google.com\tTRUE\t/\tTRUE\t0\tNID\tnope\n";
        let mut session = Session::from_netscape(file).expect("signed in");
        assert_eq!(session.header(), "SAPISID=abc; SID=sid1");
        assert_eq!(Session::from_header(&session.header()), session);
        assert_eq!(
            Session::from_netscape(&session.to_netscape()),
            Some(session.clone())
        );
        assert_eq!(
            sapisidhash(1, "abc"),
            format!("SAPISIDHASH 1_{}", {
                let d = Sha1::digest("1 abc https://music.youtube.com");
                d.iter().map(|b| format!("{b:02x}")).collect::<String>()
            })
        );
        assert!(session.merge("SID=sid2; Path=/; Secure"));
        assert!(!session.merge("SID=sid2; Path=/"));
        assert!(!session.merge("OTHER=1"));
        assert_eq!(session.header(), "SAPISID=abc; SID=sid2");
    }

    #[test]
    fn anonymous_browser_has_no_session() {
        assert!(Session::from_netscape(".youtube.com\tTRUE\t/\tTRUE\t0\tPREF\tx\n").is_none());
    }
}
