//! The sound itself: decoding, resampling to the device, the equaliser, the
//! crossfade between two tracks, and the volume. Each track decodes on a
//! thread of its own; the mixer runs wherever the output renders it: the
//! device callback (`player.rs`'s `Renderer`) or, on macOS, the player thread
//! (`MacOutput::pump`), where the renderers apply the volume instead.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel, sync_channel};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, CODEC_TYPE_OPUS, Decoder, DecoderOptions};
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;

/// Centre frequencies of the equaliser's bands, in Hz.
pub const EQ_BANDS: [f32; 9] = [
    62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];
/// How far a band moves either way, in dB.
pub const EQ_RANGE: f32 = 12.0;
/// Peaks about an octave wide, matching the bands' octave spacing.
const EQ_Q: f32 = 1.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EqPreset {
    Flat,
    Bass,
    Vocal,
    Treble,
    Rock,
    Late,
    Custom,
}

impl EqPreset {
    pub const ALL: [EqPreset; 7] = [
        EqPreset::Flat,
        EqPreset::Bass,
        EqPreset::Vocal,
        EqPreset::Treble,
        EqPreset::Rock,
        EqPreset::Late,
        EqPreset::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EqPreset::Flat => "Flat",
            EqPreset::Bass => "Bass boost",
            EqPreset::Vocal => "Vocal",
            EqPreset::Treble => "Treble",
            EqPreset::Rock => "Rock",
            EqPreset::Late => "Late night",
            EqPreset::Custom => "Custom",
        }
    }

    /// The preset curves, from YTubic's design handoff.
    pub fn gains(self) -> Option<[f32; EQ_BANDS.len()]> {
        Some(match self {
            EqPreset::Flat => [0.0; EQ_BANDS.len()],
            EqPreset::Bass => [10.0, 8.0, 5.0, 2.0, 0.0, -1.0, -1.0, 0.0, 0.0],
            EqPreset::Vocal => [-3.0, -1.0, 2.0, 5.0, 6.0, 4.0, 2.0, 0.0, -1.0],
            EqPreset::Treble => [-2.0, -1.0, 0.0, 1.0, 2.0, 4.0, 7.0, 9.0, 10.0],
            // Winamp's Rock curve on these bands: lows and highs up, mids back.
            EqPreset::Rock => [5.0, 3.0, -2.0, -4.0, -2.0, 1.0, 4.0, 5.0, 6.0],
            EqPreset::Late => [5.0, 4.0, 2.0, 0.0, -1.0, -2.0, -3.0, -4.0, -6.0],
            EqPreset::Custom => return None,
        })
    }
}

/// Opus always decodes at 48 kHz, whatever the container says.
pub(crate) const OPUS_RATE: u32 = 48_000;
/// The longest Opus packet: 120 ms at 48 kHz.
const OPUS_MAX_FRAMES: usize = 5760;

/// Symphonia decodes AAC; Opus, which it does not, goes to libopus
/// (`opusic_c`).
enum Codec {
    Symphonia(Box<dyn Decoder>),
    Opus {
        decoder: opusic_c::Decoder,
        /// Encoder priming at the start, which is not part of the song.
        skip: usize,
        buffer: Vec<f32>,
    },
}

/// One decoded track, packet by packet, always as stereo.
pub struct Track {
    format: Box<dyn FormatReader>,
    codec: Codec,
    track_id: u32,
    time_base: Option<TimeBase>,
    pub rate: u32,
    channels: usize,
    /// Frames handed out since the start (or the last seek).
    frames: u64,
    pub duration: Option<f64>,
    /// Why reading stopped early: the download failed, not the file ended.
    pub failure: Option<String>,
}

