//! Lyrics. YouTube Music's own come first: they are keyed on the videoId, so
//! they cannot be another song's words, and the Android client gets them
//! line-synced where the web client gets plain text. LRCLIB, matched by
//! title, artist and length, covers the tracks YouTube has nothing for.
//! Synced lyrics beat plain ones from either source.

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::innertube::{API, USER_AGENT, find_all, text};
use crate::model::{Item, LyricLine, Lyrics, LyricsText};

/// The Android app's version, which YouTube wants in the body and a header.
const CLIENT_VERSION: &str = "7.21.50";
/// Seconds an LRCLIB record's length may differ from the track's.
const LENGTH_SLACK: f64 = 3.0;

pub async fn fetch(http: &reqwest::Client, track: &Item) -> Result<Option<Lyrics>> {
    let (youtube, lrclib) = tokio::join!(youtube(http, &track.id), lrclib(http, track));
    let synced = |l: &Option<Lyrics>| {
        l.as_ref()
            .is_some_and(|l| matches!(l.text, LyricsText::Timed(_)))
    };
    match (youtube, lrclib) {
        (Ok(y), _) if synced(&y) => Ok(y),
        (_, Ok(l)) if synced(&l) => Ok(l),
        (Ok(Some(y)), _) => Ok(Some(y)),
        (_, Ok(Some(l))) => Ok(Some(l)),
        (Err(error), _) | (_, Err(error)) => Err(error),
        _ => Ok(None),
    }
}

/// Anonymous on purpose: the Android client context beside the person's web
/// cookies is the kind of mismatch anti-abuse systems notice.
async fn android(http: &reqwest::Client, endpoint: &str, mut body: Value) -> Result<Value> {
    body["context"] = json!({ "client": {
        "clientName": "ANDROID_MUSIC",
        "clientVersion": CLIENT_VERSION,
        "androidSdkVersion": 34,
        "hl": "en",
        "gl": "US",
    } });
    let response = http
        .post(format!("{API}/{endpoint}?prettyPrint=false"))
        .header("X-YouTube-Client-Name", "21")
        .header("X-YouTube-Client-Version", CLIENT_VERSION)
        .header("Origin", "https://music.youtube.com")
        .header("User-Agent", USER_AGENT)
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("YouTube Music lyrics: HTTP {}", response.status());
    }
    Ok(response.json().await?)
}

