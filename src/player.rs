//! The player engine, on a thread of its own. It starts a track's download
//! and plays it as it arrives, keeps the next track coming, moves on by
//! itself when a track ends (crossfading when that is on), and owns the
//! output device. The interface tells it what to play and what comes next;
//! it reports what it did.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

#[cfg(not(target_os = "macos"))]
use fastframe_audio::{Output, OutputOptions, Render};

#[cfg(target_os = "macos")]
use crate::mac_output::MacOutput;

use crate::audio::{Eq, Mixer, Voice};
use crate::innertube::Innertube;
use crate::stream::{Download, Resolver};

const TICK: Duration = Duration::from_millis(50);
/// Downloads kept in memory: the current track, the next, a couple behind.
const KEPT_DOWNLOADS: usize = 4;
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
    /// A track is likely to be played soon: resolve it ahead.
    Warm(String),
    Pause,
    Resume,
    Seek(f64),
    Volume(f32),
    Eq(Eq),
    Crossfade(f32),
    /// A device (by unique ID) that plays along with the default one.
    SecondOutput(Option<String>),
    Stop,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// The track asked for is sounding.
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

pub struct Player {
    tx: mpsc::Sender<Command>,
    position_ms: Arc<AtomicU64>,
    length_ms: Arc<AtomicU64>,
}

/// What a download needs: the HTTP client, the resolver, and the session
/// (through the API client, which keeps it current).
#[derive(Clone)]
pub struct Fetcher {
    pub http: reqwest::Client,
    pub resolver: Arc<Resolver>,
    pub api: Arc<Innertube>,
}

impl Player {
    pub fn start(
        runtime: tokio::runtime::Handle,
        fetcher: Fetcher,
        eq: Eq,
        volume: f32,
        crossfade: f32,
        emit: impl Fn(Event) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let position_ms = Arc::new(AtomicU64::new(0));
        let length_ms = Arc::new(AtomicU64::new(0));
        let (position, length) = (position_ms.clone(), length_ms.clone());
        // Built on its own thread: on macOS the output holds AVFoundation
        // objects, which stay on the thread that made them.
        let spawned = std::thread::Builder::new()
            .name("player".into())
            .spawn(move || {
                // On macOS the volume is the renderers', applied after the queue.
                let mixer_volume = if cfg!(target_os = "macos") {
                    1.0
                } else {
                    volume
                };
                let engine = Engine {
                    mixer: Arc::new(Mutex::new(Mixer::new(eq, mixer_volume))),
                    output: None,
                    second_output: None,
                    volume,
                    downloads: VecDeque::new(),
                    current: None,
                    next: None,
                    started: false,
                    crossfade,
                    paused: true,
                    runtime,
                    fetcher,
                    emit: Box::new(emit),
                    position_ms: position,
                    length_ms: length,
                };
                engine.run(rx)
            });
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
        let _ = self.tx.send(command);
    }

    /// Seconds into the current track.
    pub fn position(&self) -> f64 {
        self.position_ms.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// The current track's length: the listing's until its file tells its own.
    pub fn length(&self) -> Option<f64> {
        let ms = self.length_ms.load(Ordering::Relaxed);
        (ms > 0).then(|| ms as f64 / 1000.0)
    }
}

#[cfg(not(target_os = "macos"))]
struct Renderer {
    mixer: Arc<Mutex<Mixer>>,
    position_ms: Arc<AtomicU64>,
}

#[cfg(not(target_os = "macos"))]
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
                .store((voice.position() * 1000.0) as u64, Ordering::Relaxed);
        }
    }
}

pub(crate) fn lock(mixer: &Mutex<Mixer>) -> MutexGuard<'_, Mixer> {
    mixer.lock().unwrap_or_else(|e| e.into_inner())
}