impl Track {
    pub fn open(source: Box<dyn MediaSource>) -> Result<Self> {
        let source = MediaSourceStream::new(source, Default::default());
        let probed = symphonia::default::get_probe()
            .format(
                &Hint::new(),
                source,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .context("not an audio file this player can read")?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .context("no audio track")?;
        let params = &track.codec_params;
        let channels = params.channels.map_or(2, |c| c.count());
        let (codec, rate) = if params.codec == CODEC_TYPE_OPUS {
            // OpusHead: magic, version, channels, then the pre-skip.
            let skip = params
                .extra_data
                .as_deref()
                .filter(|head| head.len() >= 12 && head.starts_with(b"OpusHead"))
                .map_or(0, |head| {
                    usize::from(u16::from_le_bytes([head[10], head[11]]))
                });
            let layout = match channels {
                1 => opusic_c::Channels::Mono,
                2 => opusic_c::Channels::Stereo,
                n => anyhow::bail!("Opus with {n} channels"),
            };
            let decoder = opusic_c::Decoder::new(layout, opusic_c::SampleRate::Hz48000)
                .map_err(|e| anyhow::anyhow!("Opus: {e:?}"))?;
            let buffer = vec![0.0; OPUS_MAX_FRAMES * channels];
            (
                Codec::Opus {
                    decoder,
                    skip,
                    buffer,
                },
                OPUS_RATE,
            )
        } else {
            let decoder =
                symphonia::default::get_codecs().make(params, &DecoderOptions::default())?;
            (
                Codec::Symphonia(decoder),
                params.sample_rate.context("no sample rate")?,
            )
        };
        // Lengths count in the track's time base: samples in MP4,
        // milliseconds in WebM.
        let duration = match (params.n_frames, params.time_base) {
            (Some(n), Some(base)) => Some(base.calc_time(n)).map(|t| t.seconds as f64 + t.frac),
            (Some(n), None) => Some(n as f64 / f64::from(rate)),
            _ => None,
        };
        Ok(Self {
            track_id: track.id,
            time_base: params.time_base,
            format,
            codec,
            rate,
            channels,
            frames: 0,
            duration,
            failure: None,
        })
    }

    pub fn seek(&mut self, seconds: f64) {
        let to = SeekTo::Time {
            time: seconds.max(0.0).into(),
            track_id: Some(self.track_id),
        };
        match self.format.seek(SeekMode::Accurate, to) {
            Ok(seeked) => {
                match &mut self.codec {
                    Codec::Symphonia(decoder) => decoder.reset(),
                    Codec::Opus { decoder, skip, .. } => {
                        let _ = decoder.reset();
                        *skip = 0;
                    }
                }
                // The format lands on a packet at or before the time asked.
                let landed = self.time_base.map_or(seconds, |tb| {
                    let time = tb.calc_time(seeked.actual_ts);
                    time.seconds as f64 + time.frac
                });
                self.frames = (landed * f64::from(self.rate)) as u64;
            }
            Err(error) => log::warn!("seek failed: {error}"),
        }
    }

    /// The next packet's audio as interleaved stereo, or `None` at the end.
    fn next_block(&mut self) -> Option<Vec<f32>> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(DecodeError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return None;
                }
                Err(error) => {
                    self.failure = Some(error.to_string());
                    return None;
                }
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let mut decoded = Vec::new();
            match &mut self.codec {
                Codec::Symphonia(decoder) => match decoder.decode(&packet) {
                    Ok(audio) => {
                        let mut buffer =
                            SampleBuffer::<f32>::new(audio.capacity() as u64, *audio.spec());
                        buffer.copy_interleaved_ref(audio);
                        decoded.extend_from_slice(buffer.samples());
                    }
                    // A damaged packet costs its few milliseconds, not the track.
                    Err(DecodeError::DecodeError(_)) => continue,
                    Err(error) => {
                        self.failure = Some(error.to_string());
                        return None;
                    }
                },
                Codec::Opus {
                    decoder,
                    skip,
                    buffer,
                } => {
                    let Ok(frames) = decoder.decode_float_to_slice(&packet.data, buffer, false)
                    else {
                        continue;
                    };
                    let skipped = (*skip).min(frames);
                    *skip -= skipped;
                    decoded.extend_from_slice(
                        &buffer[skipped * self.channels..frames * self.channels],
                    );
                }
            }
            if decoded.is_empty() {
                continue;
            }
            let stereo: Vec<f32> = match self.channels {
                2 => decoded,
                1 => decoded.iter().flat_map(|s| [*s, *s]).collect(),
                n => decoded.chunks(n).flat_map(|f| [f[0], f[1]]).collect(),
            };
            self.frames += (stereo.len() / 2) as u64;
            return Some(stereo);
        }
    }
}

