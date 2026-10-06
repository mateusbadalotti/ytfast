//! YouTube Music's InnerTube API, spoken as the WEB_REMIX web client, and
//! the parsers for its responses. Ported from YTubic's `src/lib/innertube`.
//!
//! The responses nest the same renderers under different wrappers from page
//! to page and change shape without notice, so the parsers read a few known
//! paths and fall back to walking the tree for the renderer they want.

use std::sync::{Mutex, RwLock};

use anyhow::{Context, Result, bail};
use reqwest::header::{AUTHORIZATION, COOKIE, HeaderMap, SET_COOKIE};
use serde_json::{Value, json};

use crate::auth::{self, Session};
use crate::model::{
    Account, ArtistPage, Browse, Collection, Feed, Item, Kind, Link, Rating, SearchFilter,
    SearchResults, Shelf, Thumbnail, Watch,
};

const CLIENT_VERSION: &str = "1.20260510.02.00";
const API: &str = "https://music.youtube.com/youtubei/v1";
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// A runaway guard for paged library shelves (~25 rows a page).
const MAX_PAGES: usize = 40;

/// youtube.com's own client, for the one question YouTube Music cannot
/// answer: its search rows carry no upload date and it cannot sort by one.
const WEB_API: &str = "https://www.youtube.com/youtubei/v1";
const WEB_CLIENT_VERSION: &str = "2.20260901.00.00";
/// youtube.com's "Sort by: Upload date", as its own menu sends it.
const SORT_BY_DATE: &str = "CAI%3D";

/// The shortest video taken for a set: singles and teasers run three or
/// four minutes, interviews twenty, sets from about an hour.
const MIN_SET_SECONDS: u32 = 30 * 60;
/// The name alone finds an artist's releases; with these words attached it
/// finds their sets, which the bare name buries.
const SET_QUERIES: [&str; 5] = ["", " set", " live", " mix", " b2b"];
const SETS_KEPT: usize = 40;

/// YouTube Music's own generated playlists, not ones the person made or saved.
const HIDDEN_PLAYLISTS: [&str; 1] = ["episodes for later"];

pub struct Innertube {
    http: reqwest::Client,
    session: RwLock<Option<Session>>,
    /// `responseContext.visitorData`, echoed back so YouTube treats the app
    /// as a returning visitor.
    visitor: Mutex<Option<String>>,
    /// The player script's signature timestamp, which `/player` wants
    /// before it answers with anything playable. Read once per run.
    signature: Mutex<Option<u64>>,
}

impl Innertube {
    pub fn new(http: reqwest::Client, session: Option<Session>) -> Self {
        Self {
            http,
            session: RwLock::new(session),
            visitor: Mutex::new(None),
            signature: Mutex::new(None),
        }
    }

    /// Adds the session's cookies and signature to a request. Returns
    /// whether there was a session to add.
    fn authorized(&self, mut request: reqwest::RequestBuilder) -> (reqwest::RequestBuilder, bool) {
        let session = self.session();
        if let Some(session) = &session {
            request = request.header(COOKIE, session.header());
            if let Some(authorization) = session.authorization() {
                request = request.header(AUTHORIZATION, authorization);
            }
        }
        (request, session.is_some())
    }

    pub fn set_session(&self, session: Option<Session>) {
        *self.session.write().unwrap_or_else(|e| e.into_inner()) = session;
    }

