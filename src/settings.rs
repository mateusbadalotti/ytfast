//! What the app remembers between runs: preferences, the queue, and where
//! playback was. One JSON file, written through a temporary file and a
//! rename so a crash mid-write leaves the previous copy whole. The session
//! is not here; it lives in the credential store (see `auth.rs`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::audio::{EQ_BANDS, EqPreset};
use crate::model::Thumbnail;
use crate::queue::Queue;

/// Queued tracks kept across runs, centred on the current one.
const KEPT_TRACKS: usize = 300;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    /// Seconds two tracks overlap; 0 is off.
    pub crossfade: f32,
    pub eq_enabled: bool,
    pub eq_preset: EqPreset,
    pub eq_custom: [f32; EQ_BANDS.len()],
    /// Keep playing similar songs when the queue runs out.
    pub autoplay: bool,
    /// The browser the session was read from.
    pub browser: String,
    /// A device (by unique ID) that plays along with the system's default
    /// output; macOS only.
    pub second_output: Option<String>,
    /// Playlists pinned to the top of the sidebar, in pinning order.
    pub pinned: Vec<String>,
    /// The last searches, newest first.
    pub recent_searches: Vec<String>,
    /// Home's sections, by YouTube's titles: the order set for them, and
    /// the ones left out.
    pub home_order: Vec<String>,
    pub home_hidden: Vec<String>,
    /// Artists picked for Home's sets and new-releases sections.
    pub home_artists: Vec<HomeArtist>,
    pub queue: Queue,
    /// Seconds into the current track when the app last closed.
    pub position: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HomeArtist {
    /// The artist's YouTube Music browse id.
    pub id: String,
    pub name: String,
    pub thumbnails: Vec<Thumbnail>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.8,
            crossfade: 0.0,
            eq_enabled: false,
            eq_preset: EqPreset::Flat,
            eq_custom: [0.0; EQ_BANDS.len()],
            autoplay: true,
            browser: "chrome".into(),
            second_output: None,
            pinned: Vec::new(),
            recent_searches: Vec::new(),
            home_order: Vec::new(),
            home_hidden: Vec::new(),
            home_artists: Vec::new(),
            queue: Queue::default(),
            position: 0.0,
        }
    }
}

impl Settings {
    pub fn eq_gains(&self) -> [f32; EQ_BANDS.len()] {
        self.eq_preset.gains().unwrap_or(self.eq_custom)
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                log::warn!("{}: {error}; starting from defaults", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut kept = self.clone();
        trim_queue(&mut kept.queue);
        let text = serde_json::to_string_pretty(&kept).map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temporary: PathBuf = path.with_extension("json.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)
    }
}

fn trim_queue(queue: &mut Queue) {
    if queue.tracks.len() <= KEPT_TRACKS {
        return;
    }
    let index = queue.index.unwrap_or(0);
    let start = index
        .saturating_sub(KEPT_TRACKS / 2)
        .min(queue.tracks.len() - KEPT_TRACKS);
    queue.tracks = queue.tracks.drain(start..start + KEPT_TRACKS).collect();
    queue.index = queue.index.map(|i| i - start);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_old_files() {
        let dir = std::env::temp_dir().join(format!("ytfast-settings-{}", std::process::id()));
        let path = dir.join("state.json");
        let settings = Settings {
            volume: 0.5,
            crossfade: 6.0,
            ..Settings::default()
        };
        settings.save(&path).expect("save");
        assert_eq!(Settings::load(&path), settings);
        std::fs::write(&path, r#"{"volume":0.3}"#).expect("write");
        let partial = Settings::load(&path);
        assert_eq!(partial.volume, 0.3);
        assert!(partial.autoplay);
        let _ = std::fs::remove_dir_all(dir);
    }
}