/// How far the decoder runs ahead of playback, in packets of ~20 ms.
const PCM_AHEAD: usize = 128;

/// Decoded audio on its way from the decoder thread to the device callback.
struct Pcm {
    /// Which seek this audio belongs to: older audio is dropped.
    generation: u64,
    /// The frame it starts at, counted from the start of the track.
    start: u64,
    rate: u32,
    samples: Vec<f32>,
}

/// A decoder thread for one track. It reads the download (waiting for the
/// parts that have not arrived; the device callback never waits) and stays
/// a couple of seconds ahead of playback.
struct Decoding {
    pcm: Receiver<Pcm>,
    seeks: Sender<f64>,
    generation: Arc<AtomicU64>,
    duration_ms: Arc<AtomicU64>,
    failure: Arc<Mutex<Option<String>>>,
}

impl Decoding {
    fn start(source: Box<dyn MediaSource>, from: f64) -> Self {
        let (pcm_tx, pcm) = sync_channel::<Pcm>(PCM_AHEAD);
        let (seeks, seek_rx) = channel::<f64>();
        let generation = Arc::new(AtomicU64::new(0));
        let duration_ms = Arc::new(AtomicU64::new(0));
        let failure = Arc::new(Mutex::new(None));
        let (thread_generation, thread_duration, thread_failure) =
            (generation.clone(), duration_ms.clone(), failure.clone());
        let fail = move |error: String| {
            *thread_failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
        };
        let spawned = std::thread::Builder::new()
            .name("decoder".into())
            .spawn(move || {
                let mut track = match Track::open(source) {
                    Ok(track) => track,
                    Err(error) => return fail(format!("{error:#}")),
                };
                if let Some(duration) = track.duration {
                    thread_duration.store((duration * 1000.0) as u64, Ordering::Relaxed);
                }
                if from > 0.0 {
                    track.seek(from);
                }
                let mut current = thread_generation.load(Ordering::Acquire);
                loop {
                    // Only the latest of several quick seeks matters.
                    if let Some(to) = seek_rx.try_iter().last() {
                        current = thread_generation.load(Ordering::Acquire);
                        track.seek(to);
                    }
                    let start = track.frames;
                    let Some(samples) = track.next_block() else {
                        if let Some(failure) = track.failure.take() {
                            fail(failure);
                        }
                        return;
                    };
                    let pcm = Pcm {
                        generation: current,
                        start,
                        rate: track.rate,
                        samples,
                    };
                    if pcm_tx.send(pcm).is_err() {
                        return; // the voice is gone
                    }
                }
            });
        if let Err(error) = spawned {
            *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
        }
        Self {
            pcm,
            seeks,
            generation,
            duration_ms,
            failure,
        }
    }
}

enum Frame {
    Audio([f32; 2]),
    /// Still downloading or decoding: silence until it arrives.
    Waiting,
    End,
}

/// A track on its way to the speakers: resampled, with its own fade.
pub struct Voice {
    decoding: Decoding,
    pcm: Option<Pcm>,
    read: usize,
    rate: u32,
    /// Frames into the track at its own rate, or where it was asked to start.
    frames: u64,
    from: f64,
    gain: f32,
    target: f32,
    step: f32,
    // ponytail: linear interpolation; a windowed-sinc resampler if anyone
    // hears the difference at 44.1 → 48 kHz.
    prev: [f32; 2],
    next: [f32; 2],
    phase: f64,
    primed: bool,
    /// The first frame has been mixed (on macOS, queued for the device).
    pub started: bool,
    pub finished: bool,
    /// Fading out after a skip or under a crossfade; no longer "the" track.
    pub leaving: bool,
}

