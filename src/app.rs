//! Application state. Views in `ui/` read it and push `Action`s; the actions
//! are applied after the frame is drawn, so no view mutates state while it
//! is borrowed. Answers from the backend and the player arrive as events.
//!
//! The interface is optimistic: a control shows its result at once (a
//! clicked song is the playing song, a like is filled) and the backend makes
//! it true behind it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fastframe_now_playing::{self as now_playing, NowPlaying};

use crate::audio::{EQ_RANGE, Eq, EqPreset};
use crate::auth::Session;
use crate::backend::{Backend, Event, Fill, Paths};
use crate::images;
use crate::lyrics;
use crate::model::{
    Account, ArtistPage, Browse, Collection, Feed, Item, Kind, Loadable, Lyrics, Page, Rating,
    SearchFilter, SearchResults, Shelf, Watch, playlist_browse_id,
};
use crate::player::{self, Command, Player, TrackRef};
use crate::queue::Repeat;
use crate::settings::{HomeArtist, Settings};
use crate::stream::Resolver;

pub const LIBRARY_PLAYLISTS: &str = "FEmusic_liked_playlists";
pub const LIBRARY_ALBUMS: &str = "FEmusic_liked_albums";
pub const LIBRARY_ARTISTS: &str = "FEmusic_library_corpus_artists";
/// Liked Music's browse id, which its page is filed under.
const LIKED_MUSIC: &str = "VLLM";

/// Previous restarts the track once it has played this long.
const RESTART_AFTER: f64 = 3.0;
const SAVE_EVERY: Duration = Duration::from_secs(2);
const TOAST_FOR: Duration = Duration::from_secs(4);
/// How often a playing, loading or toasting window redraws.
const REDRAW_EVERY: Duration = Duration::from_millis(200);
/// Cover size sent to the desktop's Now Playing panels.
const NOW_PLAYING_ART_PX: u32 = 544;
const OWNERSHIP_CHECKS_AT_ONCE: usize = 4;
/// Unplayable tracks skipped in a row before playback stops trying.
const MAX_SKIPS: u32 = 3;
/// Seconds a track plays before it goes to the YouTube history, so a skip
/// straight past it does not count as a listen.
const HISTORY_AFTER: f64 = 10.0;
/// How long the pointer rests on a track before it is resolved ahead.
const WARM_AFTER: Duration = Duration::from_millis(150);
/// Tracks resolved ahead when an album or playlist opens.
const WARM_TOP: usize = 3;
const RECENT_SEARCHES: usize = 10;
/// Unmuting never lands on silence, should the app have started muted.
const MIN_UNMUTED: f32 = 0.2;

/// What the menu bar's seek and volume items move by.
#[cfg(target_os = "macos")]
const MENU_SEEK: f64 = 10.0;
#[cfg(target_os = "macos")]
const MENU_VOLUME_STEP: f32 = 0.05;
const REPO_URL: &str = "https://github.com/mateusbadalotti/ytfast";
/// How long the corner card counts down before reopening into an update.
#[cfg(target_os = "macos")]
const UPDATE_RESTART_IN: Duration = Duration::from_secs(4);
/// How long the corner card says the app was updated.
const UPDATED_NOTICE_FOR: Duration = Duration::from_secs(10);

/// What the corner card says about updates.
pub enum UpdateNotice {
    None,
    /// Downloading and checking a newer release.
    Installing {
        version: String,
    },
    /// In place of the running app; it reopens at `restart_at` unless
    /// something plays, or on Restart now.
    Ready {
        version: String,
        bundle: std::path::PathBuf,
        restart_at: Option<Instant>,
    },
    /// This run is a version newer than the last one.
    Updated {
        version: String,
        until: Instant,
    },
}

/// Home's own sections, ordered and hidden by title like YouTube's.
pub const HOME_PLAYLISTS: &str = "Your playlists";
pub const HOME_SETS: &str = "Sets from your artists";
pub const HOME_NEW: &str = "New from your artists";
pub const HOME_LONG: &str = "Your long listens";

pub enum HomeSection<'a> {
    Feed(&'a Shelf),
    Playlists,
    Sets,
    New,
    Long,
}

impl HomeSection<'_> {
    pub fn title(&self) -> &str {
        match self {
            HomeSection::Feed(shelf) => &shelf.title,
            HomeSection::Playlists => HOME_PLAYLISTS,
            HomeSection::Sets => HOME_SETS,
            HomeSection::New => HOME_NEW,
            HomeSection::Long => HOME_LONG,
        }
    }
}
/// Shelf titles whose order is remembered; YouTube rotates some daily.
const HOME_ORDER_KEPT: usize = 60;
/// YouTube's section titles remembered as already offered.
const HOME_KNOWN_KEPT: usize = 200;

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
    ToggleNowPlaying,
    /// From the update card.
    RestartToUpdate,
    DismissUpdate,
    OpenReleaseNotes(String),
    /// Mutes, or brings back the volume from before the mute.
    ToggleMute,
    ToggleSide(Side),
    /// A menu shows this track's rating: find it out if unknown.
    WantRating(String),
    /// The pointer is on a playable item this frame.
    Hover(String),
    /// A playable item was just pressed, likely the first click of a double
    /// click: resolve it now.
    Warm(String),
    ForgetSearch(String),
    ClearSearches,
    AddHomeArtist(HomeArtist),
    RemoveHomeArtist(String),
    /// Looks artists up for the Home artists picker.
    FindArtists(String),
    /// The artist whose sets Home shows.
    PickSetsArtist(String),
    /// Puts a link on the clipboard.
    CopyLink(String),
    ShowShelf(String, bool),
    MoveShelf {
        title: String,
        up: bool,
    },
    ResetHome,
    RetryLyrics,
    LoadMoreHome,
    LoadMoreFeed(Browse),
    Retry(Page),
    SignIn,
    SignOut,
    SetBrowser(String),
    SetAutoplay(bool),
    SetCrossfade(f32),
    SetSecondOutput(Option<String>),
    Pin(String),
    Unpin(String),
    AddToPlaylist {
        playlist: String,
        title: String,
        video: String,
    },
    /// Asks before deleting: YouTube has no undo.
    AskDelete(Item),
    ConfirmDelete,
    CancelDelete,
    SetEqEnabled(bool),
    SetEqPreset(EqPreset),
    SetEqBand(usize, f32),
}

