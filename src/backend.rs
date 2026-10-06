//! Bridge between the UI thread and asynchronous work.
//!
//! egui runs on the main thread and must never block. A tokio runtime runs
//! the InnerTube requests, lyrics, sign-in and downloads; every answer comes
//! back as an `Event` and wakes the interface with `request_repaint`, so the
//! app stays idle when nothing happens.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::Result;

use crate::auth::{self, Session};
use crate::innertube::Innertube;
use crate::model::{
    Account, ArtistPage, Browse, Collection, Feed, Item, Lyrics, Rating, SearchFilter,
    SearchResults,
};
use crate::player;
use crate::stream::Ytdlp;

pub struct Paths {
    pub state: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn new() -> Self {
        let dirs = directories::ProjectDirs::from("", "", "ytfast");
        let base = || std::env::temp_dir().join("ytfast");
        Self {
            state: dirs
                .as_ref()
                .map_or_else(base, |d| d.config_dir().to_path_buf())
                .join("state.json"),
            data: dirs
                .as_ref()
                .map_or_else(base, |d| d.data_dir().to_path_buf()),
            cache: dirs
                .as_ref()
                .map_or_else(base, |d| d.cache_dir().to_path_buf()),
        }
    }
}

impl Default for Paths {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a list of tracks fetched for the queue goes.
#[derive(Clone, Debug, PartialEq)]
pub enum Fill {
    /// Replaces the queue, starting at the first track.
    Replace,
    /// Follows the seed in a queue that holds just it (Play on a song).
    After(String),
    /// Autoplay: appended once the queue runs out after this track.
    Autoplay(String),
}

pub enum Event {
    Home {
        more: bool,
        result: Result<Feed>,
    },
    Collection {
        id: String,
        result: Result<Collection>,
    },
    CollectionMore {
        id: String,
        result: Result<(Vec<Item>, Option<String>)>,
    },
    Artist {
        id: String,
        result: Result<ArtistPage>,
    },
    ArtistSets {
        id: String,
        result: Result<Vec<Item>>,
    },
    LongListens(Result<Vec<Item>>),
    Feed {
        target: Browse,
        more: bool,
        result: Result<Feed>,
    },
    Search {
        query: String,
        filter: SearchFilter,
        result: Result<SearchResults>,
    },
    Library {
        browse_id: &'static str,
        result: Result<Vec<Item>>,
    },
    Queue {
        fill: Fill,
        result: Result<Vec<Item>>,
    },
    Rating {
        id: String,
        result: Result<Rating>,
    },
    Rated {
        id: String,
        result: Result<()>,
    },
    Lyrics {
        id: String,
        result: Result<Option<Lyrics>>,
    },
    Account(Result<Option<Account>>),
    SignedIn(Result<Session>),
    /// The stored session, read at startup.
    SessionLoaded(Option<Session>),
    /// Output devices as (unique ID, name).
    Devices(Vec<(String, String)>),
    /// The library's playlists the signed-in person owns, by browse id.
    OwnPlaylists(std::collections::HashSet<String>),
    AddedToPlaylist {
        playlist: String,
        title: String,
        result: Result<()>,
    },
    PlaylistDeleted {
        id: String,
        title: String,
        result: Result<()>,
    },
    Art {
        id: String,
        path: PathBuf,
    },
    Ytdlp(Result<()>),
    /// A play was reported to the account's YouTube history.
    History {
        id: String,
        result: Result<()>,
    },
    Player(player::Event),
}

pub struct Backend {
    runtime: tokio::runtime::Runtime,
    pub http: reqwest::Client,
    pub api: Arc<Innertube>,
    pub ytdlp: Arc<Ytdlp>,
    pub paths: Paths,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    ctx: egui::Context,
}

impl Backend {
    pub fn new(ctx: egui::Context, paths: Paths) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("ytfast-net")
            .build()?;
        let http = reqwest::Client::builder()
            .gzip(true)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(std::io::Error::other)?;
        auth::init_store();
        // The session arrives through `load_session`: the Keychain can take
        // seconds to answer, and the window must not wait for it.
        let api = Arc::new(Innertube::new(http.clone(), None));
        let ytdlp = Arc::new(Ytdlp::new(&paths.data));
        let (tx, rx) = channel();
        Ok(Self {
            runtime,
            http,
            api,
            ytdlp,
            paths,
            tx,
            rx,
            ctx,
        })
    }

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// A sender that also wakes the interface. `run` and `save_art` send
    /// through it, and the player thread gets one of its own.
    pub fn emitter(&self) -> impl Fn(Event) + Send + 'static {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        move |event| {
            let _ = tx.send(event);
            ctx.request_repaint();
        }
    }

    /// Runs `work` on the runtime and delivers its event.
    pub fn run(&self, work: impl Future<Output = Event> + Send + 'static) {
        let emit = self.emitter();
        self.runtime.spawn(async move { emit(work.await) });
    }

    pub fn events(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }

    /// Reads the stored session off the interface thread.
    pub fn load_session(&self) {
        self.run(async {
            Event::SessionLoaded(tokio::task::spawn_blocking(auth::load).await.ok().flatten())
        });
    }

    pub fn sign_in(&self, browser: String) {
        let (ytdlp, scratch) = (self.ytdlp.clone(), self.paths.cache.clone());
        self.run(async move {
            let result = async {
                let session = auth::import(&ytdlp, &browser, &scratch).await?;
                let stored = session.clone();
                tokio::task::spawn_blocking(move || auth::save(&stored)).await??;
                Ok(session)
            }
            .await;
            Event::SignedIn(result)
        });
    }

    /// Keeps the cover of the playing track on disk, where the desktop's
    /// Now Playing panels read it.
    pub fn save_art(&self, id: String, url: String) {
        let (http, dir) = (self.http.clone(), self.paths.cache.join("art"));
        let emit = self.emitter();
        self.runtime.spawn(async move {
            let path = dir.join(format!("{}.jpg", id.replace(['/', '\\', '.'], "_")));
            if !path.exists() {
                let bytes = async {
                    http.get(&url)
                        .send()
                        .await?
                        .error_for_status()?
                        .bytes()
                        .await
                }
                .await;
                let Ok(bytes) = bytes else { return };
                if tokio::fs::create_dir_all(&dir).await.is_err()
                    || tokio::fs::write(&path, &bytes).await.is_err()
                {
                    return;
                }
            }
            emit(Event::Art { id, path });
        });
    }
}