impl Voice {
    /// Starts decoding `source` from `from` seconds.
    pub fn new(source: Box<dyn MediaSource>, from: f64) -> Self {
        Self {
            decoding: Decoding::start(source, from),
            pcm: None,
            read: 0,
            rate: 0,
            frames: 0,
            from,
            gain: 1.0,
            target: 1.0,
            step: 0.0,
            prev: [0.0; 2],
            next: [0.0; 2],
            phase: 0.0,
            primed: false,
            started: false,
            finished: false,
            leaving: false,
        }
    }

    /// Seconds into the track.
    pub fn position(&self) -> f64 {
        if self.rate == 0 {
            self.from
        } else {
            self.frames as f64 / f64::from(self.rate)
        }
    }

    /// The track's length as its file tells it, once the decoder has read it.
    pub fn duration(&self) -> Option<f64> {
        let ms = self.decoding.duration_ms.load(Ordering::Relaxed);
        (ms > 0).then(|| ms as f64 / 1000.0)
    }

    /// Why the track could not play, if it could not.
    pub fn failure(&self) -> Option<String> {
        self.decoding
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Ramps the gain to `target` over `frames` output frames.
    pub fn fade(&mut self, from: Option<f32>, target: f32, frames: f32) {
        if let Some(from) = from {
            self.gain = from;
        }
        self.target = target;
        self.step = (target - self.gain).abs() / frames.max(1.0);
    }

    pub fn seek(&mut self, seconds: f64) {
        self.decoding.generation.fetch_add(1, Ordering::AcqRel);
        let _ = self.decoding.seeks.send(seconds);
        self.pcm = None;
        self.read = 0;
        self.from = seconds;
        self.frames = (seconds * f64::from(self.rate)) as u64;
        self.primed = false;
        self.phase = 0.0;
        self.finished = false;
    }

    /// The next frame is decoded, or the track has ended: sampling will not
    /// wait on the decoder.
    fn buffered(&mut self) -> bool {
        let generation = self.decoding.generation.load(Ordering::Acquire);
        loop {
            if let Some(pcm) = &self.pcm
                && self.read + 1 < pcm.samples.len()
            {
                return true;
            }
            match self.decoding.pcm.try_recv() {
                Ok(pcm) if pcm.generation == generation => {
                    self.rate = pcm.rate;
                    self.read = 0;
                    self.pcm = Some(pcm);
                }
                Ok(_) => {} // from before a seek
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => return true,
            }
        }
    }

    fn next_frame(&mut self) -> Frame {
        let generation = self.decoding.generation.load(Ordering::Acquire);
        loop {
            if let Some(pcm) = &self.pcm
                && self.read + 1 < pcm.samples.len()
            {
                let frame = [pcm.samples[self.read], pcm.samples[self.read + 1]];
                self.read += 2;
                self.frames = pcm.start + (self.read / 2) as u64;
                return Frame::Audio(frame);
            }
            match self.decoding.pcm.try_recv() {
                Ok(pcm) if pcm.generation == generation => {
                    self.rate = pcm.rate;
                    self.read = 0;
                    self.pcm = Some(pcm);
                }
                Ok(_) => {} // from before a seek
                Err(TryRecvError::Empty) => return Frame::Waiting,
                Err(TryRecvError::Disconnected) => return Frame::End,
            }
        }
    }

    /// The next output frame at `out_rate`, `None` once the track has ended.
    fn sample(&mut self, out_rate: u32) -> Option<[f32; 2]> {
        if !self.primed {
            match self.next_frame() {
                Frame::Audio(frame) => self.prev = frame,
                Frame::Waiting => return Some([0.0; 2]),
                Frame::End => return None,
            }
            self.next = match self.next_frame() {
                Frame::Audio(frame) => frame,
                Frame::Waiting | Frame::End => self.prev,
            };
            self.primed = true;
            self.started = true;
        }
        let t = self.phase as f32;
        let out = [
            self.prev[0] + (self.next[0] - self.prev[0]) * t,
            self.prev[1] + (self.next[1] - self.prev[1]) * t,
        ];
        let ratio = f64::from(self.rate) / f64::from(out_rate);
        self.phase += ratio;
        while self.phase >= 1.0 {
            match self.next_frame() {
                Frame::Audio(frame) => {
                    self.phase -= 1.0;
                    self.prev = self.next;
                    self.next = frame;
                }
                // Behind the download: hold here, silent, until it catches up.
                Frame::Waiting => {
                    self.phase -= ratio;
                    return Some([0.0; 2]);
                }
                Frame::End => return None,
            }
        }
        if self.gain != self.target {
            self.gain = if self.gain < self.target {
                (self.gain + self.step).min(self.target)
            } else {
                (self.gain - self.step).max(self.target)
            };
        }
        Some([out[0] * self.gain, out[1] * self.gain])
    }
}

/// RBJ cookbook biquad, transposed direct form II, one state per channel.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: [f32; 2],
    z2: [f32; 2],
}