pub struct App {
    pub backend: Backend,
    pub player: Player,
    controls: NowPlaying,
    pub settings: Settings,
    dirty: bool,
    saved_at: Instant,
    /// What the corner card says about updates.
    pub update: UpdateNotice,

    pub page: Page,
    back: Vec<Page>,
    forward: Vec<Page>,
    pub home: Loadable<Feed>,
    pub home_more: bool,
    pub collections: HashMap<String, Loadable<Collection>>,
    pub artists: HashMap<String, Loadable<ArtistPage>>,
    /// Sets by artist id, for Home's sets section.
    pub artist_sets: HashMap<String, Loadable<Vec<Item>>>,
    pub sets_artist: Option<String>,
    /// The playing track fills the window.
    pub now_playing: bool,
    /// Home's Long listens, asked for once the section shows.
    pub long_listens: Loadable<Vec<Item>>,
    /// The menu bar asked for the search field.
    pub focus_search: bool,
    /// The volume to go back to when Mute is picked again.
    unmute_to: f32,
    /// What the Home artists picker last looked up.
    pub artist_query: String,
    pub feeds: HashMap<Browse, Loadable<Feed>>,
    pub feeds_more: Option<Browse>,
    pub library: HashMap<&'static str, Loadable<Vec<Item>>>,
    pub search_text: String,
    pub search_query: String,
    pub search_filter: SearchFilter,
    pub searches: HashMap<(String, SearchFilter), Loadable<SearchResults>>,

