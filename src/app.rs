//! Application state. Views in `ui/` read it and push `Action`s; the actions
//! are applied after the frame is drawn, so no view mutates state while it
//! is borrowed. Answers from the backend and the player arrive as events.
//!
//! The interface is optimistic: a control shows its result at once (a
//! clicked song is the playing song, a like is filled) and the backend makes
//! it true behind it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use fastframe_now_playing::{self as now_playing, NowPlaying};

use crate::audio::{EQ_RANGE, Eq, EqPreset};
use crate::backend::{Backend, Event, Fill, Paths};
use crate::images;
use crate::lyrics;
use crate::model::{
    Account, ArtistPage, Browse, Collection, Feed, Item, Kind, Loadable, Lyrics, Page, Rating,
    SearchFilter, SearchResults, Watch,
};
use crate::player::{self, Command, Player, TrackRef};
use crate::queue::Repeat;
use crate::settings::Settings;

pub const LIBRARY_PLAYLISTS: &str = "FEmusic_liked_playlists";
pub const LIBRARY_ALBUMS: &str = "FEmusic_liked_albums";
pub const LIBRARY_ARTISTS: &str = "FEmusic_library_corpus_artists";
pub const LIKED_SONGS: &str = "LM";

/// Previous restarts the track once it has played this long.
const RESTART_AFTER: f64 = 3.0;
const SAVE_EVERY: Duration = Duration::from_secs(2);
const TOAST_FOR: Duration = Duration::from_secs(4);
/// Unplayable tracks skipped in a row before playback stops trying.
const MAX_SKIPS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Queue,
    Lyrics,
}

pub enum Action {
    Open(Page),
    /// A card or row was clicked: open it, or play it.
    OpenItem(Item),
    Back,
    Forward,
    Search(String),
    SearchFilter(SearchFilter),
    PlayAll {
        tracks: Vec<Item>,
        start: usize,
        source: Option<String>,
    },
    /// Plays an album or playlist from its card, loading it first.
    PlayCollection {
        id: String,
        shuffle: bool,
    },
    PlayWatch(Watch),
    PlayNext(Item),
    AddToQueue(Item),
    StartRadio(Item),
    QueueJump(usize),
    QueueRemove(usize),
    ClearQueue,
    TogglePlay,
    Next,
    Previous,
    Seek(f64),
    Volume(f32),
    ToggleShuffle,
    CycleRepeat,
    Rate(String, Rating),
    ToggleSide(Side),
    LoadMoreHome,
    LoadMoreFeed(Browse),
    Retry(Page),
    SignIn,
    SignOut,
    SetBrowser(String),
    SetAutoplay(bool),
    SetCrossfade(f32),
    SetEqEnabled(bool),
    SetEqPreset(EqPreset),
    SetEqBand(usize, f32),
}

pub struct App {
    pub backend: Backend,
    pub player: Player,
    controls: Option<NowPlaying>,
    pub settings: Settings,
    dirty: bool,
    saved_at: Instant,

    pub page: Page,
    back: Vec<Page>,
    forward: Vec<Page>,
    pub home: Loadable<Feed>,
    pub home_more: bool,
    pub collections: HashMap<String, Loadable<Collection>>,
    pub artists: HashMap<String, Loadable<ArtistPage>>,
    pub feeds: HashMap<Browse, Loadable<Feed>>,
    pub feeds_more: Option<Browse>,
    pub library: HashMap<&'static str, Loadable<Vec<Item>>>,
    pub search_text: String,
    pub search_query: String,
    pub search_filter: SearchFilter,
    pub searches: HashMap<(String, SearchFilter), Loadable<SearchResults>>,

    pub signed_in: bool,
    pub account: Option<Account>,
    pub signing_in: bool,
    pub sign_in_error: Option<String>,

