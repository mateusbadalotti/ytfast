//! Helium's YouTube session, read the way Chromium keeps cookies on macOS:
//! an SQLite file whose values are AES-128-CBC encrypted under a key derived
//! from a Keychain password.
//!
//! yt-dlp finds each Chromium browser's password by a Keychain name it knows
//! ("Chrome Safe Storage", …). Helium files its own as "Helium Storage Key",
//! so it is read here instead. The SQLite file goes through the `sqlite3`
//! and `security` tools macOS ships, so no database library is built in.

use std::path::{Path, PathBuf};

use aes::Aes128;
use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha1::Sha1;

use crate::auth::Session;

const KEYCHAIN_SERVICE: &str = "Helium Storage Key";
const KEYCHAIN_ACCOUNT: &str = "Helium";
/// Chromium's fixed derivation parameters on macOS.
const SALT: &[u8] = b"saltysalt";
const ITERATIONS: u32 = 1003;
/// From this database version on, a value starts with the SHA-256 of its
/// cookie's domain.
const DOMAIN_HASH_VERSION: u32 = 24;
const DOMAIN_HASH_LEN: usize = 32;

fn root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("no home folder")?;
    Ok(PathBuf::from(home).join("Library/Application Support/net.imput.helium"))
}

/// The profile Helium used last, from its `Local State`.
fn profile(root: &Path) -> PathBuf {
    let last = std::fs::read_to_string(root.join("Local State"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|state| state["profile"]["last_used"].as_str().map(str::to_string))
        .filter(|name| !name.contains(['/', '\\']) && name != "..");
    root.join(last.unwrap_or_else(|| "Default".into()))
}

pub async fn import(scratch: &Path) -> Result<Session> {
    let root = root()?;
    let profile = profile(&root);
    let database = [profile.join("Network/Cookies"), profile.join("Cookies")]
        .into_iter()
        .find(|p| p.is_file())
        .context("Helium has no cookies on this computer; is it installed and signed in?")?;
    // Helium holds the file locked while it runs: read a copy.
    let copy = scratch.join("helium-cookies.sqlite");
    tokio::fs::copy(&database, &copy).await?;
    let result = async {
        let password = keychain_password().await?;
        read(&copy, &password).await
    }
    .await;
    let _ = tokio::fs::remove_file(&copy).await;
    Session::from_cookies(result?).context("Helium is not signed in to YouTube")
}

/// Asks the Keychain; macOS shows its own prompt the first time.
async fn keychain_password() -> Result<String> {
    let output = tokio::process::Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-w",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
        ])
        .output()
        .await?;
    if !output.status.success() {
        bail!("the Keychain did not give out Helium's storage key");
    }
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches('\n')
        .to_string())
}