async fn youtube(http: &reqwest::Client, video_id: &str) -> Result<Option<Lyrics>> {
    let next = android(http, "next", json!({ "videoId": video_id })).await?;
    let mut tabs = Vec::new();
    find_all(&next, "tabRenderer", &mut tabs);
    let tab = tabs.into_iter().find(|tab| {
        tab["endpoint"]["browseEndpoint"]["browseEndpointContextSupportedConfigs"]["browseEndpointContextMusicConfig"]
            ["pageType"]
            == "MUSIC_PAGE_TYPE_TRACK_LYRICS"
    });
    // No tab, or one marked unselectable: YouTube has nothing for this track.
    let Some(tab) = tab.filter(|tab| tab["unselectable"] != true) else {
        return Ok(None);
    };
    let Some(browse_id) = tab["endpoint"]["browseEndpoint"]["browseId"].as_str() else {
        return Ok(None);
    };
    let page = android(http, "browse", json!({ "browseId": browse_id })).await?;

    let mut blocks = Vec::new();
    find_all(&page, "timedLyricsData", &mut blocks);
    if let Some(rows) = blocks
        .iter()
        .filter_map(|b| b.as_array())
        .find(|b| !b.is_empty())
    {
        let mut lines: Vec<LyricLine> = rows
            .iter()
            .filter_map(|row| {
                // The trailing "Source: …" row has no cue and would land at 0.
                let start = &row["cueRange"]["startTimeMilliseconds"];
                let ms: f64 = start
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .or(start.as_f64())?;
                let line = row["lyricLine"].as_str().unwrap_or_default().trim();
                Some(LyricLine {
                    start: (ms / 1000.0) as f32,
                    text: if line == "♪" {
                        String::new()
                    } else {
                        line.to_string()
                    },
                })
            })
            .collect();
        if !lines.is_empty() {
            lines.sort_by(|a, b| a.start.total_cmp(&b.start));
            return Ok(Some(Lyrics {
                text: LyricsText::Timed(lines),
                source: "YouTube Music",
            }));
        }
    }

    let mut shelves = Vec::new();
    find_all(&page, "musicDescriptionShelfRenderer", &mut shelves);
    let plain = shelves
        .iter()
        .map(|s| text(&s["description"]).trim().to_string())
        .find(|t| !t.is_empty());
    Ok(plain.map(|t| Lyrics {
        text: LyricsText::Plain(t),
        source: "YouTube Music",
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    duration: Option<f64>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
    #[serde(default)]
    instrumental: bool,
}

async fn lrclib(http: &reqwest::Client, track: &Item) -> Result<Option<Lyrics>> {
    let title = clean_title(&track.title);
    let artist = track
        .artists
        .first()
        .map(|a| a.name.clone())
        .unwrap_or_else(|| track.subtitle.clone())
        .trim_end_matches(" - Topic")
        .to_string();
    let duration = track.duration.map(f64::from);
    let client = concat!(
        "ytfast v",
        env!("CARGO_PKG_VERSION"),
        " (https://github.com/mateusbadalotti/ytfast)"
    );

    let mut get_query = vec![
        ("track_name", title.clone()),
        ("artist_name", artist.clone()),
    ];
    if let Some(album) = &track.album {
        get_query.push(("album_name", album.name.clone()));
    }
    if let Some(duration) = track.duration {
        get_query.push(("duration", duration.to_string()));
    }
    let get = async {
        let response = http
            .get("https://lrclib.net/api/get")
            .header("Lrclib-Client", client)
            .query(&get_query)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        let response = response
            .error_for_status()
            .map_err(reqwest::Error::without_url)?;
        Ok::<_, anyhow::Error>(vec![response.json::<Record>().await?])
    };
    let search = async {
        let response = http
            .get("https://lrclib.net/api/search")
            .header("Lrclib-Client", client)
            .query(&[("track_name", &title), ("artist_name", &artist)])
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        let response = response
            .error_for_status()
            .map_err(reqwest::Error::without_url)?;
        Ok::<_, anyhow::Error>(response.json::<Vec<Record>>().await?)
    };
    let (get, search) = tokio::join!(get, search);
    if let (Err(error), Err(_)) = (&get, &search) {
        bail!("LRCLIB did not answer: {error:#}");
    }
    let records: Vec<Record> = get
        .unwrap_or_default()
        .into_iter()
        .chain(search.unwrap_or_default())
        .collect();
    let fits = |r: &&Record| match (duration, r.duration) {
        (Some(want), Some(got)) => (want - got).abs() <= LENGTH_SLACK,
        _ => true,
    };
    let fitting: Vec<&Record> = records.iter().filter(fits).collect();
    if fitting.iter().any(|r| r.instrumental) {
        return Ok(Some(Lyrics {
            text: LyricsText::Plain("Instrumental".into()),
            source: "LRCLIB",
        }));
    }
    let synced = fitting.iter().find_map(|r| {
        r.synced_lyrics
            .as_deref()
            .map(parse_lrc)
            .filter(|l| !l.is_empty())
    });
    if let Some(lines) = synced {
        return Ok(Some(Lyrics {
            text: LyricsText::Timed(lines),
            source: "LRCLIB",
        }));
    }
    let plain = fitting
        .iter()
        .find_map(|r| r.plain_lyrics.clone().filter(|p| !p.trim().is_empty()));
    Ok(plain.map(|p| Lyrics {
        text: LyricsText::Plain(p),
        source: "LRCLIB",
    }))
}

/// "Song (Official Video) [4K]" → "Song": the parts LRCLIB never has.
fn clean_title(title: &str) -> String {
    const NOISE: [&str; 8] = [
        "official",
        "video",
        "audio",
        "lyric",
        "visualizer",
        "hd",
        "4k",
        " mv",
    ];
    let mut out = String::new();
    let mut rest = title;
    while let Some(open) = rest.find(['(', '[']) {
        let close = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(len) = rest[open..].find(close) else {
            break;
        };
        let inside = format!(" {}", rest[open + 1..open + len].to_lowercase());
        out.push_str(&rest[..open]);
        if !NOISE.iter().any(|n| inside.contains(n)) {
            out.push_str(&rest[open..=open + len]);
        }
        rest = &rest[open + len + 1..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// LRC text into timed lines. A positive `[offset:+ms]` shifts lines earlier.
pub fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
    let offset = lrc
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("[offset:")?
                .strip_suffix(']')?
                .trim()
                .parse::<f32>()
                .ok()
        })
        .unwrap_or(0.0)
        / 1000.0;
    let mut lines = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(tag) = rest.strip_prefix('[') {
            let Some(end) = tag.find(']') else { break };
            let Some(seconds) = timestamp(&tag[..end]) else {
                break;
            };
            stamps.push(seconds);
            rest = &tag[end + 1..];
        }
        for start in stamps {
            lines.push(LyricLine {
                start: (start - offset).max(0.0),
                text: rest.trim().to_string(),
            });
        }
    }
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    lines
}

/// "mm:ss", "mm:ss.xx" or "mm:ss:xx" in seconds.
fn timestamp(tag: &str) -> Option<f32> {
    let (minutes, rest) = tag.split_once(':')?;
    let (seconds, fraction) = rest.split_once(['.', ':']).unwrap_or((rest, ""));
    let minutes: u32 = minutes.parse().ok()?;
    let seconds: u32 = seconds.parse().ok()?;
    let fraction = if fraction.is_empty() {
        0.0
    } else {
        let digits: String = fraction.chars().chain("000".chars()).take(3).collect();
        digits.parse::<f32>().ok()? / 1000.0
    };
    Some(minutes as f32 * 60.0 + seconds as f32 + fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let lines =
            parse_lrc("[ar:Someone]\n[00:15.67][00:20]Chorus\n[00:12.3]First\n[01:02:500]Late");
        let got: Vec<(f32, &str)> = lines.iter().map(|l| (l.start, l.text.as_str())).collect();
        assert_eq!(
            got,
            [
                (12.3, "First"),
                (15.67, "Chorus"),
                (20.0, "Chorus"),
                (62.5, "Late")
            ]
        );
        let shifted = parse_lrc("[offset:+500]\n[00:01.00]A");
        assert_eq!(shifted[0].start, 0.5);
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("Song (Official Music Video) [4K]"), "Song");
        assert_eq!(
            clean_title("Song (feat. Someone) [Lyrics]"),
            "Song (feat. Someone)"
        );
        assert_eq!(clean_title("Plain"), "Plain");
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use crate::model::{Kind, Link};

    #[tokio::test]
    #[ignore = "talks to YouTube and LRCLIB"]
    async fn finds_synced_lyrics() {
        let track = Item {
            kind: Kind::Song,
            id: "4NRXx6U8ABQ".into(),
            title: "Blinding Lights".into(),
            subtitle: "The Weeknd".into(),
            thumbnails: Vec::new(),
            artists: vec![Link {
                id: None,
                name: "The Weeknd".into(),
            }],
            album: None,
            duration: Some(200),
            explicit: false,
            play_video_id: None,
        };
        let http = reqwest::Client::new();
        let lyrics = fetch(&http, &track)
            .await
            .expect("lyrics")
            .expect("some lyrics");
        let LyricsText::Timed(lines) = &lyrics.text else {
            panic!("plain from {}", lyrics.source)
        };
        println!("{} lines from {}", lines.len(), lyrics.source);
        assert!(lines.len() > 20);
        let lrclib = super::lrclib(&http, &track)
            .await
            .expect("lrclib")
            .expect("lrclib lyrics");
        assert!(matches!(lrclib.text, LyricsText::Timed(_)));
    }
}