    pub playing: bool,
    pub loading: bool,
    ended: bool,
    skips: u32,
    next_sent: Option<TrackRef>,
    /// The collection the queue was built from, so its later pages join it.
    queue_source: Option<String>,
    pending_play: Option<(String, bool)>,
    autoplay_for: Option<String>,
    pub rating: Option<(String, Rating)>,
    pub lyrics: HashMap<String, Loadable<Option<Lyrics>>>,
    pub side: Option<Side>,
    art_file: Option<(String, std::path::PathBuf)>,
    pub toasts: Vec<(String, Instant)>,
    pub actions: Vec<Action>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        crate::theme::install(&ctx);
        let paths = Paths::new();
        let settings = Settings::load(&paths.state);
        let backend = match Backend::new(ctx.clone(), paths) {
            Ok(backend) => backend,
            Err(error) => panic!("could not start the network runtime: {error}"),
        };
        ctx.add_bytes_loader(std::sync::Arc::new(images::Loader::new(
            backend.http.clone(),
            backend.handle(),
        )));
        let emit = backend.emitter();
        let player = Player::start(
            backend.handle(),
            backend.http.clone(),
            backend.ytdlp.clone(),
            Eq {
                enabled: settings.eq_enabled,
                gains: settings.eq_gains(),
            },
            settings.volume,
            settings.crossfade,
            move |event| emit(Event::Player(event)),
        );
        // macOS delivers the media keys on the main thread, which this is.
        let wake = ctx.clone();
        let controls = NowPlaying::start(now_playing::App::new("ytfast", "ytfast"), move || {
            wake.request_repaint()
        });