#[derive(Clone, Copy)]
enum Shape {
    LowShelf,
    Peak,
    HighShelf,
}

impl Biquad {
    fn design(shape: Shape, hz: f32, gain_db: f32, rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w = std::f32::consts::TAU * hz.min(rate * 0.45) / rate;
        let (sin, cos) = w.sin_cos();
        let (b0, b1, b2, a0, a1, a2) = match shape {
            Shape::Peak => {
                let alpha = sin / (2.0 * EQ_Q);
                (
                    1.0 + alpha * a,
                    -2.0 * cos,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cos,
                    1.0 - alpha / a,
                )
            }
            Shape::LowShelf | Shape::HighShelf => {
                // Shelf slope 1.
                let alpha = sin / 2.0 * std::f32::consts::SQRT_2;
                let k = 2.0 * a.sqrt() * alpha;
                let s = if matches!(shape, Shape::LowShelf) {
                    1.0
                } else {
                    -1.0
                };
                (
                    a * ((a + 1.0) - s * (a - 1.0) * cos + k),
                    s * 2.0 * a * ((a - 1.0) - s * (a + 1.0) * cos),
                    a * ((a + 1.0) - s * (a - 1.0) * cos - k),
                    (a + 1.0) + s * (a - 1.0) * cos + k,
                    -s * 2.0 * ((a - 1.0) + s * (a + 1.0) * cos),
                    (a + 1.0) + s * (a - 1.0) * cos - k,
                )
            }
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: [0.0; 2],
            z2: [0.0; 2],
        }
    }

