//! The player engine, on a thread of its own. It fetches audio, keeps the
//! next track downloaded, moves on by itself when a track ends (crossfading
//! when that is on), and owns the output device. The interface tells it what
//! to play and what comes next; it reports what it did.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use fastframe_audio::{Output, OutputOptions, Render};

use crate::audio::{Eq, Mixer, Track, Voice};
use crate::stream::{self, Ytdlp};

const TICK: Duration = Duration::from_millis(50);
/// Downloads kept in memory: the current track, the next, a few behind.
const CACHED_TRACKS: usize = 6;
/// A skip fades the old track out this fast instead of cutting it.
const SKIP_FADE: f32 = 0.04;

#[derive(Clone, Debug, PartialEq)]
pub struct TrackRef {
    pub id: String,
    /// The length the listing gave, for tracks whose file does not say.
    pub duration: Option<f64>,
}

pub enum Command {
    Play {
        track: TrackRef,
        from: f64,
        paused: bool,
    },
    /// What follows the current track when it ends by itself.
    Next(Option<TrackRef>),
    Pause,
    Resume,
    Seek(f64),
    Volume(f32),
    Eq(Eq),
    Crossfade(f32),
    Stop,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// The track asked for is playing.
    Started {
        id: String,
    },
    /// The current track ended and the player moved to the one it was told
    /// comes next.
    Advanced {
        id: String,
    },
    /// The last track ended and nothing follows.
    Ended,
    Failed {
        id: String,
        error: String,
    },
}

enum Message {
    Command(Command),
    Fetched(String, Result<Arc<Vec<u8>>, String>),
}

pub struct Player {
    tx: mpsc::Sender<Message>,
    position_ms: Arc<AtomicU64>,
    length_ms: Arc<AtomicU64>,
}