        let mut app = Self {
            signed_in: backend.api.signed_in(),
            backend,
            player,
            controls: Some(controls),
            settings,
            dirty: false,
            saved_at: Instant::now(),
            page: Page::Home,
            back: Vec::new(),
            forward: Vec::new(),
            home: Loadable::NotLoaded,
            home_more: false,
            collections: HashMap::new(),
            artists: HashMap::new(),
            feeds: HashMap::new(),
            feeds_more: None,
            library: HashMap::new(),
            search_text: String::new(),
            search_query: String::new(),
            search_filter: SearchFilter::All,
            searches: HashMap::new(),
            account: None,
            signing_in: false,
            sign_in_error: None,
            playing: false,
            loading: false,
            ended: false,
            skips: 0,
            next_sent: None,
            queue_source: None,
            pending_play: None,
            autoplay_for: None,
            rating: None,
            lyrics: HashMap::new(),
            side: None,
            art_file: None,
            toasts: Vec::new(),
            actions: Vec::new(),
        };
        app.ensure_loaded(&Page::Home);
        if app.signed_in {
            app.load_account();
        }
        let (http, ytdlp) = (app.backend.http.clone(), app.backend.ytdlp.clone());
        app.backend
            .run(async move { Event::Ytdlp(ytdlp.ensure(&http).await) });
        // Back where the last run left off, paused.
        if let Some(track) = app.current_ref() {
            let from = app.settings.position;
            app.player.send(Command::Play {
                track,
                from,
                paused: true,
            });
            app.track_changed();
        }
        app
    }

    pub fn current(&self) -> Option<&Item> {
        self.settings.queue.current()
    }

    fn current_ref(&self) -> Option<TrackRef> {
        self.current().map(track_ref)
    }

    pub fn position(&self) -> f64 {
        self.player.position()
    }

    pub fn length(&self) -> Option<f64> {
        self.player
            .length()
            .or_else(|| self.current().and_then(|t| t.duration).map(f64::from))
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push((text.into(), Instant::now() + TOAST_FOR));
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    // -----------------------------------------------------------------------
    // Loading

    fn load_account(&mut self) {
        let api = self.backend.api.clone();
        self.backend
            .run(async move { Event::Account(api.account().await) });
        for id in [LIBRARY_PLAYLISTS, LIBRARY_ALBUMS, LIBRARY_ARTISTS] {
            self.load_library(id);
        }
    }

    fn load_library(&mut self, browse_id: &'static str) {
        self.library.insert(browse_id, Loadable::Loading);
        let api = self.backend.api.clone();
        self.backend.run(async move {
            Event::Library {
                browse_id,
                result: api.library(browse_id).await,
            }
        });
    }

    fn load_home(&mut self, more: bool) {
        let cursor = match (&self.home, more) {
            (Loadable::Loaded(feed), true) => match &feed.continuation {
                Some(cursor) => Some(cursor.clone()),
                None => return,
            },
            (_, true) => return,
            _ => None,
        };
        if more {
            self.home_more = true;
        } else {
            self.home = Loadable::Loading;
        }
        let api = self.backend.api.clone();
        self.backend.run(async move {
            Event::Home {
                more,
                result: api.home(cursor.as_deref()).await,
            }
        });
    }

    fn load_collection(&mut self, id: &str, album: bool) {
        self.collections.insert(id.to_string(), Loadable::Loading);
        let (api, id) = (self.backend.api.clone(), id.to_string());
        self.backend.run(async move {
            let result = if album {
                api.album(&id).await
            } else {
                api.playlist(&id).await
            };
            Event::Collection { id, result }
        });
    }

    fn load_collection_more(&mut self, id: String, token: String) {
        let api = self.backend.api.clone();
        self.backend.run(async move {
            Event::CollectionMore {
                result: api.playlist_more(&token).await,
                id,
            }
        });
    }

    fn load_feed(&mut self, target: Browse, more: bool) {
        let cursor = match (self.feeds.get(&target), more) {
            (Some(Loadable::Loaded(feed)), true) => match &feed.continuation {
                Some(cursor) => Some(cursor.clone()),
                None => return,
            },
            (_, true) => return,
            _ => None,
        };
        if more {
            self.feeds_more = Some(target.clone());
        } else {
            self.feeds.insert(target.clone(), Loadable::Loading);
        }
        let api = self.backend.api.clone();
        self.backend.run(async move {
            let result = api.browse(&target, cursor.as_deref()).await;
            Event::Feed {
                target,
                more,
                result,
            }
        });
    }

    fn load_search(&mut self) {
        let key = (self.search_query.clone(), self.search_filter);
        if key.0.trim().is_empty()
            || self
                .searches
                .get(&key)
                .is_some_and(|s| !matches!(s, Loadable::Failed(_)))
        {
            return;
        }
        self.searches.insert(key.clone(), Loadable::Loading);
        let api = self.backend.api.clone();
        self.backend.run(async move {
            let (query, filter) = key;
            let result = api.search(&query, filter).await;
            Event::Search {
                query,
                filter,
                result,
            }
        });
    }

    pub fn ensure_loaded(&mut self, page: &Page) {
        match page {
            Page::Home if self.home.needs_load() => self.load_home(false),
            Page::Album(id) if !self.collections.contains_key(id) => self.load_collection(id, true),
            Page::Playlist(id) if !self.collections.contains_key(id) => {
                self.load_collection(id, false)
            }
            Page::Artist(id) if !self.artists.contains_key(id) => {
                self.artists.insert(id.clone(), Loadable::Loading);
                let (api, id) = (self.backend.api.clone(), id.clone());
                self.backend.run(async move {
                    Event::Artist {
                        result: api.artist(&id).await,
                        id,
                    }
                });
            }
            Page::Browse(target) if !self.feeds.contains_key(target) => {
                self.load_feed(target.clone(), false)
            }
            Page::Search => self.load_search(),
            Page::Library if self.signed_in => {
                for id in [LIBRARY_PLAYLISTS, LIBRARY_ALBUMS, LIBRARY_ARTISTS] {
                    if self
                        .library
                        .get(id)
                        .is_none_or(|l| matches!(l, Loadable::Failed(_)))
                    {
                        self.load_library(id);
                    }
                }
            }
            _ => {}
        }
    }

    fn want_lyrics(&mut self) {
        let Some(track) = self.current().cloned() else {
            return;
        };
        if self.side != Some(Side::Lyrics) || self.lyrics.contains_key(&track.id) {
            return;
        }
        self.lyrics.insert(track.id.clone(), Loadable::Loading);
        let http = self.backend.http.clone();
        self.backend.run(async move {
            Event::Lyrics {
                result: lyrics::fetch(&http, &track).await,
                id: track.id,
            }
        });
    }

    // -----------------------------------------------------------------------
    // Playback

    fn play_current(&mut self, from: f64) {
        let Some(track) = self.current_ref() else {
            return;
        };
        self.player.send(Command::Play {
            track,
            from,
            paused: false,
        });
        self.playing = true;
        self.loading = true;
        self.ended = false;
        self.track_changed();
    }

    /// Everything that follows the current track changing.
    fn track_changed(&mut self) {
        self.dirty = true;
        self.sync_next();
        let Some(track) = self.current().cloned() else {
            return;
        };
        self.rating = None;
        if self.signed_in {
            let (api, id) = (self.backend.api.clone(), track.id.clone());
            self.backend.run(async move {
                Event::Rating {
                    result: api.rating(&id).await,
                    id,
                }
            });
        }
        if let Some(url) = images::art_url(&track.thumbnails, 544) {
            self.backend.save_art(track.id.clone(), url);
        }
        self.want_lyrics();
        self.maybe_autoplay();
    }

    /// Tells the player what follows the current track, when that changed.
    fn sync_next(&mut self) {
        let next = self.settings.queue.peek_next().map(track_ref);
        if next != self.next_sent {
            self.next_sent = next.clone();
            self.player.send(Command::Next(next));
        }
    }

    /// With autoplay on, the last queued track brings its radio along.
    fn maybe_autoplay(&mut self) {
        let queue = &self.settings.queue;
        if !self.settings.autoplay || queue.repeat != Repeat::Off || !queue.is_last() {
            return;
        }
        let Some(seed) = queue.current().map(|t| t.id.clone()) else {
            return;
        };
        if self.autoplay_for.as_ref() == Some(&seed) {
            return;
        }
        self.autoplay_for = Some(seed.clone());
        let api = self.backend.api.clone();
        self.backend.run(async move {
            Event::Queue {
                result: api.radio(&seed).await,
                fill: Fill::Autoplay(seed),
            }
        });
    }

    fn play_all(&mut self, tracks: Vec<Item>, start: usize, source: Option<String>) {
        let tracks: Vec<Item> = tracks.into_iter().filter(Item::is_playable).collect();
        if tracks.is_empty() {
            return;
        }
        self.queue_source = source;
        self.autoplay_for = None;
        self.settings.queue.replace(tracks, start);
        self.play_current(0.0);
    }

    // -----------------------------------------------------------------------
    // Actions

    fn apply(&mut self, action: Action) {
        match action {
            Action::Open(page) => self.open(page),
            Action::OpenItem(item) => self.open_item(item),
            Action::Back => {
                if let Some(page) = self.back.pop() {
                    self.forward
                        .push(std::mem::replace(&mut self.page, page.clone()));
                    self.ensure_loaded(&page);
                }
            }
            Action::Forward => {
                if let Some(page) = self.forward.pop() {
                    self.back
                        .push(std::mem::replace(&mut self.page, page.clone()));
                    self.ensure_loaded(&page);
                }
            }
            Action::Search(query) => {
                self.search_query = query.trim().to_string();
                self.search_text = self.search_query.clone();
                self.open(Page::Search);
                self.load_search();
            }
            Action::SearchFilter(filter) => {
                self.search_filter = filter;
                self.load_search();
            }
            Action::PlayAll {
                tracks,
                start,
                source,
            } => self.play_all(tracks, start, source),
            Action::PlayCollection { id, shuffle } => {
                if let Some(collection) = self.collections.get(&id).and_then(Loadable::get) {
                    let tracks = collection.tracks.clone();
                    self.play_collection(tracks, shuffle, id);
                } else {
                    self.pending_play = Some((id.clone(), shuffle));
                    let album = id.starts_with("MPRE");
                    self.load_collection(&id, album);
                }
            }
            Action::PlayWatch(watch) => {
                let api = self.backend.api.clone();
                self.backend.run(async move {
                    let result = match (&watch.playlist_id, &watch.video_id) {
                        (Some(playlist), video) => {
                            api.watch_playlist(playlist, video.as_deref()).await
                        }
                        (None, Some(video)) => api.radio(video).await,
                        (None, None) => Ok(Vec::new()),
                    };
                    Event::Queue {
                        result,
                        fill: Fill::Replace,
                    }
                });
            }
            Action::PlayNext(item) => {
                self.toast(format!("“{}” plays next", item.title));
                if self.current().is_none() {
                    self.play_all(vec![item], 0, None);
                } else {
                    self.settings.queue.play_next(item);
                    self.queue_edited();
                }
            }
            Action::AddToQueue(item) => {
                self.toast(format!("Added “{}” to the queue", item.title));
                if self.current().is_none() {
                    self.play_all(vec![item], 0, None);
                } else {
                    self.settings.queue.add([item]);
                    self.queue_edited();
                }
            }
            Action::StartRadio(item) => self.play_song(item),
            Action::QueueJump(at) => {
                if self.settings.queue.jump(at).is_some() {
                    self.play_current(0.0);
                }
            }
            Action::QueueRemove(at) => {
                if self.settings.queue.remove(at) {
                    if self.current().is_some() {
                        self.play_current(0.0);
                    } else {
                        self.player.send(Command::Stop);
                        self.playing = false;
                    }
                }
                self.queue_edited();
            }
            Action::ClearQueue => {
                self.settings.queue.clear_upcoming();
                self.queue_source = None;
                self.queue_edited();
            }
            Action::TogglePlay => self.toggle_play(),
            Action::Next => {
                if self.settings.queue.advance(true).is_some() {
                    self.play_current(0.0);
                }
            }
            Action::Previous => {
                if self.position() > RESTART_AFTER || self.settings.queue.index == Some(0) {
                    self.seek(0.0);
                } else if self.settings.queue.previous().is_some() {
                    self.play_current(0.0);
                }
            }
            Action::Seek(seconds) => self.seek(seconds),
            Action::Volume(volume) => {
                self.settings.volume = volume.clamp(0.0, 1.0);
                self.player.send(Command::Volume(self.settings.volume));
                self.dirty = true;
            }
            Action::ToggleShuffle => {
                let on = !self.settings.queue.shuffle;
                self.settings.queue.set_shuffle(on);
                self.queue_edited();
            }
            Action::CycleRepeat => {
                self.settings.queue.cycle_repeat();
                self.queue_edited();
            }
            Action::Rate(id, rating) => {
                if self.current().is_some_and(|t| t.id == id) {
                    self.rating = Some((id.clone(), rating));
                }
                let api = self.backend.api.clone();
                self.backend.run(async move {
                    Event::Rated {
                        result: api.rate(&id, rating).await,
                        id,
                    }
                });
            }
            Action::ToggleSide(side) => {
                self.side = if self.side == Some(side) {
                    None
                } else {
                    Some(side)
                };
                self.want_lyrics();
            }
            Action::LoadMoreHome => {
                if !self.home_more {
                    self.load_home(true);
                }
            }
            Action::LoadMoreFeed(target) => {
                if self.feeds_more.is_none() {
                    self.load_feed(target, true);
                }
            }
            Action::Retry(page) => {
                match &page {
                    Page::Home => self.home = Loadable::NotLoaded,
                    Page::Album(id) | Page::Playlist(id) => _ = self.collections.remove(id),
                    Page::Artist(id) => _ = self.artists.remove(id),
                    Page::Browse(target) => _ = self.feeds.remove(target),
                    Page::Search => {
                        _ = self
                            .searches
                            .remove(&(self.search_query.clone(), self.search_filter))
                    }
                    Page::Library => self.library.clear(),
                    Page::Settings => {}
                }
                self.ensure_loaded(&page);
            }
            Action::SignIn => {
                self.signing_in = true;
                self.sign_in_error = None;
                self.backend.sign_in(self.settings.browser.clone());
            }
            Action::SignOut => {
                crate::auth::clear();
                self.backend.api.set_session(None);
                self.signed_in = false;
                self.account = None;
                self.library.clear();
                self.rating = None;
                self.home = Loadable::NotLoaded;
                self.ensure_loaded(&Page::Home);
            }
            Action::SetBrowser(browser) => {
                self.settings.browser = browser;
                self.dirty = true;
            }
            Action::SetAutoplay(on) => {
                self.settings.autoplay = on;
                self.dirty = true;
                self.maybe_autoplay();
            }
            Action::SetCrossfade(seconds) => {
                self.settings.crossfade = seconds;
                self.player.send(Command::Crossfade(seconds));
                self.dirty = true;
            }
            Action::SetEqEnabled(on) => {
                self.settings.eq_enabled = on;
                self.eq_changed();
            }
            Action::SetEqPreset(preset) => {
                if preset == EqPreset::Custom {
                    self.settings.eq_custom = self.settings.eq_gains();
                }
                self.settings.eq_preset = preset;
                self.eq_changed();
            }
            Action::SetEqBand(band, gain) => {
                // Moving a band turns a preset into a custom curve that starts
                // from the preset's own numbers.
                self.settings.eq_custom = self.settings.eq_gains();
                self.settings.eq_custom[band] = gain.clamp(-EQ_RANGE, EQ_RANGE);
                self.settings.eq_preset = EqPreset::Custom;
                self.eq_changed();
            }
        }
    }

    fn eq_changed(&mut self) {
        self.player.send(Command::Eq(Eq {
            enabled: self.settings.eq_enabled,
            gains: self.settings.eq_gains(),
        }));
        self.dirty = true;
    }

    fn queue_edited(&mut self) {
        self.dirty = true;
        self.sync_next();
        self.maybe_autoplay();
    }

    fn seek(&mut self, seconds: f64) {
        self.player.send(Command::Seek(seconds.max(0.0)));
        if self.ended {
            self.play_current(seconds);
        }
    }

    fn toggle_play(&mut self) {
        if self.current().is_none() {
            return;
        }
        if self.ended {
            self.play_current(0.0);
        } else if self.playing {
            self.player.send(Command::Pause);
            self.playing = false;
        } else {
            self.player.send(Command::Resume);
            self.playing = true;
        }
    }

    fn open(&mut self, page: Page) {
        if page != self.page {
            self.back
                .push(std::mem::replace(&mut self.page, page.clone()));
            self.forward.clear();
        }
        self.ensure_loaded(&page);
    }

    fn open_item(&mut self, item: Item) {
        if item.is_playable() {
            return self.play_song(item);
        }
        if let Some(video_id) = &item.play_video_id {
            let video = Item {
                kind: Kind::Video,
                id: video_id.clone(),
                ..item
            };
            return self.play_song(video);
        }
        if let Some(page) = Page::for_item(&item) {
            self.open(page);
        }
    }

    /// One song: it plays now and its radio follows, as in YouTube Music.
    fn play_song(&mut self, item: Item) {
        let seed = item.id.clone();
        self.play_all(vec![item], 0, None);
        self.autoplay_for = Some(seed.clone());
        let api = self.backend.api.clone();
        self.backend.run(async move {
            Event::Queue {
                result: api.radio(&seed).await,
                fill: Fill::After(seed),
            }
        });
    }

    fn play_collection(&mut self, tracks: Vec<Item>, shuffle: bool, id: String) {
        if shuffle && !self.settings.queue.shuffle {
            self.settings.queue.shuffle = true;
        }
        let start = if shuffle {
            rand::random_range(0..tracks.len().max(1))
        } else {
            0
        };
        self.play_all(tracks, start, Some(id));
    }

    // -----------------------------------------------------------------------
    // Events

    fn handle(&mut self, event: Event) {
        match event {
            Event::Home { more, result } => {
                if more {
                    self.home_more = false;
                    match (result, self.home.get_mut()) {
                        (Ok(page), Some(feed)) => {
                            feed.shelves.extend(page.shelves);
                            feed.continuation = page.continuation;
                        }
                        (Err(error), Some(feed)) => {
                            feed.continuation = None;
                            log::warn!("home: {error:#}");
                        }
                        _ => {}
                    }
                } else {
                    self.home = Loadable::from_result(result);
                }
            }
            Event::Collection { id, result } => {
                let loaded = Loadable::from_result(result);
                if let Some(collection) = loaded.get() {
                    if let Some(token) = collection.continuation.clone() {
                        self.load_collection_more(id.clone(), token);
                    }
                    if let Some((_, shuffle)) = self.pending_play.take_if(|(p, _)| *p == id) {
                        let tracks = collection.tracks.clone();
                        self.play_collection(tracks, shuffle, id.clone());
                    }
                }
                self.collections.insert(id, loaded);
            }
            Event::CollectionMore { id, result } => {
                let Some(collection) = self.collections.get_mut(&id).and_then(Loadable::get_mut)
                else {
                    return;
                };
                match result {
                    Ok((tracks, next)) => {
                        let fresh: Vec<Item> = tracks
                            .into_iter()
                            .filter(|t| !collection.tracks.iter().any(|c| c.id == t.id))
                            .collect();
                        collection.tracks.extend(fresh.iter().cloned());
                        collection.continuation = next.clone().filter(|_| !fresh.is_empty());
                        if self.queue_source.as_ref() == Some(&id) {
                            self.settings.queue.add(fresh);
                            if self.settings.queue.shuffle {
                                self.settings.queue.set_shuffle(true);
                            }
                            self.queue_edited();
                        }
                        if let Some(token) = self
                            .collections
                            .get(&id)
                            .and_then(Loadable::get)
                            .and_then(|c| c.continuation.clone())
                        {
                            self.load_collection_more(id, token);
                        }
                    }
                    Err(error) => {
                        collection.continuation = None;
                        log::warn!("{id}: {error:#}");
                    }
                }
            }
            Event::Artist { id, result } => {
                self.artists.insert(id, Loadable::from_result(result));
            }
            Event::Feed {
                target,
                more,
                result,
            } => {
                if more {
                    self.feeds_more = None;
                    if let Some(feed) = self.feeds.get_mut(&target).and_then(Loadable::get_mut) {
                        match result {
                            Ok(page) => {
                                feed.shelves.extend(page.shelves);
                                feed.continuation = page.continuation;
                            }
                            Err(_) => feed.continuation = None,
                        }
                    }
                } else {
                    self.feeds.insert(target, Loadable::from_result(result));
                }
            }
            Event::Search {
                query,
                filter,
                result,
            } => {
                self.searches
                    .insert((query, filter), Loadable::from_result(result));
            }
            Event::Library { browse_id, result } => {
                self.library
                    .insert(browse_id, Loadable::from_result(result));
            }
            Event::Queue { fill, result } => self.queue_filled(fill, result),
            Event::Rating { id, result } => match result {
                Ok(rating) if self.current().is_some_and(|t| t.id == id) => {
                    self.rating = Some((id, rating))
                }
                Ok(_) => {}
                Err(error) => log::warn!("rating: {error:#}"),
            },
            Event::Rated { id, result } => {
                if let Err(error) = result {
                    self.toast(format!("Could not save that rating: {error:#}"));
                    let api = self.backend.api.clone();
                    self.backend.run(async move {
                        Event::Rating {
                            result: api.rating(&id).await,
                            id,
                        }
                    });
                } else {
                    // Liked songs is a playlist the server rebuilds: load it again.
                    self.collections.remove(LIKED_SONGS);
                }
            }
            Event::Lyrics { id, result } => {
                self.lyrics.insert(id, Loadable::from_result(result));
            }
            Event::Account(result) => {
                match result {
                    Ok(Some(account)) => self.account = Some(account),
                    Ok(None) => {
                        self.account = None;
                        self.toast("YouTube no longer accepts the saved session. Sign in again in Settings.");
                    }
                    Err(error) => log::warn!("account: {error:#}"),
                }
            }
            Event::SignedIn(result) => {
                self.signing_in = false;
                match result {
                    Ok(session) => {
                        self.backend.api.set_session(Some(session));
                        self.signed_in = true;
                        self.load_account();
                        self.home = Loadable::NotLoaded;
                        self.ensure_loaded(&Page::Home);
                    }
                    Err(error) => self.sign_in_error = Some(format!("{error:#}")),
                }
            }
            Event::Art { id, path } => self.art_file = Some((id, path)),
            Event::Ytdlp(result) => {
                if let Err(error) = result {
                    log::warn!("yt-dlp update: {error:#}");
                }
            }
            Event::Player(event) => self.player_event(event),
        }
    }

    fn queue_filled(&mut self, fill: Fill, result: anyhow::Result<Vec<Item>>) {
        let tracks = match result {
            Ok(tracks) => tracks,
            Err(error) => {
                if fill == Fill::Replace {
                    self.toast(format!("Could not start that: {error:#}"));
                }
                if matches!(fill, Fill::Autoplay(_)) {
                    self.autoplay_for = None;
                }
                return;
            }
        };
        match fill {
            Fill::Replace => self.play_all(tracks, 0, None),
            Fill::After(seed) | Fill::Autoplay(seed) => {
                let queue = &mut self.settings.queue;
                if queue.current().is_none_or(|t| t.id != seed) || !queue.is_last() {
                    return;
                }
                let fresh: Vec<Item> = tracks
                    .into_iter()
                    .filter(|t| !queue.tracks.iter().any(|q| q.id == t.id))
                    .collect();
                queue.add(fresh);
                self.queue_edited();
            }
        }
    }

    fn player_event(&mut self, event: player::Event) {
        match event {
            player::Event::Started { id } => {
                if self.current().is_some_and(|t| t.id == id) {
                    self.loading = false;
                    self.skips = 0;
                }
            }
            player::Event::Advanced { id } => {
                self.next_sent = None;
                self.settings.queue.advance(false);
                if self.current().is_none_or(|t| t.id != id) {
                    // The queue changed under the player: play what it says now.
                    self.play_current(0.0);
                } else {
                    self.loading = false;
                    self.track_changed();
                }
            }
            player::Event::Ended => {
                self.playing = false;
                self.ended = true;
                self.next_sent = None;
            }
            player::Event::Failed { id, error } => {
                if self.current().is_none_or(|t| t.id != id) {
                    return;
                }
                self.loading = false;
                self.toast(format!("Could not play this track: {error}"));
                self.skips += 1;
                if self.skips < MAX_SKIPS && self.settings.queue.advance(true).is_some() {
                    self.play_current(0.0);
                } else {
                    self.playing = false;
                    self.skips = 0;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Desktop media controls

    fn sync_controls(&mut self, ctx: &egui::Context) {
        let Some(mut controls) = self.controls.take() else {
            return;
        };
        for command in controls.commands() {
            let action = match command {
                now_playing::Command::Play if !self.playing => Action::TogglePlay,
                now_playing::Command::Pause | now_playing::Command::Stop if self.playing => {
                    Action::TogglePlay
                }
                now_playing::Command::PlayPause => Action::TogglePlay,
                now_playing::Command::Next => Action::Next,
                now_playing::Command::Previous => Action::Previous,
                now_playing::Command::SeekBy(ms) => {
                    Action::Seek(self.player.position() + ms as f64 / 1000.0)
                }
                now_playing::Command::SetPosition { position, .. } => {
                    Action::Seek(position.as_secs_f64())
                }
                now_playing::Command::SetVolume(volume) => Action::Volume(volume as f32),
                now_playing::Command::SetShuffle(on) if on != self.settings.queue.shuffle => {
                    Action::ToggleShuffle
                }
                now_playing::Command::Raise => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    continue;
                }
                now_playing::Command::Quit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    continue;
                }
                _ => continue,
            };
            self.actions.push(action);
        }
        let queue = &self.settings.queue;
        let track = queue.current().map(|t| now_playing::Track {
            id: t.id.clone(),
            title: t.title.clone(),
            artists: if t.artists.is_empty() {
                vec![t.subtitle.clone()]
            } else {
                t.artists.iter().map(|a| a.name.clone()).collect()
            },
            album: t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            duration: self.length().map(Duration::from_secs_f64),
            art_file: self
                .art_file
                .as_ref()
                .filter(|(id, _)| *id == t.id)
                .map(|(_, p)| p.clone()),
            art_url: images::art_url(&t.thumbnails, 544),
            url: Some(format!("https://music.youtube.com/watch?v={}", t.id)),
            ..now_playing::Track::default()
        });
        let state = now_playing::State {
            playback: match (&track, self.playing) {
                (None, _) => now_playing::Playback::Stopped,
                (_, true) => now_playing::Playback::Playing,
                (_, false) => now_playing::Playback::Paused,
            },
            track,
            position: Duration::from_secs_f64(self.player.position()),
            volume: Some(f64::from(self.settings.volume)),
            shuffle: Some(queue.shuffle),
            repeat: Some(match queue.repeat {
                Repeat::Off => now_playing::Repeat::Off,
                Repeat::All => now_playing::Repeat::Playlist,
                Repeat::One => now_playing::Repeat::Track,
            }),
            controls: now_playing::Controls {
                next: queue.peek_next().is_some() || !queue.is_last(),
                ..Default::default()
            },
        };
        controls.update(state);
        self.controls = Some(controls);
    }

    fn save(&mut self) {
        self.settings.position = self.player.position();
        if let Err(error) = self.settings.save(&self.backend.paths.state) {
            log::warn!("could not save state: {error}");
        }
        self.dirty = false;
        self.saved_at = Instant::now();
    }
}

fn track_ref(item: &Item) -> TrackRef {
    TrackRef {
        id: item.id.clone(),
        duration: item.duration.map(f64::from),
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        for event in self.backend.events() {
            self.handle(event);
        }
        self.sync_controls(&ctx);
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.actions.push(Action::TogglePlay);
        }

        crate::ui::show(self, ui);

        for action in std::mem::take(&mut self.actions) {
            self.apply(action);
        }
        let now = Instant::now();
        self.toasts.retain(|(_, until)| *until > now);
        if self.dirty && self.saved_at.elapsed() > SAVE_EVERY {
            self.save();
        }
        if self.playing || !self.toasts.is_empty() || self.loading {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save();
    }
}