    pub fn session(&self) -> Option<Session> {
        self.session
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn signed_in(&self) -> bool {
        self.session
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    async fn post(&self, endpoint: &str, mut body: Value) -> Result<Value> {
        let visitor = self
            .visitor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut client = json!({
            "clientName": "WEB_REMIX",
            "clientVersion": CLIENT_VERSION,
            "hl": "en",
            "gl": "US",
            "platform": "DESKTOP",
            "userAgent": format!("{USER_AGENT},gzip(gfe)"),
            "originalUrl": "https://music.youtube.com/",
        });
        if let Some(visitor) = &visitor {
            client["visitorData"] = json!(visitor);
        }
        body["context"] = json!({
            "client": client,
            "user": { "lockedSafetyMode": false },
            "request": { "useSsl": true },
        });
        let separator = if endpoint.contains('?') { '&' } else { '?' };
        let mut request = self
            .http
            .post(format!("{API}/{endpoint}{separator}prettyPrint=false"))
            .header("User-Agent", USER_AGENT)
            .header("X-YouTube-Client-Name", "67")
            .header("X-YouTube-Client-Version", CLIENT_VERSION)
            .header("Origin", "https://music.youtube.com")
            .header("X-Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("X-Goog-AuthUser", "0")
            .json(&body);
        if let Some(visitor) = &visitor {
            request = request.header("X-Goog-Visitor-Id", visitor);
        }
        let (request, signed_in) = self.authorized(request);
        let response = request
            .send()
            .await
            .with_context(|| format!("{endpoint}: no answer"))?;
        if signed_in {
            self.take_rotated(response.headers());
        }
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!(
                "{endpoint}: HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            );
        }
        let json: Value = response.json().await?;
        if let Some(visitor) = json["responseContext"]["visitorData"].as_str() {
            *self.visitor.lock().unwrap_or_else(|e| e.into_inner()) = Some(visitor.to_string());
        }
        Ok(json)
    }

    /// Keeps the session's rotated cookies and stores them.
    fn take_rotated(&self, headers: &HeaderMap) {
        let mut guard = self.session.write().unwrap_or_else(|e| e.into_inner());
        let Some(session) = guard.as_mut() else {
            return;
        };
        let mut changed = false;
        for value in headers.get_all(SET_COOKIE) {
            if let Ok(value) = value.to_str() {
                changed |= session.merge(value);
            }
        }
        if changed {
            let session = session.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(error) = auth::save(&session) {
                    log::warn!("{error:#}");
                }
            });
        }
    }

    async fn browse_id(&self, browse_id: &str, params: Option<&str>) -> Result<Value> {
        let mut body = json!({ "browseId": browse_id });
        if let Some(params) = params {
            body["params"] = json!(params);
        }
        self.post("browse", body).await
    }

    async fn continuation(&self, token: &str) -> Result<Value> {
        self.post("browse", json!({ "continuation": token })).await
    }

    pub async fn home(&self, cursor: Option<&str>) -> Result<Feed> {
        let (sections, continuation) = match cursor {
            None => {
                let json = self.browse_id("FEmusic_home", None).await?;
                let list = first_tab(&json)["sectionListRenderer"].clone();
                let sections = array(&list["contents"]).to_vec();
                let next =
                    token_in_contents(&sections).or_else(|| token_in_list(&list["continuations"]));
                (sections, next)
            }
            Some(cursor) => {
                let json = self.continuation(cursor).await?;
                let mut sections: Vec<Value> = array(&json["onResponseReceivedActions"])
                    .iter()
                    .flat_map(|a| {
                        array(&a["appendContinuationItemsAction"]["continuationItems"]).to_vec()
                    })
                    .collect();
                let legacy = &json["continuationContents"]["sectionListContinuation"];
                if sections.is_empty() {
                    sections = array(&legacy["contents"]).to_vec();
                }
                let next = token_in_contents(&sections)
                    .or_else(|| token_in_list(&legacy["continuations"]));
                (sections, next)
            }
        };
        Ok(Feed {
            title: String::new(),
            shelves: shelves_of(&sections),
            continuation,
        })
    }

    /// A shelf's "More" target: an artist's discography, "Playlists by …".
    pub async fn browse(&self, target: &Browse, cursor: Option<&str>) -> Result<Feed> {
        let json = match cursor {
            Some(cursor) => self.continuation(cursor).await?,
            None => self.browse_id(&target.id, target.params.as_deref()).await?,
        };
        let wrappers = if cursor.is_some() {
            let contents = &json["continuationContents"];
            if contents["gridContinuation"]["items"].is_array() {
                collect_shelves(&[
                    json!({ "gridRenderer": { "items": contents["gridContinuation"]["items"] } }),
                ])
            } else {
                collect_shelves(array(&contents["sectionListContinuation"]["contents"]))
            }
        } else {
            collect_shelves(array(&first_tab(&json)["sectionListRenderer"]["contents"]))
        };
        let shelves = wrappers
            .iter()
            .enumerate()
            .map(|(i, w)| map_shelf(w, i))
            .filter(|s| !s.items.is_empty())
            .collect();
        Ok(Feed {
            title: text(&json["header"]["musicHeaderRenderer"]["title"]),
            shelves,
            continuation: find_continuation(&json),
        })
    }

    pub async fn album(&self, id: &str) -> Result<Collection> {
        let json = self.browse_id(id, None).await?;
        let header = first_of(&[
            &json["header"]["musicDetailHeaderRenderer"],
            &json["header"]["musicResponsiveHeaderRenderer"],
            &json["contents"]["twoColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]
                ["sectionListRenderer"]["contents"][0]["musicResponsiveHeaderRenderer"],
        ]);
        let mut artists = Vec::new();
        let mut album = None;
        links(&header["subtitle"]["runs"], &mut artists, &mut album);
        links(
            &header["straplineTextOne"]["runs"],
            &mut artists,
            &mut album,
        );
        let title = text(&header["title"]);
        let thumbnails = header_thumbnails(header);
        let link = Link {
            id: Some(id.to_string()),
            name: title.clone(),
        };
        let mut seen = std::collections::HashSet::new();
        let tracks = rows(&json)
            .filter(|t| t.kind == Kind::Song && seen.insert(t.id.clone()))
            .map(|mut track| {
                // Album rows leave out what the header already says.
                if track.artists.is_empty() {
                    track.artists = artists.clone();
                    track.subtitle = track.artist_names();
                }
                track.album = Some(link.clone());
                track.thumbnails = thumbnails.clone();
                track
            })
            .collect();
        Ok(Collection {
            id: id.to_string(),
            title,
            artists,
            subtitle: text(&header["subtitle"]),
            info: text(&header["secondSubtitle"]),
            description: description(header),
            thumbnails,
            tracks,
            continuation: None,
        })
    }

    pub async fn playlist(&self, id: &str) -> Result<Collection> {
        let browse_id = if id.starts_with("VL") {
            id.to_string()
        } else {
            format!("VL{id}")
        };
        let json = self.browse_id(&browse_id, None).await?;
        let empty = Value::Null;
        let header = find_any(
            &json,
            &["musicDetailHeaderRenderer", "musicResponsiveHeaderRenderer"],
        )
        .unwrap_or(&empty);
        let mut thumbnails = header_thumbnails(header);
        if thumbnails.is_empty() {
            thumbnails = deep_thumbnails(&header["thumbnail"]);
        }
        // Editable playlists append a Suggestions shelf: only the playlist's
        // own shelf holds its tracks.
        let scope = find(&json, "musicPlaylistShelfRenderer").unwrap_or(&json);
        let mut seen = std::collections::HashSet::new();
        let mut tracks: Vec<Item> = rows(scope)
            .filter(|t| t.kind == Kind::Song && seen.insert(t.id.clone()))
            .collect();
        let mut continuation = find_continuation(scope);
        if tracks.is_empty() {
            // Radio-style playlists (RDCLAK…, RDAMPL…) have their tracks under /next.
            tracks = self
                .watch_playlist(&browse_id[2..], None)
                .await
                .unwrap_or_default();
            continuation = None;
        }
        let owner = text(&header["straplineTextOne"]);
        Ok(Collection {
            id: browse_id.clone(),
            title: text(&header["title"]),
            artists: Vec::new(),
            subtitle: if owner.is_empty() {
                text(&header["subtitle"])
            } else {
                owner
            },
            info: text(&header["secondSubtitle"]),
            description: description(header),
            thumbnails,
            tracks,
            continuation,
        })
    }

    pub async fn playlist_more(&self, token: &str) -> Result<(Vec<Item>, Option<String>)> {
        let json = self.continuation(token).await?;
        let tracks = rows(&json).filter(|t| t.kind == Kind::Song).collect();
        let next = find_continuation(&json).filter(|next| next != token);
        Ok((tracks, next))
    }

    pub async fn artist(&self, id: &str) -> Result<ArtistPage> {
        let json = self.browse_id(id, None).await?;
        let header = first_of(&[
            &json["header"]["musicImmersiveHeaderRenderer"],
            &json["header"]["musicDetailHeaderRenderer"],
        ]);
        let thumbnails = thumbnails(first_of(&[
            &header["thumbnail"]["musicThumbnailRenderer"]["thumbnail"],
            &header["thumbnail"]["croppedSquareThumbnailRenderer"]["thumbnail"],
            &header["foregroundThumbnail"]["musicThumbnailRenderer"]["thumbnail"],
        ]));
        Ok(ArtistPage {
            id: id.to_string(),
            name: text(&header["title"]),
            description: text(&header["description"]),
            audience: text(&header["monthlyListenerCount"]),
            thumbnails,
            shuffle: watch_target(&header["playButton"]),
            radio: watch_target(&header["startRadioButton"]),
            shelves: shelves_of(array(&first_tab(&json)["sectionListRenderer"]["contents"])),
        })
    }

    pub async fn search(&self, query: &str, filter: SearchFilter) -> Result<SearchResults> {
        let mut body = json!({ "query": query });
        if let Some(params) = search_params(filter) {
            body["params"] = json!(params);
        }
        let json = self.post("search", body).await?;
        let sections = first_of(&[
            &json["contents"]["tabbedSearchResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"]
                ["contents"],
            &json["contents"]["sectionListRenderer"]["contents"],
        ]);
        let sections = array(sections);
        if filter != SearchFilter::All {
            let mut shelves = shelves_of(sections);
            if filter == SearchFilter::Videos {
                for item in shelves.iter_mut().flat_map(|s| s.items.iter_mut()) {
                    if item.kind == Kind::Song {
                        item.kind = Kind::Video;
                    }
                }
            }
            return Ok(SearchResults { top: None, shelves });
        }
        Ok(group_search(sections))
    }

    /// A station seeded on one song, as "Start radio" plays it.
    pub async fn radio(&self, video_id: &str) -> Result<Vec<Item>> {
        let json = self
            .post("next", json!({ "videoId": video_id, "playlistId": format!("RDAMVM{video_id}"), "isAudioOnly": true }))
            .await?;
        Ok(panel_tracks(&json))
    }

    /// The tracks of a watch playlist: an artist's shuffle (`RDAO…`), mix
    /// (`RDEM…`), an album (`OLAK…`) or a playlist.
    pub async fn watch_playlist(
        &self,
        playlist_id: &str,
        video_id: Option<&str>,
    ) -> Result<Vec<Item>> {
        let mut body = json!({ "playlistId": playlist_id, "isAudioOnly": true });
        if let Some(video_id) = video_id {
            body["videoId"] = json!(video_id);
        }
        Ok(panel_tracks(&self.post("next", body).await?))
    }

    /// How the signed-in person rated a track, from its watch page.
    pub async fn rating(&self, video_id: &str) -> Result<Rating> {
        let json = self.post("next", json!({ "videoId": video_id })).await?;
        let status = find(&json, "likeButtonRenderer").and_then(|b| b["likeStatus"].as_str());
        Ok(match status {
            Some("LIKE") => Rating::Like,
            Some("DISLIKE") => Rating::Dislike,
            _ => Rating::None,
        })
    }

    pub async fn rate(&self, video_id: &str, rating: Rating) -> Result<()> {
        let endpoint = match rating {
            Rating::Like => "like/like",
            Rating::Dislike => "like/dislike",
            Rating::None => "like/removelike",
        };
        self.post(endpoint, json!({ "target": { "videoId": video_id } }))
            .await?;
        Ok(())
    }

    /// Every item of a library page (`FEmusic_liked_playlists`,
    /// `FEmusic_liked_albums`, `FEmusic_library_corpus_artists`), all pages.
    pub async fn library(&self, browse_id: &str) -> Result<Vec<Item>> {
        let json = self.browse_id(browse_id, None).await?;
        let list = &first_tab(&json)["sectionListRenderer"];
        let wrappers = collect_shelves(array(&list["contents"]));
        let mut items = Vec::new();
        for (i, wrapper) in wrappers.iter().enumerate() {
            let shelf = first_of(&[
                &wrapper["musicShelfRenderer"],
                &wrapper["musicCarouselShelfRenderer"],
                wrapper,
            ]);
            // A one-shelf page may hang the shelf's token off the section list.
            let token = paging_token(shelf)
                .or_else(|| (wrappers.len() == 1).then(|| paging_token(list)).flatten());
            items.extend(map_shelf(wrapper, i).items);
            let rest = self.all_pages(token).await;
            if !rest.is_empty() {
                items.extend(
                    map_shelf(&json!({ "musicShelfRenderer": { "contents": rest } }), i).items,
                );
            }
        }
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| {
            !HIDDEN_PLAYLISTS.contains(&item.title.trim().to_lowercase().as_str())
                && seen.insert(item.id.clone())
        });
        Ok(items)
    }

    /// Rows past a shelf's first page. A failed page ends the walk: the pages
    /// that loaded beat an error.
    async fn all_pages(&self, mut token: Option<String>) -> Vec<Value> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..MAX_PAGES {
            let Some(current) = token.take().filter(|t| seen.insert(t.clone())) else {
                break;
            };
            let Ok(json) = self.continuation(&current).await else {
                break;
            };
            let (items, next) = continuation_page(&json);
            if items.is_empty() {
                break;
            }
            out.extend(items);
            token = next;
        }
        out
    }

