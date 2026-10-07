//! Cover art: an egui bytes loader on the app's own HTTP client, and the
//! pick of the right size from what YouTube lists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use egui::load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError};

use crate::model::Thumbnail;

type Entry = Poll<Result<Arc<[u8]>, String>>;

pub struct Loader {
    http: reqwest::Client,
    runtime: tokio::runtime::Handle,
    cache: Arc<Mutex<HashMap<String, Entry>>>,
}

impl Loader {
    pub fn new(http: reqwest::Client, runtime: tokio::runtime::Handle) -> Self {
        Self {
            http,
            runtime,
            cache: Arc::default(),
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl BytesLoader for Loader {
    fn id(&self) -> &str {
        egui::generate_loader_id!(Loader)
    }

    fn load(&self, ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        if !uri.starts_with("https://") {
            return Err(LoadError::NotSupported);
        }
        let mut cache = self.cache();
        match cache.get(uri) {
            Some(Poll::Ready(Ok(bytes))) => {
                return Ok(BytesPoll::Ready {
                    size: None,
                    bytes: Bytes::Shared(bytes.clone()),
                    mime: None,
                });
            }
            Some(Poll::Ready(Err(error))) => return Err(LoadError::Loading(error.clone())),
            Some(Poll::Pending) => return Ok(BytesPoll::Pending { size: None }),
            None => {}
        }
        cache.insert(uri.to_string(), Poll::Pending);
        drop(cache);
        let (http, store, ctx, uri) = (
            self.http.clone(),
            self.cache.clone(),
            ctx.clone(),
            uri.to_string(),
        );
        self.runtime.spawn(async move {
            let mut result = fetch(&http, &uri).await;
            for smaller in smaller_stills(&uri) {
                let missing = result
                    .as_ref()
                    .is_err_and(|e| e.status() == Some(reqwest::StatusCode::NOT_FOUND));
                if !missing {
                    break;
                }
                result = fetch(&http, &smaller).await;
            }
            let result = result.map_err(|e| e.to_string());
            let mut cache = store.lock().unwrap_or_else(|e| e.into_inner());
            // Forgotten while loading: drop the answer.
            if let Some(entry) = cache.get_mut(&uri) {
                *entry = Poll::Ready(result);
                ctx.request_repaint();
            }
        });
        Ok(BytesPoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.cache().remove(uri);
    }

    fn forget_all(&self) {
        self.cache().clear();
    }

    fn byte_size(&self) -> usize {
        self.cache()
            .values()
            .map(|entry| match entry {
                Poll::Ready(Ok(bytes)) => bytes.len(),
                _ => 0,
            })
            .sum()
    }

    fn has_pending(&self) -> bool {
        self.cache().values().any(Poll::is_pending)
    }
}

async fn fetch(http: &reqwest::Client, url: &str) -> reqwest::Result<Arc<[u8]>> {
    let bytes = http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    Ok(Arc::<[u8]>::from(bytes.as_ref()))
}

const STILLS: &str = "https://i.ytimg.com/vi/";
pub const LARGEST_STILL: &str = "maxresdefault";

/// A video's still by YouTube's name for its size: `default` is 120×90 and
/// `maxresdefault` 1280×720. The listed thumbnails are smaller copies of
/// these, signed, so they cannot be asked for in another size.
pub fn still(thumbnails: &[Thumbnail], name: &str) -> Option<String> {
    let id = thumbnails
        .iter()
        .find_map(|t| t.url.strip_prefix(STILLS)?.split('/').next())?;
    Some(format!("{STILLS}{id}/{name}.jpg"))
}

/// What to fetch, in turn, when a video's largest still is missing: only HD
/// uploads have one, some others lack `sddefault` too, and every public
/// video has `hqdefault`.
fn smaller_stills(uri: &str) -> Vec<String> {
    match uri.strip_suffix(&format!("/{LARGEST_STILL}.jpg")) {
        Some(base) if uri.starts_with(STILLS) => ["sddefault", "hqdefault"]
            .map(|name| format!("{base}/{name}.jpg"))
            .into(),
        _ => Vec::new(),
    }
}

/// The art to draw `px` pixels wide: the smallest listed that is big enough.
pub fn pick(thumbnails: &[Thumbnail], px: u32) -> Option<&Thumbnail> {
    thumbnails
        .iter()
        .filter(|t| t.width >= px)
        .min_by_key(|t| t.width)
        .or_else(|| thumbnails.iter().max_by_key(|t| t.width))
}

/// Google's image CDN renders any size asked for, so its URLs get `px` on
/// the longer side, in the shape the image was listed in. Only the size
/// options change: the others stay, among them `p` and `c`, which crop the
/// photo to that shape, so it is drawn unstretched.
pub fn sized_url(thumbnail: &Thumbnail, px: u32) -> String {
    let url = &thumbnail.url;
    if !(url.contains("googleusercontent.com") || url.contains("ggpht.com")) {
        return url.clone();
    }
    let Some(at) = url.rfind('=') else {
        return url.clone();
    };
    let is_size = |option: &&str| {
        option.len() > 1
            && option.starts_with(['w', 'h', 's'])
            && option[1..].bytes().all(|b| b.is_ascii_digit())
    };
    let kept: Vec<&str> = url[at + 1..]
        .split('-')
        .filter(|option| !option.is_empty() && !is_size(option))
        .collect();
    let (width, height) = (
        u64::from(thumbnail.width.max(1)),
        u64::from(thumbnail.height.max(1)),
    );
    let long = u64::from(px);
    let (w, h) = if width >= height {
        (long, long * height / width)
    } else {
        (long * width / height, long)
    };
    let mut options = format!("w{w}-h{h}");
    for option in kept {
        options.push('-');
        options.push_str(option);
    }
    format!("{}={options}", &url[..at])
}

pub fn art_url(thumbnails: &[Thumbnail], px: u32) -> Option<String> {
    pick(thumbnails, px).map(|t| sized_url(t, px))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb(url: &str, width: u32) -> Thumbnail {
        Thumbnail {
            url: url.into(),
            width,
            height: width,
        }
    }

    #[test]
    fn picks_and_resizes() {
        let list = [
            thumb("https://i.ytimg.com/a.jpg", 120),
            thumb("https://i.ytimg.com/b.jpg", 480),
        ];
        assert_eq!(
            art_url(&list, 200).as_deref(),
            Some("https://i.ytimg.com/b.jpg")
        );
        assert_eq!(
            art_url(&list, 900).as_deref(),
            Some("https://i.ytimg.com/b.jpg")
        );
        let google = [thumb(
            "https://lh3.googleusercontent.com/abc=w60-h60-l90-rj",
            60,
        )];
        assert_eq!(
            art_url(&google, 300).as_deref(),
            Some("https://lh3.googleusercontent.com/abc=w300-h300-l90-rj")
        );
        assert_eq!(art_url(&[], 10), None);
    }

    #[test]
    fn stills_step_down_from_the_largest() {
        let video = [thumb(
            "https://i.ytimg.com/vi/abc/hqdefault.jpg?sqp=x&rs=y",
            400,
        )];
        let largest = still(&video, LARGEST_STILL);
        assert_eq!(
            largest.as_deref(),
            Some("https://i.ytimg.com/vi/abc/maxresdefault.jpg")
        );
        assert_eq!(
            smaller_stills(&largest.unwrap()),
            [
                "https://i.ytimg.com/vi/abc/sddefault.jpg",
                "https://i.ytimg.com/vi/abc/hqdefault.jpg"
            ]
        );
        assert!(smaller_stills("https://i.ytimg.com/vi/abc/hqdefault.jpg").is_empty());
        let song = [thumb(
            "https://lh3.googleusercontent.com/abc=w60-h60-l90-rj",
            60,
        )];
        assert_eq!(still(&song, LARGEST_STILL), None);
    }

    #[test]
    fn resizing_keeps_the_crop() {
        let artist = thumb(
            "https://lh3.googleusercontent.com/x=w544-h544-p-l90-rj",
            544,
        );
        assert_eq!(
            sized_url(&artist, 200),
            "https://lh3.googleusercontent.com/x=w200-h200-p-l90-rj"
        );
        let banner = Thumbnail {
            url: "https://lh3.googleusercontent.com/z=w1440-h600-p-l90-rj".into(),
            width: 1440,
            height: 600,
        };
        assert_eq!(
            sized_url(&banner, 2000),
            "https://lh3.googleusercontent.com/z=w2000-h833-p-l90-rj"
        );
        let avatar = thumb("https://yt3.ggpht.com/y=s88-c-k-c0x00ffffff-no-rj", 88);
        assert_eq!(
            sized_url(&avatar, 128),
            "https://yt3.ggpht.com/y=w128-h128-c-k-c0x00ffffff-no-rj"
        );
    }
}
