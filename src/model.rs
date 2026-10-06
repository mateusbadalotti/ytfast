//! What the interface shows: items, shelves, pages, and their loading state.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Thumbnail {
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// A name that may link to a page: an artist, or an album on a track row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Song,
    Video,
    Album,
    Playlist,
    Artist,
}

/// One card or row. `id` is a videoId for songs and videos and a browseId
/// for everything else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub kind: Kind,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub thumbnails: Vec<Thumbnail>,
    pub artists: Vec<Link>,
    pub album: Option<Link>,
    pub duration: Option<u32>,
    pub explicit: bool,
    /// A "playlist" card that is really one long video with chapters: its
    /// playlist page is empty, so the card plays this video instead.
    pub play_video_id: Option<String>,
}

impl Item {
    pub fn is_playable(&self) -> bool {
        matches!(self.kind, Kind::Song | Kind::Video)
    }

    /// The item's page on music.youtube.com, to share.
    pub fn link(&self) -> String {
        let base = "https://music.youtube.com";
        match self.kind {
            Kind::Song | Kind::Video => format!("{base}/watch?v={}", self.id),
            Kind::Playlist => {
                let list = self.id.strip_prefix("VL").unwrap_or(&self.id);
                format!("{base}/playlist?list={list}")
            }
            Kind::Album => format!("{base}/browse/{}", self.id),
            Kind::Artist => format!("{base}/channel/{}", self.id),
        }
    }

    pub fn artist_names(&self) -> String {
        if self.artists.is_empty() {
            return self.subtitle.clone();
        }
        let names: Vec<&str> = self.artists.iter().map(|a| a.name.as_str()).collect();
        names.join(", ")
    }
}

/// Where a shelf's "More" link leads.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Browse {
    pub id: String,
    pub params: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shelf {
    pub title: String,
    pub items: Vec<Item>,
    /// Rows rather than cards (Top songs, Quick picks).
    pub list: bool,
    pub more: Option<Browse>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feed {
    /// The page's own title, when it has one ("More" pages).
    pub title: String,
    pub shelves: Vec<Shelf>,
    pub continuation: Option<String>,
}

/// An album or a playlist: a header and its tracks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Collection {
    pub id: String,
    pub title: String,
    pub artists: Vec<Link>,
    /// "Album • 2024" or the playlist's owner line.
    pub subtitle: String,
    /// "12 songs • 45 minutes".
    pub info: String,
    pub description: String,
    pub thumbnails: Vec<Thumbnail>,
    pub tracks: Vec<Item>,
    /// More tracks to load (long playlists).
    pub continuation: Option<String>,
}

/// A watch target from a header button: a seed video, a watch playlist, or both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Watch {
    pub video_id: Option<String>,
    pub playlist_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub description: String,
    pub audience: String,
    pub thumbnails: Vec<Thumbnail>,
    pub shuffle: Option<Watch>,
    pub radio: Option<Watch>,
    pub shelves: Vec<Shelf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Videos,
    Albums,
    Artists,
    Playlists,
}

impl SearchFilter {
    pub const ALL: [SearchFilter; 6] = [
        SearchFilter::All,
        SearchFilter::Songs,
        SearchFilter::Videos,
        SearchFilter::Albums,
        SearchFilter::Artists,
        SearchFilter::Playlists,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SearchFilter::All => "All",
            SearchFilter::Songs => "Songs",
            SearchFilter::Videos => "Videos",
            SearchFilter::Albums => "Albums",
            SearchFilter::Artists => "Artists",
            SearchFilter::Playlists => "Playlists",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchResults {
    pub top: Option<Item>,
    pub shelves: Vec<Shelf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rating {
    Like,
    Dislike,
    #[default]
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub name: String,
    pub email: String,
    pub photo: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LyricLine {
    /// Seconds into the track.
    pub start: f32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LyricsText {
    Timed(Vec<LyricLine>),
    Plain(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lyrics {
    pub text: LyricsText,
    pub source: &'static str,
}

/// Every screen the central panel can show.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Home,
    Search,
    Library,
    Album(String),
    Playlist(String),
    Artist(String),
    Browse(Browse),
    Settings,
}

impl Page {
    pub fn for_item(item: &Item) -> Option<Page> {
        match item.kind {
            Kind::Album => Some(Page::Album(item.id.clone())),
            Kind::Playlist => Some(Page::Playlist(item.id.clone())),
            Kind::Artist => Some(Page::Artist(item.id.clone())),
            Kind::Song | Kind::Video => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Loadable<T> {
    #[default]
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn get(&self) -> Option<&T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }

    pub fn get_mut(&mut self) -> Option<&mut T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }

    pub fn needs_load(&self) -> bool {
        matches!(self, Loadable::NotLoaded)
    }

    pub fn from_result(result: anyhow::Result<T>) -> Self {
        match result {
            Ok(value) => Loadable::Loaded(value),
            Err(error) => Loadable::Failed(format!("{error:#}")),
        }
    }
}
