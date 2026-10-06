//! The sound itself: decoding, resampling to the device, the equaliser, the
//! crossfade between two tracks, and the volume. It all runs inside the
//! device callback, which `player.rs` hands this mixer to.

use std::io::Cursor;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, Decoder, DecoderOptions};
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
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
    Late,
    Custom,
}

impl EqPreset {
    pub const ALL: [EqPreset; 6] = [
        EqPreset::Flat,
        EqPreset::Bass,
        EqPreset::Vocal,
        EqPreset::Treble,
        EqPreset::Late,
        EqPreset::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EqPreset::Flat => "Flat",
            EqPreset::Bass => "Bass boost",
            EqPreset::Vocal => "Vocal",
            EqPreset::Treble => "Treble",
            EqPreset::Late => "Late night",
            EqPreset::Custom => "Custom",
        }
    }

    /// The preset curves, from YTubic's design handoff.
    pub fn gains(self) -> Option<[f32; 9]> {
        Some(match self {
            EqPreset::Flat => [0.0; 9],
            EqPreset::Bass => [10.0, 8.0, 5.0, 2.0, 0.0, -1.0, -1.0, 0.0, 0.0],
            EqPreset::Vocal => [-3.0, -1.0, 2.0, 5.0, 6.0, 4.0, 2.0, 0.0, -1.0],
            EqPreset::Treble => [-2.0, -1.0, 0.0, 1.0, 2.0, 4.0, 7.0, 9.0, 10.0],
            EqPreset::Late => [5.0, 4.0, 2.0, 0.0, -1.0, -2.0, -3.0, -4.0, -6.0],
            EqPreset::Custom => return None,
        })
    }
}

/// One decoded track, frame by frame, always as stereo.
pub struct Track {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    pub rate: u32,
    channels: usize,
    samples: Vec<f32>,
    read: usize,
    /// Frames handed out since the start (or the last seek).
    frames: u64,
    pub duration: Option<f64>,
    done: bool,
}