    pub signed_in: bool,
    /// The stored session has been read (or found missing).
    pub session_ready: bool,
    pub account: Option<Account>,
    pub signing_in: bool,
    /// The session was read from the browser again this run, after YouTube
    /// stopped taking the stored one; it is not tried twice.
    session_refreshed: bool,
    /// A rating YouTube refused for a lost session, sent again once the
    /// session is back.
    retry_rating: Option<(String, Rating)>,
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
    /// The track whose current play already went to the YouTube history.
    history_for: Option<String>,
    /// How the signed-in person rated tracks, as far as the app has asked.
    pub ratings: HashMap<String, Rating>,
    rating_requests: HashSet<String>,
    /// The playable item under the pointer, since when, and whether it was
    /// sent to be resolved ahead of a click.
    hovered: Option<(String, Instant, bool)>,
    hover_seen: bool,
    pub lyrics: HashMap<String, Loadable<Option<Lyrics>>>,
    pub side: Option<Side>,
    art_file: Option<(String, std::path::PathBuf)>,
    pub toasts: Vec<(String, Instant)>,
    /// Output devices as (unique ID, name), read when Settings opens.
    pub devices: Vec<(String, String)>,
    /// Library playlists the signed-in person owns, by browse id.
    pub own_playlists: HashSet<String>,
    /// The playlist waiting for a confirmed delete.
    pub deleting: Option<Item>,
    pub actions: Vec<Action>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Paths) -> Self {
        let ctx = cc.egui_ctx.clone();
        crate::theme::install(&ctx);
        // Built once, before the first frame; a pick wakes the loop so it is
        // not held until the next repaint.
        #[cfg(target_os = "macos")]
        {
            crate::mac_menu::init();
            let waker = ctx.clone();
            crate::mac_menu::set_waker(move || waker.request_repaint());
        }
        let mut settings = Settings::load(&paths.state);
        // A run on a version other than the last one tells what changed.
        let version = env!("CARGO_PKG_VERSION");
        let version_changed = settings.last_version != version;
        let update = if version_changed && !settings.last_version.is_empty() {
            UpdateNotice::Updated {
                version: version.to_string(),
                until: Instant::now() + UPDATED_NOTICE_FOR,
            }
        } else {
            UpdateNotice::None
        };
        settings.last_version = version.to_string();
        let backend = match Backend::new(ctx.clone(), paths) {
            Ok(backend) => backend,
            Err(error) => panic!("could not start the network runtime: {error}"),
        };
        ctx.add_bytes_loader(Arc::new(images::Loader::new(
            backend.http.clone(),
            backend.handle(),
        )));
        #[cfg(target_os = "macos")]
        {
            let http = backend.http.clone();
            backend
                .run(async move { Event::UpdateFound(crate::update::newer_release(&http).await) });
        }
        let emit = backend.emitter();
        let fetcher = player::Fetcher {
            http: backend.http.clone(),
            resolver: Arc::new(Resolver::new(
                backend.ytdlp.clone(),
                backend.paths.cache.clone(),
            )),
            api: backend.api.clone(),
        };
        let player = Player::start(
            backend.handle(),
            fetcher,
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

        let unmute_to = settings.volume;
        let app = Self {
            signed_in: false,
            session_ready: false,
            backend,
            player,
            controls,
            settings,
            dirty: version_changed,
            saved_at: Instant::now(),
            update,
            page: Page::Home,
            back: Vec::new(),
            forward: Vec::new(),
            home: Loadable::NotLoaded,
            home_more: false,
            collections: HashMap::new(),
            artists: HashMap::new(),
            artist_sets: HashMap::new(),
            sets_artist: None,
            now_playing: false,
            long_listens: Loadable::NotLoaded,
            focus_search: false,
            unmute_to,
            artist_query: String::new(),
            feeds: HashMap::new(),
            feeds_more: None,
            library: HashMap::new(),
            search_text: String::new(),
            search_query: String::new(),
            search_filter: SearchFilter::All,
            searches: HashMap::new(),
            account: None,
            signing_in: false,
            session_refreshed: false,
            retry_rating: None,
            sign_in_error: None,
            playing: false,
            loading: false,
            ended: false,
            skips: 0,
            next_sent: None,
            queue_source: None,
            pending_play: None,
            autoplay_for: None,
            history_for: None,
            ratings: HashMap::new(),
            rating_requests: HashSet::new(),
            hovered: None,
            hover_seen: false,
            lyrics: HashMap::new(),
            side: None,
            art_file: None,
            toasts: Vec::new(),
            devices: Vec::new(),
            own_playlists: HashSet::new(),
            deleting: None,
            actions: Vec::new(),
        };
        if let Some(device) = app.settings.second_output.clone() {
            app.player.send(Command::SecondOutput(Some(device)));
        }
        app.backend.load_session();
        let (http, ytdlp) = (app.backend.http.clone(), app.backend.ytdlp.clone());
        app.backend
            .run(async move { Event::Ytdlp(ytdlp.ensure(&http).await) });
        app
    }

    /// What starts once it is known whether there is a session: the home
    /// feed and library for that account, and the track the last run left
    /// paused, resolved with the session for the best quality.
    fn session_loaded(&mut self, session: Option<Session>) {
        self.session_ready = true;
        self.signed_in = session.is_some();
        self.backend.api.set_session(session);
        self.ensure_loaded(&self.page.clone());
        if self.signed_in {
            self.load_account();
        }
        if let Some(track) = self.current_ref() {
            let from = self.settings.position;
            self.player.send(Command::Play {
                track,
                from,
                paused: true,
            });
            self.track_changed();
        }
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

    fn set_volume(&mut self, volume: f32) {
        self.settings.volume = volume.clamp(0.0, 1.0);
        self.player.send(Command::Volume(self.settings.volume));
        self.dirty = true;
    }

    /// Reads the session from the browser again when YouTube stops taking the
    /// stored one, once a run: the browser keeps its own fresh. False when it
    /// was already tried.
    fn refresh_session(&mut self) -> bool {
        if self.session_refreshed || !self.signed_in {
            return false;
        }
        self.session_refreshed = true;
        log::info!(
            "the YouTube session stopped working; reading it from {} again",
            self.settings.browser
        );
        self.signing_in = true;
        self.sign_in_error = None;
        self.backend.sign_in(self.settings.browser.clone());
        true
    }

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
        self.search_for(self.search_query.clone(), self.search_filter);
    }

    fn search_for(&mut self, query: String, filter: SearchFilter) {
        let key = (query, filter);
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
        if *page == Page::Home {
            self.load_home_extras();
        }
        if *page == Page::Settings {
            self.load_whole_home();
        }
        match page {
            // The feed is the account's: it waits for the stored session.
            Page::Home if self.home.needs_load() && self.session_ready => self.load_home(false),
            Page::Album(id) if !self.collections.contains_key(id) => self.load_collection(id, true),
            Page::Playlist(id) if !self.collections.contains_key(id) => {
                self.load_collection(id, false)
            }
            Page::Artist(id) if !self.artists.contains_key(id) => self.load_artist(id),
            Page::Browse(target) if !self.feeds.contains_key(target) => {
                self.load_feed(target.clone(), false)
            }
            Page::Search => self.load_search(),
            #[cfg(target_os = "macos")]
            Page::Settings => {
                self.backend.run(async {
                    let devices =
                        tokio::task::spawn_blocking(crate::mac_output::output_devices).await;
                    Event::Devices(devices.unwrap_or_default())
                });
            }
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

    /// Finds which of the library's playlists are the account's own, four
    /// at a time: only those can be added to or deleted.
    fn check_ownership(&mut self, items: &[Item]) {
        let candidates: Vec<String> = items
            .iter()
            .filter(|item| item.kind == Kind::Playlist && item.id.starts_with("VLPL"))
            .map(|item| item.id.clone())
            .collect();
        let api = self.backend.api.clone();
        self.backend.run(async move {
            use futures_util::StreamExt;
            let owned = futures_util::stream::iter(candidates)
                .map(|id| {
                    let api = api.clone();
                    async move { api.owns_playlist(&id).await.unwrap_or(false).then_some(id) }
                })
                .buffer_unordered(OWNERSHIP_CHECKS_AT_ONCE)
                .filter_map(|owned| async move { owned })
                .collect()
                .await;
            Event::OwnPlaylists(owned)
        });
    }

    pub fn rating_of(&self, id: &str) -> Option<Rating> {
        self.ratings.get(id).copied()
    }

    /// Asks how a track is rated, once at a time, when signed in.
    fn request_rating(&mut self, id: String) {
        if !self.signed_in || !self.rating_requests.insert(id.clone()) {
            return;
        }
        let api = self.backend.api.clone();
        self.backend.run(async move {
            Event::Rating {
                result: api.rating(&id).await,
                id,
            }
        });
    }

    fn want_lyrics(&mut self) {
        let Some(track) = self.current().cloned() else {
            return;
        };
        // Asked for every track, so the lyrics button knows when there are none.
        if self.lyrics.contains_key(&track.id) {
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
        self.history_for = None;
        self.sync_next();
        let Some(track) = self.current().cloned() else {
            return;
        };
        // Asked again each time it plays: it may have changed elsewhere.
        self.rating_requests.remove(&track.id);
        self.request_rating(track.id.clone());
        if let Some(url) = images::art_url(&track.thumbnails, NOW_PLAYING_ART_PX) {
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

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Open(page) => self.open(page),
            Action::OpenItem(item) => self.open_item(item),
            Action::Back => {
                self.now_playing = false;
                if let Some(page) = self.back.pop() {
                    self.forward
                        .push(std::mem::replace(&mut self.page, page.clone()));
                    self.ensure_loaded(&page);
                }
            }
            Action::Forward => {
                self.now_playing = false;
                if let Some(page) = self.forward.pop() {
                    self.back
                        .push(std::mem::replace(&mut self.page, page.clone()));
                    self.ensure_loaded(&page);
                }
            }
            Action::Search(query) => {
                self.search_query = query.trim().to_string();
                self.search_text = self.search_query.clone();
                self.remember_search();
                self.open(Page::Search);
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
            Action::Volume(volume) => self.set_volume(volume),
            Action::ToggleMute => {
                let volume = self.settings.volume;
                if volume > 0.0 {
                    self.unmute_to = volume;
                    self.set_volume(0.0);
                } else {
                    self.set_volume(self.unmute_to.max(MIN_UNMUTED));
                }
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
                self.ratings.insert(id.clone(), rating);
                let api = self.backend.api.clone();
                self.backend.run(async move {
                    Event::Rated {
                        result: api.rate(&id, rating).await,
                        id,
                    }
                });
            }
            Action::ToggleNowPlaying => self.now_playing = !self.now_playing,
            Action::RestartToUpdate => self.restart_to_update(),
            // A dismissed update still opens the next time.
            Action::DismissUpdate => self.update = UpdateNotice::None,
            Action::OpenReleaseNotes(version) => {
                ctx.open_url(egui::OpenUrl::new_tab(format!(
                    "{REPO_URL}/releases/tag/v{version}"
                )));
                self.update = UpdateNotice::None;
            }
            Action::ToggleSide(side) => {
                // From the full view the panel opens in the normal layout.
                self.side = if self.side == Some(side) && !self.now_playing {
                    None
                } else {
                    Some(side)
                };
                self.now_playing = false;
                self.want_lyrics();
            }
            Action::RetryLyrics => {
                if let Some(id) = self.current().map(|t| t.id.clone()) {
                    self.lyrics.remove(&id);
                }
                self.want_lyrics();
            }
            Action::WantRating(id) => {
                if !self.ratings.contains_key(&id) {
                    self.request_rating(id);
                }
            }
            Action::Hover(id) => {
                self.hover_seen = true;
                if self
                    .hovered
                    .as_ref()
                    .is_none_or(|(hovered, _, _)| *hovered != id)
                {
                    self.hovered = Some((id, Instant::now(), false));
                }
            }
            Action::Warm(id) => self.player.send(Command::Warm(id)),
            Action::ForgetSearch(query) => {
                self.settings.recent_searches.retain(|q| *q != query);
                self.dirty = true;
            }
            Action::ClearSearches => {
                self.settings.recent_searches.clear();
                self.dirty = true;
            }
            Action::CopyLink(link) => {
                ctx.copy_text(link);
                self.toast("Link copied");
            }
            Action::AddHomeArtist(artist) => {
                if !self.settings.home_artists.iter().any(|a| a.id == artist.id) {
                    self.toast(format!("{} is on Home now", artist.name));
                    self.settings.home_artists.push(artist);
                    self.dirty = true;
                    // Picked artists feed Long listens too: look again.
                    self.long_listens = Loadable::NotLoaded;
                    self.load_home_extras();
                }
            }
            Action::RemoveHomeArtist(id) => {
                self.settings.home_artists.retain(|a| a.id != id);
                if self.sets_artist.as_ref() == Some(&id) {
                    self.sets_artist = None;
                }
                self.dirty = true;
                self.long_listens = Loadable::NotLoaded;
                self.load_home_extras();
            }
            Action::FindArtists(query) => {
                self.artist_query = query.trim().to_string();
                self.search_for(self.artist_query.clone(), SearchFilter::Artists);
            }
            Action::PickSetsArtist(id) => {
                self.sets_artist = Some(id);
                self.load_home_extras();
            }
            Action::ShowShelf(title, shown) => {
                self.know_sections();
                let hidden = &mut self.settings.home_hidden;
                hidden.retain(|t| *t != title);
                if !shown {
                    hidden.push(title);
                }
                self.dirty = true;
                self.load_home_extras();
            }
            Action::MoveShelf { title, up } => {
                self.know_sections();
                self.move_shelf(&title, up);
            }
            Action::ResetHome => {
                self.settings.home_order.clear();
                self.settings.home_hidden.clear();
                self.settings.home_known.clear();
                self.dirty = true;
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
                self.ratings.clear();
                self.rating_requests.clear();
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
            Action::Pin(id) => {
                if !self.settings.pinned.contains(&id) {
                    self.settings.pinned.push(id);
                    self.dirty = true;
                }
            }
            Action::Unpin(id) => {
                self.settings.pinned.retain(|p| *p != id);
                self.dirty = true;
            }
            Action::AddToPlaylist {
                playlist,
                title,
                video,
            } => {
                self.toast(format!("Adding to “{title}”…"));
                let api = self.backend.api.clone();
                self.backend.run(async move {
                    let result = api.add_to_playlist(&playlist, &video).await;
                    Event::AddedToPlaylist {
                        playlist,
                        title,
                        result,
                    }
                });
            }
            Action::AskDelete(item) => self.deleting = Some(item),
            Action::CancelDelete => self.deleting = None,
            Action::ConfirmDelete => {
                if let Some(item) = self.deleting.take() {
                    let api = self.backend.api.clone();
                    self.backend.run(async move {
                        let result = api.delete_playlist(&item.id).await;
                        Event::PlaylistDeleted {
                            id: item.id,
                            title: item.title,
                            result,
                        }
                    });
                }
            }
            Action::SetSecondOutput(device) => {
                self.settings.second_output = device.clone();
                self.player.send(Command::SecondOutput(device));
                self.dirty = true;
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
        self.now_playing = false;
        if page != self.page {
            self.back
                .push(std::mem::replace(&mut self.page, page.clone()));
            self.forward.clear();
        }
        self.ensure_loaded(&page);
        if let Page::Album(id) | Page::Playlist(id) = &page
            && let Some(collection) = self.collections.get(id).and_then(Loadable::get)
        {
            self.warm_top(&collection.tracks);
        }
    }

    /// Home's sections in the order set for them, hidden ones included: the
    /// app's own first, then YouTube's; the ones never ordered keep that
    /// order, after the rest.
    pub fn home_sections(&self) -> Vec<HomeSection<'_>> {
        let feed = self
            .home
            .get()
            .map(|f| f.shelves.as_slice())
            .unwrap_or_default();
        let mut sections: Vec<HomeSection> = [
            HomeSection::Playlists,
            HomeSection::Sets,
            HomeSection::New,
            HomeSection::Long,
        ]
        .into_iter()
        .chain(feed.iter().map(HomeSection::Feed))
        .collect();
        let order = &self.settings.home_order;
        sections.sort_by_key(|s| {
            order
                .iter()
                .position(|t| t == s.title())
                .unwrap_or(usize::MAX)
        });
        sections
    }

    /// A pick from the macOS menu bar, as the action it stands for.
    #[cfg(target_os = "macos")]
    fn menu_command(&mut self, command: crate::mac_menu::MenuCommand, ctx: &egui::Context) {
        use crate::mac_menu::MenuCommand;
        let volume = self.settings.volume;
        let action = match command {
            MenuCommand::PlayPause => Action::TogglePlay,
            MenuCommand::Next => Action::Next,
            MenuCommand::Previous => Action::Previous,
            MenuCommand::SeekForward => {
                let end = self.length().unwrap_or(f64::MAX);
                Action::Seek((self.position() + MENU_SEEK).min(end))
            }
            MenuCommand::SeekBackward => Action::Seek((self.position() - MENU_SEEK).max(0.0)),
            MenuCommand::ToggleShuffle => Action::ToggleShuffle,
            MenuCommand::CycleRepeat => Action::CycleRepeat,
            MenuCommand::Like => {
                let Some(track) = self.current().filter(|_| self.signed_in) else {
                    return;
                };
                let next = if self.rating_of(&track.id) == Some(Rating::Like) {
                    Rating::None
                } else {
                    Rating::Like
                };
                Action::Rate(track.id.clone(), next)
            }
            MenuCommand::VolumeUp => Action::Volume(volume + MENU_VOLUME_STEP),
            MenuCommand::VolumeDown => Action::Volume(volume - MENU_VOLUME_STEP),
            MenuCommand::ToggleMute => Action::ToggleMute,
            MenuCommand::Back => Action::Back,
            MenuCommand::Forward => Action::Forward,
            MenuCommand::Home => Action::Open(Page::Home),
            MenuCommand::Library => Action::Open(Page::Library),
            MenuCommand::LikedMusic => Action::Open(Page::Playlist(LIKED_MUSIC.to_string())),
            MenuCommand::Settings => Action::Open(Page::Settings),
            MenuCommand::Queue => Action::ToggleSide(Side::Queue),
            MenuCommand::Lyrics => Action::ToggleSide(Side::Lyrics),
            MenuCommand::Search => {
                self.focus_search = true;
                return;
            }
            MenuCommand::OpenRepo => {
                ctx.open_url(egui::OpenUrl::new_tab(REPO_URL));
                return;
            }
            // Editing goes through egui, which owns the text field and the
            // clipboard.
            MenuCommand::Cut => return ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut),
            MenuCommand::Copy => return ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy),
            MenuCommand::Paste => {
                return ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            MenuCommand::SelectAll => {
                ctx.input_mut(|input| {
                    input.events.push(egui::Event::Key {
                        key: egui::Key::A,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::COMMAND,
                    });
                });
                return;
            }
        };
        self.actions.push(action);
    }

    /// Once Home has been arranged, a section YouTube had not sent before
    /// starts hidden, for the person to switch on in Settings. Before that,
    /// Home is YouTube's as it comes.
    fn hide_new_sections(&mut self) {
        let settings = &self.settings;
        if settings.home_order.is_empty()
            && settings.home_hidden.is_empty()
            && settings.home_known.is_empty()
        {
            return;
        }
        let Some(feed) = self.home.get() else {
            return;
        };
        let new: Vec<String> = feed
            .shelves
            .iter()
            .map(|s| &s.title)
            .filter(|t| {
                ![
                    &settings.home_known,
                    &settings.home_order,
                    &settings.home_hidden,
                ]
                .iter()
                .any(|list| list.contains(t))
            })
            .cloned()
            .collect();
        if new.is_empty() {
            return;
        }
        log::info!("new on Home, hidden until switched on: {}", new.join(", "));
        self.settings.home_hidden.extend(new.iter().cloned());
        self.remember_known(new);
    }

    /// Takes the sections loaded now as offered, the moment Home is first
    /// arranged, so only later ones count as new.
    fn know_sections(&mut self) {
        let titles: Vec<String> = self
            .home
            .get()
            .map(|f| f.shelves.iter().map(|s| s.title.clone()).collect())
            .unwrap_or_default();
        let known = &self.settings.home_known;
        let unknown: Vec<String> = titles.into_iter().filter(|t| !known.contains(t)).collect();
        self.remember_known(unknown);
    }

    fn remember_known(&mut self, titles: Vec<String>) {
        if titles.is_empty() {
            return;
        }
        let known = &mut self.settings.home_known;
        known.extend(titles);
        let over = known.len().saturating_sub(HOME_KNOWN_KEPT);
        known.drain(..over);
        self.dirty = true;
    }

    /// Every page of Home's feed, one after the other, while Settings lists
    /// its sections to arrange.
    fn load_whole_home(&mut self) {
        if self.page != Page::Settings || !self.session_ready || self.home_more {
            return;
        }
        if self.home.needs_load() {
            self.load_home(false);
        } else if self.home.get().is_some_and(|f| f.continuation.is_some()) {
            self.load_home(true);
        }
    }

    /// What Home's own sections need: each picked artist's page for the
    /// new releases, and the chosen artist's sets.
    fn load_home_extras(&mut self) {
        if self.shows_shelf(HOME_NEW) {
            let ids: Vec<String> = self
                .settings
                .home_artists
                .iter()
                .map(|a| a.id.clone())
                .collect();
            for id in ids {
                if !self.artists.contains_key(&id) {
                    self.load_artist(&id);
                }
            }
        }
        if self.shows_shelf(HOME_LONG) && self.signed_in && self.long_listens.needs_load() {
            self.long_listens = Loadable::Loading;
            let api = self.backend.api.clone();
            let picked: Vec<String> = self
                .settings
                .home_artists
                .iter()
                .map(|a| a.name.clone())
                .collect();
            self.backend
                .run(async move { Event::LongListens(api.long_listens(&picked).await) });
        }
        if self.shows_shelf(HOME_SETS)
            && let Some(artist) = self.sets_artist_now().cloned()
            && !self.artist_sets.contains_key(&artist.id)
        {
            self.artist_sets
                .insert(artist.id.clone(), Loadable::Loading);
            let api = self.backend.api.clone();
            self.backend.run(async move {
                Event::ArtistSets {
                    result: api.artist_sets(&artist.name).await,
                    id: artist.id,
                }
            });
        }
    }

    /// The artist whose sets show: the one picked, else the first.
    pub fn sets_artist_now(&self) -> Option<&HomeArtist> {
        let artists = &self.settings.home_artists;
        artists
            .iter()
            .find(|a| Some(&a.id) == self.sets_artist.as_ref())
            .or(artists.first())
    }

    /// Drops a collection's page, which the server has changed, and loads it
    /// again at once when it is the one on screen; otherwise when it opens.
    fn reload_collection(&mut self, id: &str) {
        self.collections.remove(id);
        if matches!(&self.page, Page::Album(shown) | Page::Playlist(shown) if shown == id) {
            self.ensure_loaded(&self.page.clone());
        }
    }

    fn load_artist(&mut self, id: &str) {
        self.artists.insert(id.to_string(), Loadable::Loading);
        let (api, id) = (self.backend.api.clone(), id.to_string());
        self.backend.run(async move {
            Event::Artist {
                result: api.artist(&id).await,
                id,
            }
        });
    }

    pub fn shows_shelf(&self, title: &str) -> bool {
        !self.settings.home_hidden.iter().any(|t| t == title)
    }

    /// Swaps a shelf with its neighbour. The whole order shown is kept, and
    /// after it the shelves not on Home today, which come back in place.
    fn move_shelf(&mut self, title: &str, up: bool) {
        let mut titles: Vec<String> = Vec::new();
        for section in self.home_sections() {
            if !titles.iter().any(|t| t == section.title()) {
                titles.push(section.title().to_string());
            }
        }
        let Some(at) = titles.iter().position(|t| t == title) else {
            return;
        };
        let to = if up { at.checked_sub(1) } else { Some(at + 1) };
        let Some(to) = to.filter(|&to| to < titles.len()) else {
            return;
        };
        titles.swap(at, to);
        let absent: Vec<String> = self
            .settings
            .home_order
            .iter()
            .filter(|t| !titles.contains(t))
            .cloned()
            .collect();
        titles.extend(absent);
        titles.truncate(HOME_ORDER_KEPT);
        self.settings.home_order = titles;
        self.dirty = true;
    }

    /// Puts the search just run at the top of the recent ones, once.
    fn remember_search(&mut self) {
        let query = &self.search_query;
        if query.is_empty() {
            return;
        }
        let recent = &mut self.settings.recent_searches;
        recent.retain(|q| q.to_lowercase() != query.to_lowercase());
        recent.insert(0, query.clone());
        recent.truncate(RECENT_SEARCHES);
        self.dirty = true;
    }

    /// Resolves the first tracks of a list just opened, the likeliest played.
    fn warm_top(&self, tracks: &[Item]) {
        let top: Vec<&Item> = tracks
            .iter()
            .filter(|t| t.is_playable())
            .take(WARM_TOP)
            .collect();
        // The newest ask is resolved first: sent last to first, the first
        // track is ready soonest.
        for track in top.into_iter().rev() {
            self.player.send(Command::Warm(track.id.clone()));
        }
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
        // With autoplay on, playing just this track has already asked for
        // its radio.
        if self.autoplay_for.as_ref() == Some(&seed) {
            return;
        }
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
        if shuffle {
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
                self.hide_new_sections();
                self.load_whole_home();
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
                    } else if matches!(&self.page, Page::Album(p) | Page::Playlist(p) if *p == id) {
                        self.warm_top(&collection.tracks);
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
                        let next = next.filter(|_| !fresh.is_empty());
                        collection.continuation = next.clone();
                        if self.queue_source.as_ref() == Some(&id) {
                            self.settings.queue.add(fresh);
                            if self.settings.queue.shuffle {
                                self.settings.queue.set_shuffle(true);
                            }
                            self.queue_edited();
                        }
                        if let Some(token) = next {
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
            Event::ArtistSets { id, result } => {
                self.artist_sets.insert(id, Loadable::from_result(result));
            }
            Event::LongListens(result) => {
                match &result {
                    Ok(items) => log::info!("long listens: {} found", items.len()),
                    Err(error) => log::warn!("long listens: {error:#}"),
                }
                self.long_listens = Loadable::from_result(result);
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
                if let Err(error) = &result
                    && signed_out(error)
                {
                    self.refresh_session();
                }
                if browse_id == LIBRARY_PLAYLISTS
                    && let Ok(items) = &result
                {
                    self.check_ownership(items);
                }
                self.library
                    .insert(browse_id, Loadable::from_result(result));
            }
            Event::OwnPlaylists(owned) => {
                log::info!(
                    "{} of the library's playlists are the account's own",
                    owned.len()
                );
                self.own_playlists = owned;
            }
            Event::AddedToPlaylist {
                playlist,
                title,
                result,
            } => match result {
                Ok(()) => {
                    self.toast(format!("Added to “{title}”"));
                    // Its page shows the new track.
                    self.reload_collection(&playlist_browse_id(&playlist));
                }
                Err(error) => self.toast(format!("Could not add to “{title}”: {error:#}")),
            },
            Event::PlaylistDeleted { id, title, result } => match result {
                Ok(()) => {
                    self.toast(format!("Deleted “{title}”"));
                    if let Some(items) = self
                        .library
                        .get_mut(LIBRARY_PLAYLISTS)
                        .and_then(Loadable::get_mut)
                    {
                        items.retain(|item| item.id != id);
                    }
                    self.own_playlists.remove(&id);
                    self.settings.pinned.retain(|p| *p != id);
                    self.collections.remove(&id);
                    if self.page == Page::Playlist(id) {
                        self.open(Page::Library);
                    }
                    self.dirty = true;
                }
                Err(error) => self.toast(format!("Could not delete “{title}”: {error:#}")),
            },
            Event::Queue { fill, result } => self.queue_filled(fill, result),
            Event::Rating { id, result } => {
                self.rating_requests.remove(&id);
                match result {
                    Ok(rating) => _ = self.ratings.insert(id, rating),
                    Err(error) => log::warn!("rating: {error:#}"),
                }
            }
            Event::Rated { id, result } => {
                if let Err(error) = result {
                    if signed_out(&error) && self.refresh_session() {
                        if let Some(rating) = self.ratings.get(&id) {
                            self.retry_rating = Some((id, *rating));
                        }
                        return;
                    }
                    self.toast(format!("Could not save that rating: {error:#}"));
                    self.rating_requests.remove(&id);
                    self.request_rating(id);
                } else {
                    // Liked songs is a playlist the server rebuilds: load it again.
                    self.reload_collection(LIKED_MUSIC);
                }
            }
            Event::Lyrics { id, result } => {
                self.lyrics.insert(id, Loadable::from_result(result));
            }
            Event::Account(result) => match result {
                Ok(Some(account)) => self.account = Some(account),
                Ok(None) => {
                    self.account = None;
                    if !self.refresh_session() {
                        self.toast("YouTube no longer accepts the saved session. Sign in again in Settings.");
                    }
                }
                Err(error) => {
                    log::warn!("account: {error:#}");
                    if signed_out(&error) {
                        self.refresh_session();
                    }
                }
            },
            Event::SignedIn(result) => {
                self.signing_in = false;
                match result {
                    Ok(session) => {
                        self.backend.api.set_session(Some(session));
                        self.signed_in = true;
                        self.load_account();
                        self.home = Loadable::NotLoaded;
                        self.ensure_loaded(&Page::Home);
                        if let Some((id, rating)) = self.retry_rating.take() {
                            self.actions.push(Action::Rate(id, rating));
                        }
                    }
                    Err(error) => {
                        if self.retry_rating.take().is_some() {
                            self.toast("YouTube no longer accepts the saved session. Sign in again in Settings.");
                        }
                        self.sign_in_error = Some(format!("{error:#}"));
                    }
                }
            }
            Event::Art { id, path } => self.art_file = Some((id, path)),
            Event::Ytdlp(result) => {
                if let Err(error) = result {
                    log::warn!("yt-dlp update: {error:#}");
                }
            }
            Event::History { id, result } => match result {
                Ok(()) => log::info!("{id}: added to the YouTube history"),
                Err(error) => log::warn!("{id}: not added to the YouTube history: {error:#}"),
            },
            Event::SessionLoaded(session) => self.session_loaded(session),
            Event::Devices(devices) => self.devices = devices,
            Event::Player(event) => self.player_event(event),
            #[cfg(target_os = "macos")]
            Event::UpdateFound(result) => self.update_found(result),
            #[cfg(target_os = "macos")]
            Event::UpdateInstalled { version, result } => self.update_installed(version, result),
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

    /// A newer release was found: install it, saying so in the corner.
    #[cfg(target_os = "macos")]
    fn update_found(&mut self, result: anyhow::Result<Option<crate::update::Release>>) {
        let release = match result {
            Ok(Some(release)) => release,
            Ok(None) => return,
            Err(error) => return log::warn!("update: {error:#}"),
        };
        log::info!("updating to {}", release.version);
        self.update = UpdateNotice::Installing {
            version: release.version.clone(),
        };
        let http = self.backend.http.clone();
        self.backend.run(async move {
            Event::UpdateInstalled {
                result: crate::update::install_release(&http, &release).await,
                version: release.version,
            }
        });
    }

    /// The new version is in place: reopen into it shortly, unless something
    /// plays, then it opens the next time.
    #[cfg(target_os = "macos")]
    fn update_installed(&mut self, version: String, result: anyhow::Result<std::path::PathBuf>) {
        match result {
            Ok(bundle) => {
                log::info!("updated to {version}");
                let restart_at = (!self.playing).then(|| Instant::now() + UPDATE_RESTART_IN);
                self.update = UpdateNotice::Ready {
                    version,
                    bundle,
                    restart_at,
                };
            }
            Err(error) => {
                log::warn!("update to {version}: {error:#}");
                self.update = UpdateNotice::None;
                self.toast(format!("Could not update to ytfast {version}"));
            }
        }
    }

    /// Runs the corner card's clock: the countdown to reopening, which music
    /// starting holds off, and the "updated" note that goes by itself.
    fn tick_update(&mut self) {
        let now = Instant::now();
        let due = matches!(
            &self.update,
            UpdateNotice::Ready { restart_at: Some(at), .. } if now >= *at
        );
        if due && self.playing {
            if let UpdateNotice::Ready { restart_at, .. } = &mut self.update {
                *restart_at = None;
            }
        } else if due {
            self.restart_to_update();
        }
        if matches!(&self.update, UpdateNotice::Updated { until, .. } if now >= *until) {
            self.update = UpdateNotice::None;
        }
    }

    /// Quits and opens the version now in place.
    fn restart_to_update(&mut self) {
        #[cfg(target_os = "macos")]
        {
            let bundle = match &self.update {
                UpdateNotice::Ready { bundle, .. } => bundle.clone(),
                _ => return,
            };
            self.save();
            match crate::update::relaunch(&bundle) {
                Ok(()) => std::process::exit(0),
                Err(error) => log::warn!("update: {error:#}"),
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

    /// A track the pointer rests on is resolved ahead, so a double click
    /// starts it in a fraction of a second.
    fn warm_hovered(&mut self, ctx: &egui::Context) {
        if !self.hover_seen {
            self.hovered = None;
            return;
        }
        let Some((id, since, sent)) = &mut self.hovered else {
            return;
        };
        if *sent {
            return;
        }
        let waited = since.elapsed();
        if waited < WARM_AFTER {
            ctx.request_repaint_after(WARM_AFTER - waited);
            return;
        }
        *sent = true;
        self.player.send(Command::Warm(id.clone()));
    }

    /// Sends the current play to the YouTube history once it has really
    /// been listened to.
    fn report_history(&mut self) {
        if !self.signed_in || !self.playing || self.loading || self.position() < HISTORY_AFTER {
            return;
        }
        let Some(id) = self.current().map(|t| t.id.clone()) else {
            return;
        };
        if self.history_for.as_ref() == Some(&id) {
            return;
        }
        self.history_for = Some(id.clone());
        let api = self.backend.api.clone();
        self.backend.run(async move {
            Event::History {
                result: api.report_play(&id).await,
                id,
            }
        });
    }

    // -----------------------------------------------------------------------
    // Desktop media controls

    fn sync_controls(&mut self, ctx: &egui::Context) {
        for command in self.controls.commands() {
            let action = match command {
                now_playing::Command::Play if !self.playing => Action::TogglePlay,
                now_playing::Command::Pause | now_playing::Command::Stop if self.playing => {
                    Action::TogglePlay
                }
                now_playing::Command::PlayPause => Action::TogglePlay,
                now_playing::Command::Next => Action::Next,
                now_playing::Command::Previous => Action::Previous,
                now_playing::Command::SeekBy(ms) => {
                    Action::Seek(self.position() + ms as f64 / 1000.0)
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
            art_url: images::art_url(&t.thumbnails, NOW_PLAYING_ART_PX),
            url: Some(t.link()),
            ..now_playing::Track::default()
        });
        let state = now_playing::State {
            playback: match (&track, self.playing) {
                (None, _) => now_playing::Playback::Stopped,
                (_, true) => now_playing::Playback::Playing,
                (_, false) => now_playing::Playback::Paused,
            },
            track,
            position: Duration::from_secs_f64(self.position()),
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
        self.controls.update(state);
    }

    fn save(&mut self) {
        self.settings.position = self.position();
        if let Err(error) = self.settings.save(&self.backend.paths.state) {
            log::warn!("could not save state: {error}");
        }
        self.dirty = false;
        self.saved_at = Instant::now();
    }
}

/// Whether YouTube answered as to someone signed out: the session is lost.
fn signed_out(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("HTTP 401")
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
        self.tick_update();
        #[cfg(target_os = "macos")]
        for command in crate::mac_menu::drain() {
            self.menu_command(command, &ctx);
        }
        self.sync_controls(&ctx);
        self.report_history();
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.actions.push(Action::TogglePlay);
        }

        crate::ui::show(self, ui);

        self.hover_seen = false;
        for action in std::mem::take(&mut self.actions) {
            self.apply(action, &ctx);
        }
        self.warm_hovered(&ctx);
        let now = Instant::now();
        self.toasts.retain(|(_, until)| *until > now);
        if self.dirty && self.saved_at.elapsed() > SAVE_EVERY {
            self.save();
        }
        if self.playing || !self.toasts.is_empty() || self.loading {
            ctx.request_repaint_after(REDRAW_EVERY);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save();
    }
}
