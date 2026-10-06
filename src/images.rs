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
            let result = async {
                http.get(&uri)
                    .send()
                    .await?
                    .error_for_status()?
                    .bytes()
                    .await
            }
            .await
            .map(|b| Arc::<[u8]>::from(b.as_ref()))
            .map_err(|e| e.to_string());
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

/// The art to draw `px` pixels wide: the smallest listed that is big enough.
pub fn pick(thumbnails: &[Thumbnail], px: u32) -> Option<&Thumbnail> {
    thumbnails
        .iter()
        .filter(|t| t.width >= px)
        .min_by_key(|t| t.width)
        .or_else(|| thumbnails.iter().max_by_key(|t| t.width))
}

/// Google's image CDN renders any size asked for, so its URLs get `px`.
pub fn sized_url(thumbnail: &Thumbnail, px: u32) -> String {
    let url = &thumbnail.url;
    if (url.contains("googleusercontent.com") || url.contains("ggpht.com"))
        && let Some(at) = url.rfind('=')
    {
        return format!("{}=w{px}-h{px}-l90-rj", &url[..at]);
    }
    url.clone()
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
}