impl Track {
    pub fn open(bytes: Arc<Vec<u8>>) -> Result<Self> {
        let source =
            MediaSourceStream::new(Box::new(Cursor::new(ArcBytes(bytes))), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("m4a");
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
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
        let rate = params.sample_rate.context("no sample rate")?;
        let duration = params.n_frames.map(|n| n as f64 / f64::from(rate));
        let channels = params.channels.map_or(2, |c| c.count());
        let decoder = symphonia::default::get_codecs().make(params, &DecoderOptions::default())?;
        Ok(Self {
            track_id: track.id,
            time_base: params.time_base,
            format,
            decoder,
            rate,
            channels,
            samples: Vec::new(),
            read: 0,
            frames: 0,
            duration,
            done: false,
        })
    }

    pub fn position(&self) -> f64 {
        self.frames as f64 / f64::from(self.rate)
    }

    pub fn seek(&mut self, seconds: f64) {
        let to = SeekTo::Time {
            time: seconds.max(0.0).into(),
            track_id: Some(self.track_id),
        };
        match self.format.seek(SeekMode::Accurate, to) {
            Ok(seeked) => {
                self.decoder.reset();
                self.samples.clear();
                self.read = 0;
                self.done = false;
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

    fn refill(&mut self) -> bool {
        while !self.done {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(_) => {
                    self.done = true;
                    break;
                }
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let mut buffer =
                        SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
                    buffer.copy_interleaved_ref(decoded);
                    self.samples.clear();
                    self.read = 0;
                    self.samples.extend_from_slice(buffer.samples());
                    if !self.samples.is_empty() {
                        return true;
                    }
                }
                // A damaged packet costs its few milliseconds, not the track.
                Err(DecodeError::DecodeError(_)) => continue,
                Err(_) => self.done = true,
            }
        }
        false
    }

    fn next_frame(&mut self) -> Option<[f32; 2]> {
        if self.read >= self.samples.len() && !self.refill() {
            return None;
        }
        let frame = &self.samples[self.read..self.read + self.channels];
        self.read += self.channels;
        self.frames += 1;
        Some(match frame {
            [mono] => [*mono, *mono],
            [left, right, ..] => [*left, *right],
            [] => [0.0, 0.0],
        })
    }
}

/// Lets the decoder read a shared download without copying it.
struct ArcBytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for ArcBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// A track on its way to the speakers: resampled, with its own fade.
pub struct Voice {
    pub id: String,
    pub track: Track,
    gain: f32,
    target: f32,
    step: f32,
    // ponytail: linear interpolation; a windowed-sinc resampler if anyone
    // hears the difference at 44.1 → 48 kHz.
    prev: [f32; 2],
    next: [f32; 2],
    phase: f64,
    primed: bool,
    pub finished: bool,
    /// Fading out after a skip or under a crossfade; no longer "the" track.
    pub leaving: bool,
}

impl Voice {
    pub fn new(id: String, track: Track) -> Self {
        Self {
            id,
            track,
            gain: 1.0,
            target: 1.0,
            step: 0.0,
            prev: [0.0; 2],
            next: [0.0; 2],
            phase: 0.0,
            primed: false,
            finished: false,
            leaving: false,
        }
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
        self.track.seek(seconds);
        self.primed = false;
        self.phase = 0.0;
        self.finished = false;
    }

    fn sample(&mut self, ratio: f64) -> Option<[f32; 2]> {
        if !self.primed {
            self.prev = self.track.next_frame()?;
            self.next = self.track.next_frame().unwrap_or(self.prev);
            self.primed = true;
        }
        let t = self.phase as f32;
        let out = [
            self.prev[0] + (self.next[0] - self.prev[0]) * t,
            self.prev[1] + (self.next[1] - self.prev[1]) * t,
        ];
        self.phase += ratio;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.prev = self.next;
            self.next = self.track.next_frame()?;
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
    pub gains: [f32; 9],
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
/// it, and the device callback, which renders it.
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
        let rate = 44_100;
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
        let channels = usize::from(self.channels);
        // Volume moves are smoothed over ~20 ms, so a drag does not crackle.
        let target = self.volume * self.volume;
        let smooth = 1.0 - (-1.0 / (0.02 * self.rate as f32)).exp();
        for frame in out.chunks_mut(channels) {
            let mut mix = [0f32; 2];
            for voice in &mut self.voices {
                if voice.finished {
                    continue;
                }
                let ratio = f64::from(voice.track.rate) / f64::from(self.rate);
                match voice.sample(ratio) {
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

    #[tokio::test]
    #[ignore = "downloads from YouTube"]
    async fn downloads_decodes_and_seeks() {
        let data = std::env::temp_dir().join("ytfast-test");
        let ytdlp = crate::stream::Ytdlp::new(&data);
        let http = reqwest::Client::new();
        let started = std::time::Instant::now();
        let bytes = Arc::new(
            crate::stream::fetch(&http, &ytdlp, "dQw4w9WgXcQ")
                .await
                .expect("fetch"),
        );
        println!(
            "{} bytes in {:.2}s",
            bytes.len(),
            started.elapsed().as_secs_f32()
        );
        let mut track = Track::open(bytes.clone()).expect("open");
        println!(
            "rate {} channels {} duration {:?}",
            track.rate, track.channels, track.duration
        );
        let mut frames = 0u64;
        while track.next_frame().is_some() {
            frames += 1;
        }
        let seconds = frames as f64 / f64::from(track.rate);
        println!("decoded {seconds:.1}s");
        assert!((seconds - 213.0).abs() < 2.0, "{seconds}");
        let mut track = Track::open(bytes).expect("open");
        track.seek(100.0);
        println!("after seek: {:.2}s", track.position());
        assert!((track.position() - 100.0).abs() < 1.0);
        let mut after = 0u64;
        while track.next_frame().is_some() {
            after += 1;
        }
        let rest = after as f64 / f64::from(track.rate);
        assert!((rest - (seconds - 100.0)).abs() < 1.5, "{rest}");
    }
}