async fn sqlite(database: &Path, query: &str) -> Result<Vec<serde_json::Value>> {
    let output = tokio::process::Command::new("/usr/bin/sqlite3")
        .arg("-readonly")
        .arg("-json")
        .arg(database)
        .arg(query)
        .output()
        .await?;
    if !output.status.success() {
        bail!(
            "could not read Helium's cookies: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // No rows prints nothing at all rather than `[]`.
    if output.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[derive(Deserialize)]
struct Row {
    host_key: String,
    name: String,
    value: String,
    encrypted: String,
}

/// `(domain, name, value)` of every youtube.com cookie in `database`.
async fn read(database: &Path, password: &str) -> Result<Vec<(String, String, String)>> {
    let version = sqlite(database, "SELECT value FROM meta WHERE key = 'version'")
        .await?
        .first()
        .and_then(|row| {
            row["value"]
                .as_str()
                .and_then(|v| v.parse().ok())
                .or(row["value"].as_u64().map(|v| v as u32))
        })
        .unwrap_or(0);
    let rows = sqlite(
        database,
        "SELECT host_key, name, value, hex(encrypted_value) AS encrypted FROM cookies \
         WHERE host_key = 'youtube.com' OR host_key LIKE '%.youtube.com'",
    )
    .await?;
    let key = derive_key(password);
    let mut cookies = Vec::new();
    for row in rows {
        let row: Row = serde_json::from_value(row)?;
        let value = if row.value.is_empty() {
            match decrypt(
                &key,
                &from_hex(&row.encrypted),
                version >= DOMAIN_HASH_VERSION,
            ) {
                Some(value) => value,
                None => continue,
            }
        } else {
            row.value
        };
        cookies.push((row.host_key, row.name, value));
    }
    Ok(cookies)
}

fn derive_key(password: &str) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<Sha1>(password.as_bytes(), SALT, ITERATIONS, &mut key);
    key
}

fn from_hex(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

/// A `v10` value: AES-128-CBC with an IV of sixteen spaces, PKCS#7 padded.
fn decrypt(key: &[u8; 16], data: &[u8], domain_hash: bool) -> Option<String> {
    let body = data.strip_prefix(b"v10")?;
    if body.is_empty() || body.len() % 16 != 0 {
        return None;
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut previous = [b' '; 16];
    let mut plain = Vec::with_capacity(body.len());
    for chunk in body.chunks(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        plain.extend(block.iter().zip(previous).map(|(b, p)| b ^ p));
        previous.copy_from_slice(chunk);
    }
    let pad = usize::from(*plain.last()?);
    if pad == 0 || pad > 16 || pad > plain.len() {
        return None;
    }
    plain.truncate(plain.len() - pad);
    let start = if domain_hash {
        DOMAIN_HASH_LEN.min(plain.len())
    } else {
        0
    };
    String::from_utf8(plain.split_off(start)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncrypt;

    /// What Chromium writes: `v10` + AES-128-CBC(PKCS#7(plain)).
    fn encrypt(key: &[u8; 16], plain: &[u8]) -> Vec<u8> {
        let cipher = Aes128::new(GenericArray::from_slice(key));
        let pad = 16 - plain.len() % 16;
        let mut data = plain.to_vec();
        data.extend(std::iter::repeat_n(pad as u8, pad));
        let mut previous = [b' '; 16];
        let mut out = b"v10".to_vec();
        for chunk in data.chunks(16) {
            let mut block = GenericArray::clone_from_slice(chunk);
            for (b, p) in block.iter_mut().zip(previous) {
                *b ^= p;
            }
            cipher.encrypt_block(&mut block);
            previous.copy_from_slice(&block);
            out.extend_from_slice(&block);
        }
        out
    }

    #[test]
    fn decrypts_what_chromium_encrypts() {
        let key = derive_key("not the real password");
        assert_eq!(
            decrypt(&key, &encrypt(&key, b"sapisid-value"), false).as_deref(),
            Some("sapisid-value")
        );
        let mut hashed = vec![7u8; DOMAIN_HASH_LEN];
        hashed.extend_from_slice(b"value");
        assert_eq!(
            decrypt(&key, &encrypt(&key, &hashed), true).as_deref(),
            Some("value")
        );
        assert_eq!(
            decrypt(&derive_key("wrong"), &encrypt(&key, b"x"), false),
            None
        );
        assert_eq!(decrypt(&key, b"v11abc", false), None);
    }

    #[tokio::test]
    async fn reads_a_cookie_database() {
        let dir = std::env::temp_dir().join(format!("ytfast-helium-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let db = dir.join("Cookies");
        let key = derive_key("pw");
        let mut hashed = vec![0u8; DOMAIN_HASH_LEN];
        hashed.extend_from_slice(b"secret");
        let hex: String = encrypt(&key, &hashed)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        let setup = format!(
            "CREATE TABLE meta (key TEXT, value TEXT); INSERT INTO meta VALUES ('version', '24');\
             CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT, encrypted_value BLOB);\
             INSERT INTO cookies VALUES ('.youtube.com', 'SAPISID', '', X'{hex}');\
             INSERT INTO cookies VALUES ('.youtube.com', 'PREF', 'plain', X'');\
             INSERT INTO cookies VALUES ('.google.com', 'NID', 'other', X'');"
        );
        let status = std::process::Command::new("/usr/bin/sqlite3")
            .arg(&db)
            .arg(setup)
            .status()
            .expect("sqlite3");
        assert!(status.success());
        let cookies = read(&db, "pw").await.expect("read");
        assert_eq!(
            cookies,
            [
                (
                    ".youtube.com".to_string(),
                    "SAPISID".to_string(),
                    "secret".to_string()
                ),
                (
                    ".youtube.com".to_string(),
                    "PREF".to_string(),
                    "plain".to_string()
                ),
            ]
        );
        let session = Session::from_cookies(cookies).expect("signed in");
        assert_eq!(session.header(), "SAPISID=secret; PREF=plain");
        let _ = std::fs::remove_dir_all(dir);
    }
}