    /// Whether the signed-in person owns a playlist, and so can add to it or
    /// delete it: owned ones come with editing controls nothing else has.
    pub async fn owns_playlist(&self, browse_id: &str) -> Result<bool> {
        let json = self.browse_id(browse_id, None).await?;
        Ok(find_any(
            &json,
            &[
                "musicEditablePlaylistDetailHeaderRenderer",
                "editPlaylistEndpoint",
            ],
        )
        .is_some())
    }

    pub async fn add_to_playlist(&self, playlist_id: &str, video_id: &str) -> Result<()> {
        let json = self
            .post(
                "browse/edit_playlist",
                json!({
                    "playlistId": playlist_id.trim_start_matches("VL"),
                    "actions": [{
                        "action": "ACTION_ADD_VIDEO",
                        "addedVideoId": video_id,
                        "dedupeOption": "DEDUPE_OPTION_SKIP",
                    }],
                }),
            )
            .await?;
        // HTTP 200 also carries a refusal (not the owner, stale session).
        match json["status"].as_str() {
            Some("STATUS_SUCCEEDED") | None => Ok(()),
            Some(status) => bail!("YouTube refused: {status}"),
        }
    }

    /// youtube.com's InnerTube, anonymously: everything asked of it is
    /// public, and the session has no business on a second client.
    async fn post_web(&self, endpoint: &str, mut body: Value) -> Result<Value> {
        body["context"] = json!({
            "client": {
                "clientName": "WEB",
                "clientVersion": WEB_CLIENT_VERSION,
                "hl": "en",
                "gl": "US",
            },
        });
        let response = self
            .http
            .post(format!("{WEB_API}/{endpoint}?prettyPrint=false"))
            .header("User-Agent", USER_AGENT)
            .header("X-YouTube-Client-Name", "1")
            .header("X-YouTube-Client-Version", WEB_CLIENT_VERSION)
            .header("Origin", "https://www.youtube.com")
            .header("Referer", "https://www.youtube.com/")
            .header("Accept-Language", "en-US,en;q=0.9")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("youtube {endpoint}: no answer"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!(
                "youtube {endpoint}: HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            );
        }
        Ok(response.json().await?)
    }

    /// An artist's sets, newest first: videos of half an hour or more whose
    /// title names them, from a date-sorted search of the name and a few
    /// set words. One query failing costs only its results.
    pub async fn artist_sets(&self, name: &str) -> Result<Vec<Item>> {
        let searches = SET_QUERIES.map(|words| {
            self.post_web(
                "search",
                json!({ "query": format!("{name}{words}"), "params": SORT_BY_DATE }),
            )
        });
        let results = futures_util::future::join_all(searches).await;
        let mut sets: Vec<(u64, Item)> = Vec::new();
        let mut failure = None;
        for result in results {
            let json = match result {
                Ok(json) => json,
                Err(error) => {
                    failure = Some(error);
                    continue;
                }
            };
            let mut videos = Vec::new();
            find_all(&json, "videoRenderer", &mut videos);
            for video in videos {
                if let Some(set) = set_item(video, name)
                    && !sets.iter().any(|(_, kept)| kept.id == set.1.id)
                {
                    sets.push(set);
                }
            }
        }
        if sets.is_empty()
            && let Some(error) = failure
        {
            return Err(error);
        }
        // The search sorts its main list only; shelves it adds come after,
        // out of order.
        sets.sort_by_key(|(age, _)| *age);
        Ok(sets
            .into_iter()
            .map(|(_, item)| item)
            .take(SETS_KEPT)
            .collect())
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        self.post(
            "playlist/delete",
            json!({ "playlistId": playlist_id.trim_start_matches("VL") }),
        )
        .await?;
        Ok(())
    }

    /// Records a play in the signed-in account's history, the way
    /// music.youtube.com's player does once a track starts: the playback
    /// beacon from the track's `/player` answer. yt-dlp, which fetches the
    /// audio, never sends it, so without this nothing reaches the history.
    pub async fn report_play(&self, video_id: &str) -> Result<()> {
        let signature = self.signature_timestamp().await?;
        let player = self
            .post(
                "player",
                json!({
                    "videoId": video_id,
                    "playbackContext": { "contentPlaybackContext": { "signatureTimestamp": signature } },
                }),
            )
            .await?;
        let url = player["playbackTracking"]["videostatsPlaybackUrl"]["baseUrl"]
            .as_str()
            .with_context(|| {
                let status = player["playabilityStatus"]["status"]
                    .as_str()
                    .unwrap_or("no status");
                format!("no history beacon for {video_id} ({status})")
            })?;
        let request = self
            .http
            .get(url)
            .query(&[("ver", "2"), ("c", "WEB_REMIX"), ("cpn", &playback_nonce())])
            .header("User-Agent", USER_AGENT)
            .header("Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .header("X-Goog-AuthUser", "0");
        let (request, signed_in) = self.authorized(request);
        if !signed_in {
            bail!("not signed in");
        }
        let response = request.send().await.map_err(reqwest::Error::without_url)?;
        self.take_rotated(response.headers());
        response
            .error_for_status()
            .map_err(reqwest::Error::without_url)?;
        Ok(())
    }

    async fn signature_timestamp(&self) -> Result<u64> {
        if let Some(signature) = *self.signature.lock().unwrap_or_else(|e| e.into_inner()) {
            return Ok(signature);
        }
        let get = |url: String| async move {
            self.http
                .get(url)
                .header("User-Agent", USER_AGENT)
                .send()
                .await?
                .error_for_status()?
                .text()
                .await
        };
        let page = get("https://music.youtube.com/".into()).await?;
        let script_path =
            between(&page, "\"jsUrl\":\"", "\"").context("no player script on the page")?;
        let script = get(format!("https://music.youtube.com{script_path}")).await?;
        let signature = script
            .split_once("signatureTimestamp")
            .and_then(|(_, rest)| {
                let digits: String = rest
                    .trim_start_matches([':', '=', ' '])
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits.parse().ok()
            })
            .context("no signature timestamp in the player script")?;
        *self.signature.lock().unwrap_or_else(|e| e.into_inner()) = Some(signature);
        Ok(signature)
    }

    /// The signed-in account, or `None` when YouTube answers as anonymous.
    pub async fn account(&self) -> Result<Option<Account>> {
        let json = self.post("account/account_menu", json!({})).await?;
        let header = &json["actions"][0]["openPopupAction"]["popup"]["multiPageMenuRenderer"]["header"]
            ["activeAccountHeaderRenderer"];
        let name = text(&header["accountName"]);
        let email = text(&header["email"]);
        if name.is_empty() && email.is_empty() {
            return Ok(None);
        }
        let photo = array(&header["accountPhoto"]["thumbnails"])
            .last()
            .and_then(|t| t["url"].as_str())
            .map(str::to_string);
        Ok(Some(Account { name, email, photo }))
    }
}

/// The text between `start` and the next `end` after it.
fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let len = text[from..].find(end)?;
    Some(&text[from..from + len])
}

/// A client playback nonce: sixteen URL-safe base64 characters, new for
/// every play, as the web player makes them.
fn playback_nonce() -> String {
    const ALPHABET: &[u8; 64] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
    (0..16)
        .map(|_| char::from(ALPHABET[rand::random_range(0..ALPHABET.len())]))
        .collect()
}

fn search_params(filter: SearchFilter) -> Option<&'static str> {
    match filter {
        SearchFilter::All => None,
        SearchFilter::Songs => Some("EgWKAQIIAWoQEAkQBRAKEAMQBBAQEBUQEQ=="),
        SearchFilter::Videos => Some("EgWKAQIQAWoQEAkQBRAKEAMQBBAQEBUQEQ=="),
        SearchFilter::Albums => Some("EgWKAQIYAWoQEAkQBRAKEAMQBBAQEBUQEQ=="),
        SearchFilter::Artists => Some("EgWKAQIgAWoQEAkQBRAKEAMQBBAQEBUQEQ=="),
        SearchFilter::Playlists => Some("EgWKAQIoAWoQEAkQBRAKEAMQBBAQEBUQEQ=="),
    }
}