    fn process(&mut self, channel: usize, x: f32) -> f32 {
        let y = self.b0 * x + self.z1[channel];
        self.z1[channel] = self.b1 * x - self.a1 * y + self.z2[channel];
        self.z2[channel] = self.b2 * x - self.a2 * y;
        y
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Eq {
    pub enabled: bool,
    pub gains: [f32; EQ_BANDS.len()],
}

struct Equalizer {
    settings: Eq,
    filters: Vec<Biquad>,
    pre_gain: f32,
}

impl Equalizer {
    fn new(settings: Eq, rate: u32) -> Self {
        let last = EQ_BANDS.len() - 1;
        let filters = EQ_BANDS
            .iter()
            .zip(settings.gains)
            .enumerate()
            .filter(|(_, (_, gain))| settings.enabled && *gain != 0.0)
            .map(|(i, (hz, gain))| {
                let shape = if i == 0 {
                    Shape::LowShelf
                } else if i == last {
                    Shape::HighShelf
                } else {
                    Shape::Peak
                };
                Biquad::design(shape, *hz, gain, rate as f32)
            })
            .collect();
        // A boost is paid for up front, so +10 dB cannot clip a loud master.
        let max_boost = if settings.enabled {
            settings.gains.iter().fold(0f32, |m, g| m.max(*g))
        } else {
            0.0
        };
        Self {
            settings,
            filters,
            pre_gain: 10f32.powf(-max_boost / 20.0),
        }
    }

    fn process(&mut self, frame: &mut [f32; 2]) {
        if self.filters.is_empty() {
            return;
        }
        for (channel, sample) in frame.iter_mut().enumerate() {
            let mut x = *sample * self.pre_gain;
            for filter in &mut self.filters {
                x = filter.process(channel, x);
            }
            *sample = x;
        }
    }
}

/// Everything that plays. Shared between the player thread, which changes
/// it, and whatever renders it: the device callback, or on macOS the player
/// thread itself.
pub struct Mixer {
    pub rate: u32,
    channels: u16,
    pub voices: Vec<Voice>,
    /// 0..1 as the slider shows it.
    pub volume: f32,
    gain: f32,
    equalizer: Equalizer,
}

impl Mixer {
    pub fn new(eq: Eq, volume: f32) -> Self {
        let rate = OPUS_RATE;
        Self {
            rate,
            channels: 2,
            voices: Vec::new(),
            volume,
            gain: volume * volume,
            equalizer: Equalizer::new(eq, rate),
        }
    }

    pub fn configure(&mut self, rate: u32, channels: u16) {
        self.rate = rate;
        self.channels = channels.max(1);
        self.equalizer = Equalizer::new(self.equalizer.settings, rate);
    }

    pub fn set_eq(&mut self, eq: Eq) {
        if eq != self.equalizer.settings {
            self.equalizer = Equalizer::new(eq, self.rate);
        }
    }

    /// The track playing now: the newest voice that is not leaving.
    pub fn current(&self) -> Option<&Voice> {
        self.voices.iter().rev().find(|v| !v.leaving)
    }

    pub fn current_mut(&mut self) -> Option<&mut Voice> {
        self.voices.iter_mut().rev().find(|v| !v.leaving)
    }

    pub fn frames(&self, seconds: f32) -> f32 {
        seconds * self.rate as f32
    }

    pub fn render(&mut self, out: &mut [f32]) {
        self.mix(out, false);
    }

    /// Like `render`, but stops where the playing track has nothing decoded
    /// yet instead of filling in silence, and returns the frames rendered.
    /// For an output that queues ahead, where that silence would be heard
    /// later, as a cut.
    pub fn render_decoded(&mut self, out: &mut [f32]) -> usize {
        self.mix(out, true)
    }

    fn mix(&mut self, out: &mut [f32], hold: bool) -> usize {
        let channels = usize::from(self.channels);
        // Volume moves are smoothed over ~20 ms, so a drag does not crackle.
        let target = self.volume * self.volume;
        let smooth = 1.0 - (-1.0 / (0.02 * self.rate as f32)).exp();
        let mut rendered = 0;
        for frame in out.chunks_mut(channels) {
            if hold
                && !self
                    .voices
                    .iter_mut()
                    .all(|v| v.finished || v.leaving || v.buffered())
            {
                break;
            }
            rendered += 1;
            let mut mix = [0f32; 2];
            for voice in &mut self.voices {
                if voice.finished {
                    continue;
                }
                match voice.sample(self.rate) {
                    Some([l, r]) => {
                        mix[0] += l;
                        mix[1] += r;
                    }
                    None => voice.finished = true,
                }
            }
            self.equalizer.process(&mut mix);
            self.gain += (target - self.gain) * smooth;
            let [l, r] = [mix[0] * self.gain, mix[1] * self.gain];
            match frame {
                [mono] => *mono = (l + r) * 0.5,
                [left, right, rest @ ..] => {
                    *left = l;
                    *right = r;
                    rest.fill(0.0);
                }
                [] => {}
            }
        }
        self.voices
            .retain(|v| !(v.finished || v.leaving && v.gain == 0.0 && v.target == 0.0));
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_eq_passes_sound_through() {
        let mut eq = Equalizer::new(
            Eq {
                enabled: true,
                gains: [0.0; 9],
            },
            48_000,
        );
        let mut frame = [0.5, -0.25];
        eq.process(&mut frame);
        assert_eq!(frame, [0.5, -0.25]);
    }

    #[test]
    fn a_boosted_band_lifts_its_frequency() {
        let rate = 48_000.0;
        let mut gains = [0.0; 9];
        gains[4] = 12.0;
        let mut eq = Equalizer::new(
            Eq {
                enabled: true,
                gains,
            },
            rate as u32,
        );
        let peak = |eq: &mut Equalizer, hz: f32| {
            (0..48_000)
                .map(|n| {
                    let x = (std::f32::consts::TAU * hz * n as f32 / rate).sin() * 0.1;
                    let mut frame = [x, x];
                    eq.process(&mut frame);
                    frame[0].abs()
                })
                .skip(24_000)
                .fold(0f32, f32::max)
        };
        // 1 kHz comes out ~12 dB up, minus the 12 dB headroom: about 0.1.
        let at_band = peak(&mut eq, 1000.0);
        let far = peak(&mut eq, 60.0);
        assert!((at_band - 0.1).abs() < 0.01, "{at_band}");
        assert!(far < 0.03, "{far}");
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use crate::stream::{Download, Resolver, Ytdlp};
    use std::time::Instant;

    /// Plays a voice to its end (or `limit` seconds) at 48 kHz: when the first
    /// sound came, how much played, and the peak and RMS level.
    fn drain(voice: &mut Voice, limit: f64) -> (Option<f64>, f64, f32, f64) {
        let started = Instant::now();
        let (mut first, mut frames, mut peak, mut energy) = (None, 0u64, 0f32, 0f64);
        while frames < (limit * 48_000.0) as u64 {
            match voice.sample(48_000) {
                None => break,
                Some(_) if !voice.started => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                Some([l, r]) => {
                    first.get_or_insert(started.elapsed().as_secs_f64());
                    frames += 1;
                    peak = peak.max(l.abs()).max(r.abs());
                    energy += f64::from(l * l + r * r);
                }
            }
        }
        let rms = (energy / (2.0 * frames.max(1) as f64)).sqrt();
        (first, frames as f64 / 48_000.0, peak, rms)
    }

    #[test]
    #[ignore = "downloads from YouTube"]
    fn streams_while_downloading_and_seeks_ahead() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let data = std::env::temp_dir().join("ytfast-test");
        let resolver = Arc::new(Resolver::new(Arc::new(Ytdlp::new(&data)), data.clone()));
        let http = reqwest::Client::new();
        let id = "dQw4w9WgXcQ";

        let started = Instant::now();
        let download = Download::start(
            runtime.handle(),
            http.clone(),
            resolver.clone(),
            id.into(),
            None,
        );
        let mut voice = Voice::new(Box::new(download.reader()), 0.0);
        let (first, played, peak, rms) = drain(&mut voice, 1000.0);
        let first = first.expect("sound");
        println!(
            "cold: first sound after {first:.2}s, {played:.1}s played, peak {peak:.2}, rms {rms:.3}"
        );
        assert!((played - 213.0).abs() < 2.0, "{played}");
        // Music, not silence and not noise: a mastered pop track sits here.
        assert!(
            peak <= 1.2 && rms > 0.05 && rms < 0.6,
            "peak {peak} rms {rms}"
        );
        println!("whole track in {:.2}s", started.elapsed().as_secs_f64());

        // Resolved already: a second track start is the download alone.
        let other = "4NRXx6U8ABQ";
        let warm = Instant::now();
        runtime
            .block_on(resolver.resolve(other, None))
            .expect("resolve");
        println!("resolve took {:.2}s", warm.elapsed().as_secs_f64());
        let download = Download::start(runtime.handle(), http, resolver, other.into(), None);
        let mut voice = Voice::new(Box::new(download.reader()), 150.0);
        let (first, _, _, _) = drain(&mut voice, 1.0);
        println!(
            "warm, at 150s: first sound after {:.2}s, now at {:.1}s",
            first.expect("sound"),
            voice.position()
        );
        assert!(
            (voice.position() - 151.0).abs() < 1.5,
            "{}",
            voice.position()
        );
    }
}