impl Player {
    pub fn start(
        runtime: tokio::runtime::Handle,
        http: reqwest::Client,
        ytdlp: Arc<Ytdlp>,
        eq: Eq,
        volume: f32,
        crossfade: f32,
        emit: impl Fn(Event) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let position_ms = Arc::new(AtomicU64::new(0));
        let length_ms = Arc::new(AtomicU64::new(0));
        let engine = Engine {
            mixer: Arc::new(Mutex::new(Mixer::new(eq, volume))),
            output: None,
            cache: VecDeque::new(),
            fetching: HashSet::new(),
            waiting: None,
            current: None,
            next: None,
            crossfade,
            paused: true,
            runtime,
            http,
            ytdlp,
            tx: tx.clone(),
            emit: Box::new(emit),
            position_ms: position_ms.clone(),
            length_ms: length_ms.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("player".into())
            .spawn(move || engine.run(rx));
        if let Err(error) = spawned {
            log::error!("could not start the player: {error}");
        }
        Self {
            tx,
            position_ms,
            length_ms,
        }
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(Message::Command(command));
    }

    /// Seconds into the current track.
    pub fn position(&self) -> f64 {
        self.position_ms.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// The current track's length as its file tells it, once it plays.
    pub fn length(&self) -> Option<f64> {
        let ms = self.length_ms.load(Ordering::Relaxed);
        (ms > 0).then(|| ms as f64 / 1000.0)
    }
}

struct Renderer {
    mixer: Arc<Mutex<Mixer>>,
    position_ms: Arc<AtomicU64>,
}

impl Render for Renderer {
    fn configure(&mut self, sample_rate: u32, channels: u16) {
        lock(&self.mixer).configure(sample_rate, channels);
    }

    fn render(&mut self, out: &mut [f32]) {
        // The player thread holds the lock for microseconds; rather than
        // wait in the device callback, this block is silent.
        let Ok(mut mixer) = self.mixer.try_lock() else {
            out.fill(0.0);
            return;
        };
        mixer.render(out);
        if let Some(voice) = mixer.current() {
            self.position_ms
                .store((voice.track.position() * 1000.0) as u64, Ordering::Relaxed);
        }
    }
}

fn lock(mixer: &Mutex<Mixer>) -> MutexGuard<'_, Mixer> {
    mixer.lock().unwrap_or_else(|e| e.into_inner())
}

struct Engine {
    mixer: Arc<Mutex<Mixer>>,
    output: Option<Output<Renderer>>,
    cache: VecDeque<(String, Arc<Vec<u8>>)>,
    fetching: HashSet<String>,
    /// A track asked for whose audio is still downloading, and where it starts.
    waiting: Option<(TrackRef, f64)>,
    current: Option<TrackRef>,
    next: Option<TrackRef>,
    crossfade: f32,
    paused: bool,
    runtime: tokio::runtime::Handle,
    http: reqwest::Client,
    ytdlp: Arc<Ytdlp>,
    tx: mpsc::Sender<Message>,
    emit: Box<dyn Fn(Event) + Send>,
    position_ms: Arc<AtomicU64>,
    length_ms: Arc<AtomicU64>,
}

impl Engine {
    fn run(mut self, rx: mpsc::Receiver<Message>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Message::Command(command)) => self.command(command),
                Ok(Message::Fetched(id, result)) => self.fetched(id, result),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.tick();
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Play {
                track,
                from,
                paused,
            } => {
                self.paused = paused;
                self.position_ms
                    .store((from * 1000.0) as u64, Ordering::Relaxed);
                self.length_ms.store(
                    (track.duration.unwrap_or(0.0) * 1000.0) as u64,
                    Ordering::Relaxed,
                );
                self.fade_out_all(SKIP_FADE);
                self.current = Some(track.clone());
                match self.cached(&track.id) {
                    Some(bytes) => self.begin(&track.id, bytes, from, None),
                    None => {
                        self.fetch(&track.id);
                        self.waiting = Some((track, from));
                    }
                }
                self.apply_pause();
            }
            Command::Next(next) => {
                if let Some(next) = &next {
                    self.fetch(&next.id);
                }
                self.next = next;
            }
            Command::Pause => {
                self.paused = true;
                self.apply_pause();
            }
            Command::Resume => {
                self.paused = false;
                self.apply_pause();
            }
            Command::Seek(seconds) => {
                self.position_ms
                    .store((seconds * 1000.0) as u64, Ordering::Relaxed);
                match &mut self.waiting {
                    Some((_, from)) => *from = seconds,
                    None => {
                        if let Some(voice) = lock(&self.mixer).current_mut() {
                            voice.seek(seconds);
                        }
                    }
                }
            }
            Command::Volume(volume) => lock(&self.mixer).volume = volume.clamp(0.0, 1.0),
            Command::Eq(eq) => lock(&self.mixer).set_eq(eq),
            Command::Crossfade(seconds) => self.crossfade = seconds.max(0.0),
            Command::Stop => {
                lock(&self.mixer).voices.clear();
                self.current = None;
                self.waiting = None;
                self.paused = true;
                self.apply_pause();
            }
        }
    }

    fn fetched(&mut self, id: String, result: Result<Arc<Vec<u8>>, String>) {
        self.fetching.remove(&id);
        match result {
            Ok(bytes) => {
                self.cache.push_back((id.clone(), bytes.clone()));
                self.evict();
                if let Some((track, from)) = self.waiting.take_if(|(t, _)| t.id == id) {
                    self.begin(&track.id, bytes, from, None);
                    self.apply_pause();
                }
            }
            Err(error) => {
                log::warn!("{id}: {error}");
                if self.waiting.take_if(|(t, _)| t.id == id).is_some() {
                    (self.emit)(Event::Failed { id, error });
                }
            }
        }
    }

    fn cached(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        self.cache
            .iter()
            .find(|(cached, _)| cached == id)
            .map(|(_, bytes)| bytes.clone())
    }

    fn evict(&mut self) {
        let keep: Vec<String> = [&self.current, &self.next]
            .into_iter()
            .flatten()
            .map(|t| t.id.clone())
            .chain(self.waiting.iter().map(|(t, _)| t.id.clone()))
            .collect();
        while self.cache.len() > CACHED_TRACKS {
            let Some(at) = self.cache.iter().position(|(id, _)| !keep.contains(id)) else {
                break;
            };
            self.cache.remove(at);
        }
    }

    fn fetch(&mut self, id: &str) {
        if self.cached(id).is_some() || !self.fetching.insert(id.to_string()) {
            return;
        }
        let (http, ytdlp, tx, id) = (
            self.http.clone(),
            self.ytdlp.clone(),
            self.tx.clone(),
            id.to_string(),
        );
        self.runtime.spawn(async move {
            let result = stream::fetch(&http, &ytdlp, &id)
                .await
                .map(Arc::new)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Message::Fetched(id, result));
        });
    }

    fn fade_out_all(&mut self, seconds: f32) {
        let mut mixer = lock(&self.mixer);
        let frames = mixer.frames(seconds);
        for voice in &mut mixer.voices {
            voice.leaving = true;
            voice.fade(None, 0.0, frames);
        }
    }

    /// Starts a track's audio; with `fade_in`, from silence over that long.
    fn begin(&mut self, id: &str, bytes: Arc<Vec<u8>>, from: f64, fade_in: Option<f32>) {
        let track = match Track::open(bytes) {
            Ok(track) => track,
            Err(error) => {
                (self.emit)(Event::Failed {
                    id: id.to_string(),
                    error: format!("{error:#}"),
                });
                return;
            }
        };
        let length = track
            .duration
            .or(self.current.as_ref().and_then(|t| t.duration))
            .unwrap_or(0.0);
        self.length_ms
            .store((length * 1000.0) as u64, Ordering::Relaxed);
        let mut voice = Voice::new(id.to_string(), track);
        if from > 0.0 {
            voice.seek(from);
        }
        let mut mixer = lock(&self.mixer);
        if let Some(seconds) = fade_in {
            let frames = mixer.frames(seconds);
            voice.fade(Some(0.0), 1.0, frames);
        }
        mixer.voices.push(voice);
        drop(mixer);
        if fade_in.is_none() {
            (self.emit)(Event::Started { id: id.to_string() });
        }
    }

    fn apply_pause(&mut self) {
        if self.paused {
            if let Some(output) = &mut self.output {
                output.pause();
            }
            return;
        }
        if self.output.is_none() {
            let renderer = Renderer {
                mixer: self.mixer.clone(),
                position_ms: self.position_ms.clone(),
            };
            let options = OutputOptions {
                sample_rate: Some(44_100),
                ..OutputOptions::default()
            };
            match Output::open(options, renderer) {
                Ok(output) => self.output = Some(output),
                Err(error) => {
                    log::warn!("no audio output: {error}");
                    return;
                }
            }
        }
        if let Some(output) = &mut self.output {
            output.resume();
        }
    }

    fn tick(&mut self) {
        if let Some(output) = &mut self.output {
            output.maintain();
            for error in output.take_errors() {
                log::warn!("audio output: {error}");
            }
        }
        if self.paused || self.waiting.is_some() {
            return;
        }
        let Some(current) = self.current.clone() else {
            return;
        };
        let (position, length, gone) = {
            let mixer = lock(&self.mixer);
            match mixer.current() {
                Some(voice) => (voice.track.position(), voice.track.duration, voice.finished),
                None => (0.0, None, true),
            }
        };
        if gone {
            self.advance(None);
            return;
        }
        let Some(length) = length.or(current.duration) else {
            return;
        };
        let remaining = (length - position) as f32;
        let ready = self
            .next
            .as_ref()
            .is_some_and(|n| n.id != current.id && self.cached(&n.id).is_some());
        if self.crossfade > 0.0 && ready && remaining <= self.crossfade && remaining > 0.0 {
            self.fade_out_all(remaining);
            self.advance(Some(remaining));
        }
    }

    /// Moves to the track that comes next, crossfading over `fade` seconds.
    fn advance(&mut self, fade: Option<f32>) {
        let Some(next) = self.next.take() else {
            self.current = None;
            (self.emit)(Event::Ended);
            return;
        };
        self.current = Some(next.clone());
        self.position_ms.store(0, Ordering::Relaxed);
        self.length_ms.store(
            (next.duration.unwrap_or(0.0) * 1000.0) as u64,
            Ordering::Relaxed,
        );
        (self.emit)(Event::Advanced {
            id: next.id.clone(),
        });
        match self.cached(&next.id) {
            Some(bytes) => self.begin(&next.id, bytes, 0.0, fade),
            None => {
                self.fetch(&next.id);
                self.waiting = Some((next, 0.0));
            }
        }
    }
}