/// The "All" tab is one flat list of mixed rows; each row's kind is the first
/// word of its subtitle ("Song • …"), English because the context pins `hl`.
fn group_search(sections: &[Value]) -> SearchResults {
    const GROUPS: [(&str, &str, bool); 5] = [
        ("Song", "Songs", true),
        ("Artist", "Artists", false),
        ("Album", "Albums & singles", false),
        ("Video", "Videos", true),
        ("Playlist", "Community playlists", false),
    ];
    let card = sections
        .iter()
        .map(|s| &s["musicCardShelfRenderer"])
        .find(|c| c.is_object());
    let top = card.and_then(map_top_card);
    let mut seen: std::collections::HashSet<(Kind, String)> =
        top.iter().map(|t| (t.kind, t.id.clone())).collect();
    let mut buckets: Vec<Vec<Item>> = vec![Vec::new(); GROUPS.len()];
    for row in sections
        .iter()
        .flat_map(|s| array(&s["itemSectionRenderer"]["contents"]))
    {
        let row = &row["musicResponsiveListItemRenderer"];
        if !row.is_object() {
            continue;
        }
        let column = &row["flexColumns"][1]["musicResponsiveListItemFlexColumnRenderer"]["text"];
        let token = column["runs"][0]["text"]
            .as_str()
            .map(str::trim)
            .unwrap_or_default();
        let group = match token {
            "Song" => 0,
            "Artist" => 1,
            "Album" | "Single" | "EP" => 2,
            "Video" => 3,
            "Playlist" => 4,
            _ => continue,
        };
        let Some(mut item) = map_responsive(row) else {
            continue;
        };
        if group == 3 && item.kind == Kind::Song {
            item.kind = Kind::Video;
        }
        if seen.insert((item.kind, item.id.clone())) {
            buckets[group].push(item);
        }
    }
    let shelves = GROUPS
        .iter()
        .zip(buckets)
        .filter(|(_, items)| !items.is_empty())
        .map(|((_, title, list), items)| Shelf {
            title: title.to_string(),
            items,
            list: *list,
            more: None,
        })
        .collect();
    SearchResults { top, shelves }
}

fn map_top_card(card: &Value) -> Option<Item> {
    let title = text(&card["title"]);
    if title.is_empty() {
        return None;
    }
    let subtitle = text(&card["subtitle"]);
    let thumbnails = thumbnails(&card["thumbnail"]["musicThumbnailRenderer"]["thumbnail"]);
    let tap = first_of(&[
        &card["onTap"],
        &card["title"]["runs"][0]["navigationEndpoint"],
    ]);
    let mut artists = Vec::new();
    let mut album = None;
    links(&card["subtitle"]["runs"], &mut artists, &mut album);
    let mut item = Item {
        kind: Kind::Song,
        id: String::new(),
        title,
        subtitle: subtitle.clone(),
        thumbnails,
        artists,
        album,
        duration: None,
        explicit: explicit(card),
        play_video_id: None,
    };
    if let Some(video_id) = tap["watchEndpoint"]["videoId"].as_str() {
        let first = subtitle
            .split('•')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        item.kind = if first == "video" {
            Kind::Video
        } else {
            Kind::Song
        };
        item.id = video_id.to_string();
        item.duration = subtitle
            .rsplit('•')
            .next()
            .and_then(|d| parse_duration(d.trim()));
    } else {
        let (id, page_type) = browse_target(tap)?;
        item.kind = kind_of_page(page_type)?;
        item.id = id.to_string();
    }
    Some(item)
}