struct Engine {
    mixer: Arc<Mutex<Mixer>>,
    #[cfg(not(target_os = "macos"))]
    output: Option<Output<Renderer>>,
    #[cfg(target_os = "macos")]
    output: Option<MacOutput>,
    /// The device that plays along with the default one, by unique ID.
    second_output: Option<String>,
    volume: f32,
    /// Newest last.
    downloads: VecDeque<(String, Arc<Download>)>,
    current: Option<TrackRef>,
    next: Option<TrackRef>,
    /// `Started` went out for the current play.
    started: bool,
    crossfade: f32,
    paused: bool,
    runtime: tokio::runtime::Handle,
    fetcher: Fetcher,
    emit: Box<dyn Fn(Event) + Send>,
    position_ms: Arc<AtomicU64>,
    length_ms: Arc<AtomicU64>,
}

impl Engine {
    fn run(mut self, rx: mpsc::Receiver<Command>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(command) => self.command(command),
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
                self.store_position(from);
                self.store_length(track.duration);
                self.fade_out_all(SKIP_FADE);
                self.flush_output();
                self.current = Some(track.clone());
                self.begin(&track.id, from, None);
                self.apply_pause();
            }
            Command::Next(next) => {
                if let Some(next) = &next {
                    self.download(&next.id);
                }
                self.next = next;
            }
            Command::Warm(id) => {
                let session = self.fetcher.api.session();
                self.fetcher.resolver.warm(&self.runtime, id, session);
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
                self.store_position(seconds);
                if let Some(voice) = lock(&self.mixer).current_mut() {
                    voice.seek(seconds);
                }
                self.flush_output();
            }
            Command::Volume(volume) => self.set_volume(volume.clamp(0.0, 1.0)),
            Command::SecondOutput(device) => {
                self.second_output = device;
                // A new pair of renderers; the old queue goes with the old one.
                self.output = None;
                self.apply_pause();
            }
            Command::Eq(eq) => lock(&self.mixer).set_eq(eq),
            Command::Crossfade(seconds) => self.crossfade = seconds.max(0.0),
            Command::Stop => {
                lock(&self.mixer).voices.clear();
                self.flush_output();
                self.current = None;
                self.paused = true;
                self.apply_pause();
            }
        }
    }

    fn store_position(&self, seconds: f64) {
        self.position_ms
            .store((seconds * 1000.0) as u64, Ordering::Relaxed);
    }

    fn store_length(&self, seconds: Option<f64>) {
        self.length_ms
            .store((seconds.unwrap_or(0.0) * 1000.0) as u64, Ordering::Relaxed);
    }

    /// The download of a track, started when there is none yet. The ones
    /// past the few kept are cancelled, never the current or the next.
    fn download(&mut self, id: &str) -> Arc<Download> {
        if let Some(at) = self.downloads.iter().position(|(kept, _)| kept == id) {
            let entry = self.downloads.remove(at).expect("position is in range");
            let download = entry.1.clone();
            self.downloads.push_back(entry);
            return download;
        }
        let fetcher = &self.fetcher;
        let download = Download::start(
            &self.runtime,
            fetcher.http.clone(),
            fetcher.resolver.clone(),
            id.to_string(),
            fetcher.api.session(),
        );
        self.downloads.push_back((id.to_string(), download.clone()));
        let keep: Vec<&str> = [&self.current, &self.next]
            .into_iter()
            .flatten()
            .map(|t| t.id.as_str())
            .chain([id])
            .collect();
        while self.downloads.len() > KEPT_DOWNLOADS {
            let Some(at) = self
                .downloads
                .iter()
                .position(|(kept, _)| !keep.contains(&kept.as_str()))
            else {
                break;
            };
            if let Some((_, dropped)) = self.downloads.remove(at) {
                dropped.cancel();
            }
        }
        download
    }

    fn fade_out_all(&mut self, seconds: f32) {
        let mut mixer = lock(&self.mixer);
        let frames = mixer.frames(seconds);
        for voice in &mut mixer.voices {
            voice.leaving = true;
            voice.fade(None, 0.0, frames);
        }
    }

    /// Starts a track's audio, which sounds as soon as its first chunk is
    /// in; with `fade_in`, from silence over that long.
    fn begin(&mut self, id: &str, from: f64, fade_in: Option<f32>) {
        let download = self.download(id);
        let mut voice = Voice::new(Box::new(download.reader()), from);
        let mut mixer = lock(&self.mixer);
        if let Some(seconds) = fade_in {
            let frames = mixer.frames(seconds);
            voice.fade(Some(0.0), 1.0, frames);
        }
        mixer.voices.push(voice);
        drop(mixer);
        self.started = false;
    }

    #[cfg(not(target_os = "macos"))]
    fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        lock(&self.mixer).volume = volume;
    }

    #[cfg(target_os = "macos")]
    fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if let Some(output) = &self.output {
            output.set_volume(volume * volume);
        }
    }

    /// Drops audio queued at the output, so what the mixer does next is
    /// heard now. Only the macOS output queues ahead.
    fn flush_output(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(output) = &mut self.output {
            output.flush();
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn apply_pause(&mut self) {
        if self.second_output.is_some() {
            log::info!("a second output is supported on macOS only");
        }
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
            // Opus decodes at 48 kHz: asked for first, so it plays unresampled.
            let options = OutputOptions {
                sample_rate: Some(crate::audio::OPUS_RATE),
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

    #[cfg(target_os = "macos")]
    fn apply_pause(&mut self) {
        if self.paused {
            if let Some(output) = &mut self.output {
                output.pause();
            }
            return;
        }
        if self.output.is_none() {
            match MacOutput::new(self.second_output.as_deref(), &self.mixer) {
                Ok(output) => {
                    self.output = Some(output);
                    self.set_volume(self.volume);
                }
                Err(error) => {
                    log::warn!("no audio output: {error:#}");
                    return;
                }
            }
        }
        if let Some(output) = &mut self.output {
            output.play();
        }
    }

    /// Keeps the output fed and working, and the shown position on what is
    /// heard.
    fn maintain_output(&mut self) {
        #[cfg(not(target_os = "macos"))]
        if let Some(output) = &mut self.output {
            output.maintain();
            for error in output.take_errors() {
                log::warn!("audio output: {error}");
            }
        }
        #[cfg(target_os = "macos")]
        {
            if self.output.as_ref().is_some_and(MacOutput::failed) {
                // Most often the second device went away: carry on without it.
                log::warn!("audio output stopped; reopening on the default device");
                self.second_output = None;
                self.output = None;
                self.apply_pause();
            }
            if let Some(output) = &mut self.output {
                output.pump(&self.mixer);
                let ahead = output.ahead();
                if let Some(voice) = lock(&self.mixer).current() {
                    let heard = (voice.position() - ahead).max(0.0);
                    self.store_position(heard);
                }
            }
        }
    }

    fn tick(&mut self) {
        self.maintain_output();
        if self.paused {
            return;
        }
        let Some(current) = self.current.clone() else {
            return;
        };
        let (position, length, finished, started, failure) = {
            let mixer = lock(&self.mixer);
            match mixer.current() {
                Some(voice) => (
                    voice.position(),
                    voice.duration(),
                    voice.finished,
                    voice.started,
                    voice.failure(),
                ),
                None => (0.0, None, true, false, None),
            }
        };
        if let Some(error) = failure {
            self.fade_out_all(SKIP_FADE);
            self.current = None;
            (self.emit)(Event::Failed {
                id: current.id,
                error,
            });
            return;
        }
        if started && !self.started {
            self.started = true;
            (self.emit)(Event::Started {
                id: current.id.clone(),
            });
        }
        if length.is_some() {
            self.store_length(length);
        }
        if finished {
            self.advance(None);
            return;
        }
        let Some(length) = length.or(current.duration) else {
            return;
        };
        let remaining = (length - position) as f32;
        let ready = self.next.as_ref().is_some_and(|next| {
            next.id != current.id
                && self
                    .downloads
                    .iter()
                    .any(|(id, download)| *id == next.id && download.ready())
        });
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
        self.store_position(0.0);
        self.store_length(next.duration);
        (self.emit)(Event::Advanced {
            id: next.id.clone(),
        });
        self.begin(&next.id, 0.0, fade);
    }
}