fn panel_tracks(json: &Value) -> Vec<Item> {
    let panel = first_of(&[
        &json["contents"]["singleColumnMusicWatchNextResultsRenderer"]["tabbedRenderer"]["watchNextTabbedResultsRenderer"]
            ["tabs"][0]["tabRenderer"]["content"]["musicQueueRenderer"]["content"]["playlistPanelRenderer"],
        &json["continuationContents"]["playlistPanelContinuation"],
    ]);
    array(&panel["contents"])
        .iter()
        .filter_map(|c| {
            // Songs that also have a music video come wrapped, the song first.
            let row = first_of(&[
                &c["playlistPanelVideoRenderer"],
                &c["playlistPanelVideoWrapperRenderer"]["primaryRenderer"]["playlistPanelVideoRenderer"],
            ]);
            map_panel_video(row)
        })
        .collect()
}

fn watch_target(button: &Value) -> Option<Watch> {
    let endpoint = &button["buttonRenderer"]["navigationEndpoint"];
    let video_id = endpoint["watchEndpoint"]["videoId"]
        .as_str()
        .map(str::to_string);
    let playlist_id = first_of(&[
        &endpoint["watchEndpoint"]["playlistId"],
        &endpoint["watchPlaylistEndpoint"]["playlistId"],
    ])
    .as_str()
    .map(str::to_string);
    (video_id.is_some() || playlist_id.is_some()).then_some(Watch {
        video_id,
        playlist_id,
    })
}

// ---------------------------------------------------------------------------
// Response helpers

static EMPTY: Vec<Value> = Vec::new();

fn array(value: &Value) -> &[Value] {
    value.as_array().unwrap_or(&EMPTY)
}

/// The first of `candidates` that is present, like a chain of `??` in the
/// web client.
fn first_of<'a>(candidates: &[&'a Value]) -> &'a Value {
    candidates
        .iter()
        .copied()
        .find(|v| !v.is_null())
        .unwrap_or(&Value::Null)
}

fn first_tab(json: &Value) -> &Value {
    &json["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]
}

/// The text of a `runs` or `simpleText` node.
pub(crate) fn text(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(s) = value["simpleText"].as_str() {
        return s.to_string();
    }
    array(&value["runs"])
        .iter()
        .filter_map(|r| r["text"].as_str())
        .collect()
}

fn description(header: &Value) -> String {
    let direct = text(&header["description"]);
    if direct.is_empty() {
        text(&header["description"]["musicDescriptionShelfRenderer"]["description"])
    } else {
        direct
    }
}

/// The first value stored under `key`, depth first.
fn find<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    find_any(value, &[key])
}

fn find_any<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(found) = map.get(*key).filter(|v| v.is_object()) {
                    return Some(found);
                }
            }
            map.values().find_map(|v| find_any(v, keys))
        }
        Value::Array(list) => list.iter().find_map(|v| find_any(v, keys)),
        _ => None,
    }
}

/// Every value stored under `key`, at any depth.
pub(crate) fn find_all<'a>(value: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k == key {
                    out.push(v);
                }
                find_all(v, key, out);
            }
        }
        Value::Array(list) => list.iter().for_each(|v| find_all(v, key, out)),
        _ => {}
    }
}

/// A youtube.com search row as a set, with its age in seconds for the
/// order, when it is long enough and its title names the artist.
fn set_item(video: &Value, artist: &str) -> Option<(u64, Item)> {
    let id = video["videoId"].as_str()?;
    let title = text(&video["title"]);
    let duration = parse_duration(&text(&video["lengthText"]))?;
    if duration < MIN_SET_SECONDS || !names(&title, artist) {
        return None;
    }
    let published = text(&video["publishedTimeText"]);
    let channel = text(&video["ownerText"]);
    let subtitle = [channel.as_str(), published.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" • ");
    let item = Item {
        kind: Kind::Video,
        id: id.to_string(),
        title,
        subtitle,
        thumbnails: thumbnails(&video["thumbnail"]),
        artists: Vec::new(),
        album: None,
        duration: Some(duration),
        explicit: false,
        play_video_id: None,
    };
    Some((age(&published), item))
}

/// Whether `name` is in `title` as a whole word, so a short name does not
/// match inside a longer one, nor a set by someone who only mentions it.
fn names(title: &str, name: &str) -> bool {
    let (title, name) = (title.to_lowercase(), name.to_lowercase());
    if name.is_empty() {
        return false;
    }
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    title.match_indices(&name).any(|(at, _)| {
        !word(title[..at].chars().next_back()) && !word(title[at + name.len()..].chars().next())
    })
}

/// "3 weeks ago" in rough seconds, for ordering only; undated rows last.
fn age(published: &str) -> u64 {
    const UNITS: [(&str, u64); 7] = [
        ("second", 1),
        ("minute", 60),
        ("hour", 3_600),
        ("day", 86_400),
        ("week", 604_800),
        ("month", 2_629_800),
        ("year", 31_557_600),
    ];
    let count = published
        .split_whitespace()
        .find_map(|word| word.parse::<u64>().ok());
    let unit = UNITS.iter().find(|(unit, _)| published.contains(unit));
    match (count, unit) {
        (Some(count), Some((_, seconds))) => count * seconds,
        _ => u64::MAX,
    }
}

/// Every track row anywhere in a response, mapped.
fn rows(value: &Value) -> impl Iterator<Item = Item> {
    let mut found = Vec::new();
    find_all(value, "musicResponsiveListItemRenderer", &mut found);
    found
        .into_iter()
        .filter_map(map_responsive)
        .collect::<Vec<_>>()
        .into_iter()
}

/// The first paging token anywhere under `value`.
fn find_continuation(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(t) = value["nextContinuationData"]["continuation"].as_str() {
                return Some(t.to_string());
            }
            if let Some(t) = value["continuationCommand"]["token"].as_str() {
                return Some(t.to_string());
            }
            map.values().find_map(find_continuation)
        }
        Value::Array(list) => list.iter().find_map(find_continuation),
        _ => None,
    }
}

fn token_in_contents(contents: &[Value]) -> Option<String> {
    contents
        .iter()
        .find_map(|c| {
            c["continuationItemRenderer"]["continuationEndpoint"]["continuationCommand"]["token"]
                .as_str()
        })
        .map(str::to_string)
}

fn token_in_list(list: &Value) -> Option<String> {
    array(list)
        .iter()
        .find_map(|c| c["nextContinuationData"]["continuation"].as_str())
        .map(str::to_string)
}

/// The paging token of one container, read only off that container: a walk
/// of the whole response can pick up a filter chip's token instead.
fn paging_token(container: &Value) -> Option<String> {
    for c in array(&container["continuations"]) {
        let token = first_of(&[
            &c["nextContinuationData"]["continuation"],
            &c["continuationEndpoint"]["continuationCommand"]["token"],
            &c["continuationCommand"]["token"],
        ]);
        if let Some(token) = token.as_str() {
            return Some(token.to_string());
        }
    }
    let items = first_of(&[&container["items"], &container["contents"]]);
    array(items)
        .last()
        .and_then(|last| {
            last["continuationItemRenderer"]["continuationEndpoint"]["continuationCommand"]["token"]
                .as_str()
        })
        .map(str::to_string)
}

/// A section-list continuation hands back whole sections: unwrap them into rows.
fn flatten_rows(rows: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for row in rows {
        let inner = first_of(&[
            &row["gridRenderer"],
            &row["musicShelfRenderer"],
            &row["musicPlaylistShelfRenderer"],
        ]);
        let nested = array(first_of(&[&inner["items"], &inner["contents"]]));
        if nested.is_empty() {
            out.push(row.clone());
        } else {
            out.extend(nested.iter().cloned());
        }
    }
    out
}

fn continuation_page(json: &Value) -> (Vec<Value>, Option<String>) {
    let contents = &json["continuationContents"];
    let container = first_of(&[
        &contents["gridContinuation"],
        &contents["musicShelfContinuation"],
        &contents["musicPlaylistShelfContinuation"],
        &contents["sectionListContinuation"],
        &contents["playlistVideoListContinuation"],
    ]);
    if !container.is_null() {
        let items = array(first_of(&[&container["items"], &container["contents"]]));
        return (flatten_rows(items), paging_token(container));
    }
    for action in array(&json["onResponseReceivedActions"]) {
        let appended = &action["appendContinuationItemsAction"]["continuationItems"];
        if !array(appended).is_empty() {
            return (
                flatten_rows(array(appended)),
                paging_token(&json!({ "items": appended })),
            );
        }
    }
    (Vec::new(), None)
}

// ---------------------------------------------------------------------------
// Renderer mappers

fn thumbnails(node: &Value) -> Vec<Thumbnail> {
    let list = first_of(&[&node["thumbnails"], &node["thumbnail"]["thumbnails"]]);
    array(list)
        .iter()
        .filter_map(|t| {
            let url = t["url"].as_str().filter(|u| !u.is_empty())?;
            Some(Thumbnail {
                url: url.to_string(),
                width: t["width"].as_u64().unwrap_or(0) as u32,
                height: t["height"].as_u64().unwrap_or(0) as u32,
            })
        })
        .collect()
}

/// The first non-empty `thumbnails` list anywhere under `node`, for the
/// collage renderers user playlists come wrapped in.
fn deep_thumbnails(node: &Value) -> Vec<Thumbnail> {
    let mut found = Vec::new();
    find_all(node, "thumbnails", &mut found);
    found
        .into_iter()
        .map(|list| thumbnails(&json!({ "thumbnails": list })))
        .find(|list| !list.is_empty())
        .unwrap_or_default()
}

fn header_thumbnails(header: &Value) -> Vec<Thumbnail> {
    thumbnails(first_of(&[
        &header["thumbnail"]["musicThumbnailRenderer"]["thumbnail"],
        &header["thumbnail"]["croppedSquareThumbnailRenderer"]["thumbnail"],
        &header["thumbnail"]["musicThumbnailRenderer"],
        &header["thumbnail"],
    ]))
}

fn browse_target(endpoint: &Value) -> Option<(&str, &str)> {
    let browse = &endpoint["browseEndpoint"];
    let id = browse["browseId"].as_str()?;
    let page_type = browse["browseEndpointContextSupportedConfigs"]["browseEndpointContextMusicConfig"]["pageType"]
        .as_str()
        .unwrap_or_default();
    Some((id, page_type))
}

fn kind_of_page(page_type: &str) -> Option<Kind> {
    if page_type.contains("ARTIST") {
        Some(Kind::Artist)
    } else if page_type.contains("ALBUM") {
        Some(Kind::Album)
    } else if page_type.contains("PLAYLIST") || page_type.contains("PODCAST_SHOW") {
        Some(Kind::Playlist)
    } else {
        None
    }
}

/// Sorts the linked runs of a subtitle into artists and an album. Returns
/// whether any run linked anywhere.
fn links(runs: &Value, artists: &mut Vec<Link>, album: &mut Option<Link>) -> bool {
    let mut linked = false;
    for run in array(runs) {
        let Some((id, page_type)) = browse_target(&run["navigationEndpoint"]) else {
            continue;
        };
        let name = run["text"].as_str().unwrap_or_default().to_string();
        match kind_of_page(page_type) {
            Some(Kind::Artist) => artists.push(Link {
                id: Some(id.to_string()),
                name,
            }),
            Some(Kind::Album) => {
                *album = Some(Link {
                    id: Some(id.to_string()),
                    name,
                })
            }
            _ => continue,
        }
        linked = true;
    }
    linked
}

fn explicit(raw: &Value) -> bool {
    array(&raw["badges"])
        .iter()
        .chain(array(&raw["subtitleBadges"]))
        .any(|badge| {
            let badge = &badge["musicInlineBadgeRenderer"];
            badge["icon"]["iconType"]
                .as_str()
                .is_some_and(|i| i.contains("EXPLICIT"))
                || first_of(&[
                    &badge["accessibilityData"]["accessibilityData"]["label"],
                    &badge["accessibilityText"],
                ])
                .as_str()
                .is_some_and(|label| label.to_lowercase().contains("explicit"))
        })
}

/// "3:42", "1:05:03", "1 hour, 23 minutes" or "45 min", in seconds.
pub fn parse_duration(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.contains(':') {
        let parts: Option<Vec<u32>> = text.split(':').map(|p| p.trim().parse().ok()).collect();
        return match parts?.as_slice() {
            [m, s] => Some(m * 60 + s),
            [h, m, s] => Some(h * 3600 + m * 60 + s),
            _ => None,
        };
    }
    let mut total = 0;
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    for pair in words.windows(2) {
        let (Ok(n), unit) = (pair[0].parse::<u32>(), pair[1].to_lowercase()) else {
            continue;
        };
        if unit.starts_with('h') {
            total += n * 3600;
        } else if unit.starts_with("min") || unit == "m" {
            total += n * 60;
        } else if unit.starts_with("sec") || unit.starts_with("seg") || unit == "s" {
            total += n;
        }
    }
    (total > 0).then_some(total)
}

fn map_panel_video(raw: &Value) -> Option<Item> {
    let id = raw["navigationEndpoint"]["watchEndpoint"]["videoId"].as_str()?;
    let byline = first_of(&[&raw["longBylineText"], &raw["shortBylineText"]]);
    let mut artists = Vec::new();
    let mut album = None;
    links(&byline["runs"], &mut artists, &mut album);
    let mut item = Item {
        kind: Kind::Song,
        id: id.to_string(),
        title: text(&raw["title"]),
        subtitle: String::new(),
        thumbnails: thumbnails(&raw["thumbnail"]),
        artists,
        album,
        duration: parse_duration(&text(&raw["lengthText"])),
        explicit: explicit(raw),
        play_video_id: None,
    };
    item.subtitle = if item.artists.is_empty() {
        text(byline)
            .split('•')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        item.artist_names()
    };
    Some(item)
}

/// A card in a carousel (`musicTwoRowItemRenderer`).
fn map_two_row(raw: &Value) -> Option<Item> {
    let endpoint = &raw["navigationEndpoint"];
    let subtitle = text(&raw["subtitle"]);
    let mut artists = Vec::new();
    let mut album = None;
    links(&raw["subtitle"]["runs"], &mut artists, &mut album);
    let mut thumbs = thumbnails(first_of(&[
        &raw["thumbnailRenderer"]["musicThumbnailRenderer"]["thumbnail"],
        &raw["thumbnail"]["musicThumbnailRenderer"]["thumbnail"],
        &raw["thumbnail"],
    ]));
    if thumbs.is_empty() {
        thumbs = deep_thumbnails(&raw["thumbnailRenderer"]);
    }
    if thumbs.is_empty() {
        thumbs = deep_thumbnails(&raw["thumbnail"]);
    }
    let (kind, id) = if let Some(video_id) = endpoint["watchEndpoint"]["videoId"].as_str() {
        // Music videos come with 16:9 art and songs with square art; the
        // endpoint itself does not say which.
        let widest = thumbs.iter().max_by_key(|t| t.width);
        let wide = widest.is_some_and(|t| t.height > 0 && t.width as f32 / t.height as f32 > 1.4);
        (if wide { Kind::Video } else { Kind::Song }, video_id)
    } else {
        let (id, page_type) = browse_target(endpoint)?;
        (kind_of_page(page_type)?, id)
    };
    let play_video_id = (kind == Kind::Playlist)
        .then(|| {
            raw["overlay"]["musicItemThumbnailOverlayRenderer"]["content"]["musicPlayButtonRenderer"]
                ["playNavigationEndpoint"]["watchEndpoint"]["videoId"]
                .as_str()
        })
        .flatten()
        .map(str::to_string);
    let duration = (matches!(kind, Kind::Song | Kind::Video) || play_video_id.is_some())
        .then(|| {
            parse_duration(&text(&raw["lengthText"]))
                .or_else(|| {
                    array(&raw["thumbnailOverlays"]).iter().find_map(|o| {
                        parse_duration(&text(&o["thumbnailOverlayTimeStatusRenderer"]["text"]))
                    })
                })
                .or_else(|| subtitle.rsplit('•').next().and_then(parse_duration))
        })
        .flatten();
    Some(Item {
        kind,
        id: id.to_string(),
        title: text(&raw["title"]),
        subtitle,
        thumbnails: thumbs,
        artists,
        album,
        duration,
        explicit: explicit(raw),
        play_video_id,
    })
}

/// A row in a list (`musicResponsiveListItemRenderer`).
fn map_responsive(raw: &Value) -> Option<Item> {
    let column =
        |i: usize| &raw["flexColumns"][i]["musicResponsiveListItemFlexColumnRenderer"]["text"];
    let title_run = &column(0)["runs"][0];
    let mut artists = Vec::new();
    let mut album = None;
    for i in 1..array(&raw["flexColumns"]).len() {
        links(&column(i)["runs"], &mut artists, &mut album);
    }
    let duration = parse_duration(&text(
        &raw["fixedColumns"][0]["musicResponsiveListItemFixedColumnRenderer"]["text"],
    ))
    .or_else(|| {
        array(&column(1)["runs"])
            .last()
            .and_then(|r| r["text"].as_str())
            .and_then(parse_duration)
    });
    let mut thumbs = thumbnails(&raw["thumbnail"]["musicThumbnailRenderer"]["thumbnail"]);
    let video_id = first_of(&[
        &title_run["navigationEndpoint"]["watchEndpoint"]["videoId"],
        &raw["overlay"]["musicItemThumbnailOverlayRenderer"]["content"]["musicPlayButtonRenderer"]
            ["playNavigationEndpoint"]["watchEndpoint"]["videoId"],
        &raw["playlistItemData"]["videoId"],
        &raw["navigationEndpoint"]["watchEndpoint"]["videoId"],
    ])
    .as_str();
    let mut item = Item {
        kind: Kind::Song,
        id: String::new(),
        title: text(column(0)),
        subtitle: text(column(1)),
        thumbnails: Vec::new(),
        artists,
        album,
        duration: None,
        explicit: explicit(raw),
        play_video_id: None,
    };
    if let Some(video_id) = video_id {
        // Every public video has generated stills, for rows that come without art.
        if thumbs.is_empty() {
            thumbs = vec![Thumbnail {
                url: format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg"),
                width: 480,
                height: 360,
            }];
        }
        item.id = video_id.to_string();
        item.duration = duration;
        if !item.artists.is_empty() {
            item.subtitle = item.artist_names();
        }
    } else {
        let target = first_of(&[&title_run["navigationEndpoint"], &raw["navigationEndpoint"]]);
        let (id, page_type) = browse_target(target)?;
        item.kind = kind_of_page(page_type)?;
        item.id = id.to_string();
    }
    item.thumbnails = thumbs;
    Some(item)
}

/// Every carousel or shelf under a section list, through the item-section
/// and section-list wrappers. A grid becomes a shelf of its own.
fn collect_shelves(sections: &[Value]) -> Vec<Value> {
    fn walk(node: &Value, out: &mut Vec<Value>) {
        if [
            "musicCarouselShelfRenderer",
            "musicShelfRenderer",
            "musicCardShelfRenderer",
        ]
        .iter()
        .any(|k| node[*k].is_object())
        {
            out.push(node.clone());
            return;
        }
        array(&node["itemSectionRenderer"]["contents"])
            .iter()
            .for_each(|c| walk(c, out));
        array(&node["sectionListRenderer"]["contents"])
            .iter()
            .for_each(|c| walk(c, out));
        let grid = &node["gridRenderer"];
        if grid["items"].is_array() {
            out.push(json!({
                "musicShelfRenderer": {
                    "title": grid["header"]["gridHeaderRenderer"]["title"],
                    "contents": grid["items"],
                    "continuations": grid["continuations"],
                }
            }));
        }
    }
    let mut out = Vec::new();
    sections.iter().for_each(|s| walk(s, &mut out));
    out
}

fn shelves_of(sections: &[Value]) -> Vec<Shelf> {
    collect_shelves(sections)
        .iter()
        .enumerate()
        .map(|(i, w)| map_shelf(w, i))
        .filter(|s| !s.items.is_empty())
        .collect()
}

fn map_shelf(wrapper: &Value, index: usize) -> Shelf {
    let card = &wrapper["musicCardShelfRenderer"];
    let music = first_of(&[
        &wrapper["musicCarouselShelfRenderer"],
        &wrapper["musicShelfRenderer"],
        card,
    ]);
    let title = if card.is_object() {
        text(&card["header"]["musicCardShelfHeaderBasicRenderer"]["title"])
    } else {
        text(first_of(&[
            &music["header"]["musicCarouselShelfBasicHeaderRenderer"]["title"],
            &music["title"],
        ]))
    };
    let mut items = Vec::new();
    let (mut rows, mut cards) = (false, false);
    for content in array(&music["contents"]) {
        if content["musicTwoRowItemRenderer"].is_object() {
            cards = true;
            items.extend(map_two_row(&content["musicTwoRowItemRenderer"]));
        } else if content["musicResponsiveListItemRenderer"].is_object() {
            rows = true;
            items.extend(map_responsive(&content["musicResponsiveListItemRenderer"]));
        }
    }
    if let Some(featured) = card.is_object().then(|| map_card_featured(card)).flatten() {
        items.insert(0, featured);
    }
    Shelf {
        title: if title.is_empty() {
            format!("Section {}", index + 1)
        } else {
            title
        },
        items,
        list: rows && !cards,
        more: if card.is_object() {
            None
        } else {
            read_more(music)
        },
    }
}

/// The featured track at the top of a "Quick picks"-style card shelf.
fn map_card_featured(card: &Value) -> Option<Item> {
    let video_id = card["onTap"]["watchEndpoint"]["videoId"].as_str()?;
    let title = text(&card["title"]);
    if title.is_empty() {
        return None;
    }
    Some(Item {
        kind: Kind::Song,
        id: video_id.to_string(),
        title,
        subtitle: text(&card["subtitle"]),
        thumbnails: thumbnails(&card["thumbnail"]["musicThumbnailRenderer"]["thumbnail"]),
        artists: Vec::new(),
        album: None,
        duration: None,
        explicit: explicit(card),
        play_video_id: None,
    })
}

fn read_more(music: &Value) -> Option<Browse> {
    let header = &music["header"]["musicCarouselShelfBasicHeaderRenderer"];
    let nav = first_of(&[
        &header["moreContentButton"]["buttonRenderer"]["navigationEndpoint"],
        &music["bottomEndpoint"],
        &header["title"]["runs"][0]["navigationEndpoint"],
        &music["title"]["runs"][0]["navigationEndpoint"],
    ]);
    let browse = &nav["browseEndpoint"];
    Some(Browse {
        id: browse["browseId"].as_str()?.to_string(),
        params: browse["params"].as_str().map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("3:42"), Some(222));
        assert_eq!(parse_duration("1:05:03"), Some(3903));
        assert_eq!(parse_duration("1 hour, 23 minutes"), Some(4980));
        assert_eq!(parse_duration("45 min"), Some(2700));
        assert_eq!(parse_duration("1.2M views"), None);
        assert_eq!(parse_duration(""), None);
    }

    fn run(text: &str, id: &str, page_type: &str) -> Value {
        json!({ "text": text, "navigationEndpoint": { "browseEndpoint": {
            "browseId": id,
            "browseEndpointContextSupportedConfigs": { "browseEndpointContextMusicConfig": { "pageType": page_type } }
        } } })
    }

    #[test]
    fn maps_a_song_row() {
        let row = json!({
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "One More Time", "navigationEndpoint": { "watchEndpoint": { "videoId": "FGBhQbmPwH8" } } }
                ] } } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Song" }, { "text": " • " },
                    run("Daft Punk", "UC1", "MUSIC_PAGE_TYPE_ARTIST"), { "text": " • " },
                    run("Discovery", "MPREb_1", "MUSIC_PAGE_TYPE_ALBUM"), { "text": " • " },
                    { "text": "5:21" }
                ] } } }
            ],
            "badges": [{ "musicInlineBadgeRenderer": { "icon": { "iconType": "MUSIC_EXPLICIT_BADGE" } } }]
        });
        let item = map_responsive(&row).expect("a song");
        assert_eq!(item.kind, Kind::Song);
        assert_eq!(item.id, "FGBhQbmPwH8");
        assert_eq!(item.subtitle, "Daft Punk");
        assert_eq!(item.album.map(|a| a.name), Some("Discovery".into()));
        assert_eq!(item.duration, Some(321));
        assert!(item.explicit);
        assert!(item.thumbnails[0].url.contains("FGBhQbmPwH8"));
    }

    #[test]
    fn search_groups_by_subtitle_token() {
        let row = |token: &str, id: &str| {
            json!({ "itemSectionRenderer": { "contents": [{ "musicResponsiveListItemRenderer": { "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": id, "navigationEndpoint": { "watchEndpoint": { "videoId": id } } }
                ] } } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": token }] } } }
            ] } }] } })
        };
        let results = group_search(&[
            row("Video", "aaaaaaaaaaa"),
            row("Song", "bbbbbbbbbbb"),
            row("Episode", "ccccccccccc"),
        ]);
        let titles: Vec<&str> = results.shelves.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Songs", "Videos"]);
        assert_eq!(results.shelves[1].items[0].kind, Kind::Video);
    }

    fn client() -> Innertube {
        Innertube::new(
            reqwest::Client::builder()
                .gzip(true)
                .build()
                .expect("client"),
            None,
        )
    }

    #[test]
    fn a_set_names_the_artist_as_a_word() {
        assert!(names("Anyma B2B Solomun | Tomorrowland 2025", "anyma"));
        assert!(names("Korolova - Live @ Hï Ibiza", "Korolova"));
        assert!(!names("Anymal live in Berlin", "Anyma"));
        assert!(!names("ARTBAT live at Tomorrowland", "Anyma"));
    }

    #[test]
    fn ages_order_newest_first() {
        assert!(age("3 days ago") < age("2 weeks ago"));
        assert!(age("Streamed 11 months ago") < age("1 year ago"));
        assert_eq!(age(""), u64::MAX);
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_artist_sets() {
        let it = client();
        let sets = it.artist_sets("Anyma").await.expect("sets");
        for set in sets.iter().take(5) {
            println!("{} | {} | {:?}s", set.title, set.subtitle, set.duration);
        }
        println!("{} sets", sets.len());
        assert!(!sets.is_empty());
        let artist = it
            .search("Anyma", SearchFilter::Artists)
            .await
            .expect("search");
        let id = artist
            .shelves
            .iter()
            .flat_map(|s| &s.items)
            .find(|i| i.kind == Kind::Artist)
            .map(|i| i.id.clone())
            .expect("an artist");
        let page = it.artist(&id).await.expect("artist");
        for shelf in &page.shelves {
            println!(
                "shelf {:?}: {} items, list {}",
                shelf.title,
                shelf.items.len(),
                shelf.list
            );
        }
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_pages_parse() {
        let it = client();
        let home = it.home(None).await.expect("home");
        assert!(!home.shelves.is_empty(), "home has shelves");
        let more = it
            .home(home.continuation.as_deref())
            .await
            .expect("home page 2");
        println!(
            "home: {} shelves, then {}",
            home.shelves.len(),
            more.shelves.len()
        );

        let search = it
            .search("daft punk", SearchFilter::All)
            .await
            .expect("search");
        println!("top: {:?}", search.top.as_ref().map(|t| (&t.title, t.kind)));
        let find = |kind: Kind| {
            search
                .shelves
                .iter()
                .flat_map(|s| &s.items)
                .find(|i| i.kind == kind)
                .cloned()
                .expect("a result of each kind")
        };
        let album = it.album(&find(Kind::Album).id).await.expect("album");
        assert!(!album.tracks.is_empty(), "album has tracks");
        let artist = it.artist(&find(Kind::Artist).id).await.expect("artist");
        assert!(
            !artist.shelves.is_empty() && !artist.name.is_empty(),
            "artist page"
        );
        let song = find(Kind::Song);
        let radio = it.radio(&song.id).await.expect("radio");
        assert!(radio.len() > 5, "radio has tracks");
        let songs = it
            .search("daft punk", SearchFilter::Songs)
            .await
            .expect("songs");
        assert!(
            songs
                .shelves
                .iter()
                .all(|s| s.items.iter().all(Item::is_playable))
        );
        let playlists = it
            .search("daft punk", SearchFilter::Playlists)
            .await
            .expect("playlists");
        let playlist_id = playlists.shelves[0].items[0].id.clone();
        let playlist = it.playlist(&playlist_id).await.expect("playlist");
        assert!(!playlist.tracks.is_empty(), "playlist has tracks");
        println!(
            "album {} ({} tracks), artist {} ({} shelves), playlist {} ({} tracks, more: {})",
            album.title,
            album.tracks.len(),
            artist.name,
            artist.shelves.len(),
            playlist.title,
            playlist.tracks.len(),
            playlist.continuation.is_some()
        );
        if let Some(more) = artist.shelves.iter().find_map(|s| s.more.clone()) {
            let page = it.browse(&more, None).await.expect("more");
            println!("more: {} shelves", page.shelves.len());
        }
        assert_eq!(it.rating(&song.id).await.expect("rating"), Rating::None);
    }
}
