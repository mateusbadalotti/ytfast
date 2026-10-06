//! The output on macOS: AVSampleBufferAudioRenderer, fed the mixer's audio.
//!
//! Spatial Audio on AirPods applies to AVPlayer and AVSampleBufferAudioRenderer
//! output and never to an AUHAL client, which is what cpal opens. Through a
//! renderer the app keeps its own decoding (Opus, the equaliser, the
//! crossfade) and Control Center offers Off, Fixed and Head Tracked for it.
//!
//! A second device gets a second renderer on the same synchronizer, fed the
//! same buffers, so the two play in step from one clock. The system's own
//! multi-output device would do the same, but AirPods behind one lose
//! Spatial Audio.
//!
//! Everything here lives on the player thread: the AVFoundation objects are
//! not `Send`, and nothing else touches them.

use std::ffi::c_void;
use std::ptr::{NonNull, null, null_mut};
use std::sync::Mutex;

use anyhow::{Result, bail};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_av_foundation::{
    AVAudioSpatializationFormats, AVQueuedSampleBufferRendering, AVSampleBufferAudioRenderer,
    AVSampleBufferRenderSynchronizer,
};
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyStreamConfiguration, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, kAudioFormatFlagIsFloat,
    kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_media::{
    CMAudioFormatDescription, CMAudioFormatDescriptionCreate,
    CMAudioSampleBufferCreateReadyWithPacketDescriptions, CMBlockBuffer, CMSampleBuffer, CMTime,
    kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_foundation::NSString;

use crate::audio::Mixer;

pub const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;
/// Frames per buffer handed to the renderers: about 21 ms.
const BLOCK: usize = 1024;
/// Audio queued past what the device has taken. Pause and volume act at
/// once; what the mixer changes (a seek, a skip) flushes the queue instead of
/// waiting.
const AHEAD: f64 = 0.25;
/// Queued before a (re)started timeline begins to run.
const LEAD: f64 = 0.05;
/// The device's latency until it is measured: about Bluetooth's.
const LATENCY_GUESS: f64 = 0.3;

pub struct MacOutput {
    synchronizer: Retained<AVSampleBufferRenderSynchronizer>,
    renderers: Vec<Retained<AVSampleBufferAudioRenderer>>,
    format: CFRetained<CMAudioFormatDescription>,
    /// The timeline position, in frames, up to which audio is enqueued.
    queued: i64,
    playing: bool,
    /// The timeline waits at `start` for `LEAD` of audio before it runs.
    starting: bool,
    start: i64,
    /// How far ahead of the clock the device takes audio. The clock follows
    /// what is heard, so on Bluetooth it starts ~0.3 s after the rate is set.
    latency: f64,
    /// When the timeline was last set running, until its clock moves.
    measuring: Option<std::time::Instant>,
    block: Vec<f32>,
}

impl MacOutput {
    /// The system's default output, and `second` (a device UID) beside it.
    pub fn new(second: Option<&str>, mixer: &Mutex<Mixer>) -> Result<Self> {
        let asbd = AudioStreamBasicDescription {
            mSampleRate: f64::from(RATE),
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 4 * CHANNELS,
            mFramesPerPacket: 1,
            mBytesPerFrame: 4 * CHANNELS,
            mChannelsPerFrame: CHANNELS,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        let mut format: *const CMAudioFormatDescription = null();
        // SAFETY: the description and the out pointer outlive the call; no
        // channel layout, cookie or extensions are passed.
        let status = unsafe {
            CMAudioFormatDescriptionCreate(
                None,
                NonNull::from(&asbd),
                0,
                null(),
                0,
                null(),
                None,
                NonNull::from(&mut format),
            )
        };
        let Some(format) = NonNull::new(format.cast_mut()).filter(|_| status == 0) else {
            bail!("CoreMedia refused the audio format ({status})");
        };
        // SAFETY: created above with a +1 retain count that this takes over.
        let format = unsafe { CFRetained::from_raw(format) };

        // SAFETY: plain AVFoundation object creation and configuration.
        let (synchronizer, renderers) = unsafe {
            let synchronizer = AVSampleBufferRenderSynchronizer::new();
            // By default a rate change waits until the renderers judge they
            // have enough audio, a quarter of a second after every skip or
            // seek. The pump queues ahead before it starts the clock anyway.
            synchronizer.setDelaysRateChangeUntilHasSufficientMediaData(false);
            let mut renderers = vec![AVSampleBufferAudioRenderer::new()];
            if let Some(uid) = second {
                let renderer = AVSampleBufferAudioRenderer::new();
                renderer.setAudioOutputDeviceUniqueID(Some(&NSString::from_str(uid)));
                renderers.push(renderer);
            }
            for renderer in &renderers {
                // Audio-only content spatializes multichannel alone by
                // default; music is stereo.
                renderer.setAllowedAudioSpatializationFormats(
                    AVAudioSpatializationFormats::MonoStereoAndMultichannel,
                );
                synchronizer.addRenderer(ProtocolObject::from_ref(&**renderer));
            }
            (synchronizer, renderers)
        };
        crate::player::lock(mixer).configure(RATE, CHANNELS as u16);
        Ok(Self {
            synchronizer,
            renderers,
            format,
            queued: 0,
            playing: false,
            starting: true,
            start: 0,
            latency: LATENCY_GUESS,
            measuring: None,
            block: vec![0.0; BLOCK * CHANNELS as usize],
        })
    }

    pub fn play(&mut self) {
        self.playing = true;
        if !self.starting {
            // SAFETY: setting the synchronizer's rate.
            unsafe { self.synchronizer.setRate(1.0) };
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
        // SAFETY: setting the synchronizer's rate.
        unsafe { self.synchronizer.setRate(0.0) };
    }

    /// The volume, applied after the queue so a drag is heard at once.
    pub fn set_volume(&self, gain: f32) {
        for renderer in &self.renderers {
            // SAFETY: setting a renderer property.
            unsafe { renderer.setVolume(gain) };
        }
    }

    /// Drops what is queued, so a skip or a seek sounds now rather than
    /// after the queue.
    pub fn flush(&mut self) {
        // SAFETY: flushing the renderers and resetting the timeline.
        unsafe {
            self.synchronizer
                .setRate_time(0.0, CMTime::new(0, RATE as i32));
            for renderer in &self.renderers {
                renderer.flush();
            }
        }
        self.queued = 0;
        self.starting = true;
        self.start = 0;
        self.measuring = None;
    }

    /// The timeline's clock, in seconds.
    fn now(&self) -> f64 {
        // SAFETY: reading the synchronizer's clock.
        let now = unsafe { self.synchronizer.currentTime().seconds() };
        if now.is_finite() { now.max(0.0) } else { 0.0 }
    }

    /// Seconds of audio queued past what is heard now.
    pub fn ahead(&self) -> f64 {
        (self.queued as f64 / f64::from(RATE) - self.now()).max(0.0)
    }

    /// Runs the timeline from `start`, and times how long its clock takes to
    /// move: the device's latency.
    fn run_timeline(&mut self) {
        self.starting = false;
        self.measuring = Some(std::time::Instant::now());
        // SAFETY: setting the synchronizer's rate and time.
        unsafe {
            self.synchronizer
                .setRate_time(1.0, CMTime::new(self.start, RATE as i32))
        };
    }

    fn measure_latency(&mut self) {
        let Some(since) = self.measuring else {
            return;
        };
        let moved = self.now() - self.start as f64 / f64::from(RATE);
        if moved > 0.0 {
            self.latency = since.elapsed().as_secs_f64() - moved;
            self.measuring = None;
            log::debug!("audio output latency {:.0} ms", self.latency * 1000.0);
        }
    }

    /// A device stopped (unplugged, or taken by another app).
    pub fn failed(&self) -> bool {
        // SAFETY: reading renderer status.
        self.renderers.iter().any(|r| unsafe { r.status() } == objc2_av_foundation::AVQueuedSampleBufferRenderingStatus::Failed)
    }

    /// Renders and enqueues until `AHEAD` is queued past the device. Called
    /// every player tick.
    pub fn pump(&mut self, mixer: &Mutex<Mixer>) {
        if !self.playing {
            return;
        }
        self.measure_latency();
        if !self.starting && self.measuring.is_none() && self.ahead() == 0.0 {
            // Ran dry, behind the download: the timeline waits where the
            // audio stops rather than run on and skip what comes late.
            self.start = self.queued;
            self.starting = true;
            // SAFETY: setting the synchronizer's rate and time.
            unsafe {
                self.synchronizer
                    .setRate_time(0.0, CMTime::new(self.start, RATE as i32))
            };
        }
        while self.ready() && self.ahead() < self.latency + AHEAD {
            let frames = crate::player::lock(mixer).render_decoded(&mut self.block);
            if frames == 0 {
                return;
            }
            if let Err(error) = self.enqueue(frames) {
                log::warn!("audio output: {error:#}");
                return;
            }
            self.queued += frames as i64;
            if self.starting && (self.queued - self.start) as f64 >= LEAD * f64::from(RATE) {
                self.run_timeline();
            }
        }
    }

    fn ready(&self) -> bool {
        // SAFETY: reading renderer readiness.
        self.renderers
            .iter()
            .all(|r| unsafe { r.isReadyForMoreMediaData() })
    }

    fn enqueue(&self, frames: usize) -> Result<()> {
        let bytes = frames * CHANNELS as usize * size_of::<f32>();
        let mut block: *mut CMBlockBuffer = null_mut();
        // SAFETY: CoreMedia allocates the memory now (AssureMemoryNow); the
        // copy below fills exactly `bytes` of it from the mixer's block.
        let sample = unsafe {
            let status = CMBlockBuffer::create_with_memory_block(
                None,
                null_mut(),
                bytes,
                None,
                null(),
                0,
                bytes,
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut block),
            );
            let Some(block) = NonNull::new(block).filter(|_| status == 0) else {
                bail!("no memory for audio ({status})");
            };
            let block = CFRetained::from_raw(block);
            let source = NonNull::new(self.block.as_ptr().cast_mut().cast::<c_void>())
                .expect("a Vec's pointer is never null");
            let status = CMBlockBuffer::replace_data_bytes(source, &block, 0, bytes);
            if status != 0 {
                bail!("could not copy audio ({status})");
            }
            let mut sample: *mut CMSampleBuffer = null_mut();
            let status = CMAudioSampleBufferCreateReadyWithPacketDescriptions(
                None,
                &block,
                &self.format,
                frames as isize,
                CMTime::new(self.queued, RATE as i32),
                null(),
                NonNull::from(&mut sample),
            );
            let Some(sample) = NonNull::new(sample).filter(|_| status == 0) else {
                bail!("could not wrap audio ({status})");
            };
            CFRetained::from_raw(sample)
        };
        for renderer in &self.renderers {
            // SAFETY: the sample buffer is complete and stays retained.
            unsafe { renderer.enqueueSampleBuffer(&sample) };
        }
        Ok(())
    }
}

impl Drop for MacOutput {
    fn drop(&mut self) {
        // SAFETY: stopping the timeline and emptying the renderers.
        unsafe {
            self.synchronizer.setRate(0.0);
            for renderer in &self.renderers {
                renderer.flush();
            }
        }
    }
}

/// Reads one property of a CoreAudio object into `T`.
///
/// # Safety
/// `T` must be the type the property's data has.
unsafe fn property<T: Default>(object: AudioObjectID, selector: u32, scope: u32) -> Option<T> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut value = T::default();
    let mut size = size_of::<T>() as u32;
    // SAFETY: `value` has room for `size` bytes, which the caller vouches
    // is the property's type.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == 0).then_some(value)
}

/// A CFString property, as a Rust string.
fn string_property(object: AudioObjectID, selector: u32) -> Option<String> {
    // SAFETY: these properties hold a CFStringRef, returned at +1.
    let raw = unsafe { property::<usize>(object, selector, kAudioObjectPropertyScopeGlobal)? };
    let raw = NonNull::new(raw as *mut CFString)?;
    // SAFETY: taking over the +1 reference the property returned.
    let string = unsafe { CFRetained::from_raw(raw) };
    Some(string.to_string())
}

/// Output channels a device has.
fn output_channels(device: AudioObjectID) -> u32 {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = 0u32;
    // SAFETY: asking the size of the device's output stream layout.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            device,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 || (size as usize) < size_of::<AudioBufferList>() {
        return 0;
    }
    // u64 words keep the buffer list aligned for its pointer fields.
    let mut data = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: `data` has room for `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new(data.as_mut_ptr().cast()).expect("a Vec's pointer is never null"),
        )
    };
    if status != 0 {
        return 0;
    }
    let list = data.as_ptr().cast::<AudioBufferList>();
    // SAFETY: CoreAudio wrote an AudioBufferList of `mNumberBuffers`
    // buffers, all inside `size` bytes.
    unsafe {
        let count = (*list).mNumberBuffers as usize;
        let first = std::ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>();
        (0..count).map(|i| (*first.add(i)).mNumberChannels).sum()
    }
}

/// Output devices as (unique ID, name), for the second-output setting.
pub fn output_devices() -> Vec<(String, String)> {
    let system = kAudioObjectSystemObject as AudioObjectID;
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = 0u32;
    // SAFETY: asking how many devices there are.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            system,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    let mut ids = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
    if ids.is_empty() {
        return Vec::new();
    }
    // SAFETY: `ids` has room for `size` bytes of device IDs.
    let status = unsafe {
        AudioObjectGetPropertyData(
            system,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new(ids.as_mut_ptr().cast()).expect("a Vec's pointer is never null"),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.into_iter()
        .filter(|&id| output_channels(id) > 0)
        .filter_map(|id| {
            Some((
                string_property(id, kAudioDevicePropertyDeviceUID)?,
                string_property(id, kAudioObjectPropertyName)?,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Eq;

    #[test]
    #[ignore = "uses the audio device (silently)"]
    fn renderer_plays_in_time_and_lists_devices() {
        let devices = output_devices();
        println!(
            "{} output devices: {:?}",
            devices.len(),
            devices.iter().map(|d| &d.1).collect::<Vec<_>>()
        );
        assert!(!devices.is_empty());
        let mixer = Mutex::new(Mixer::new(
            Eq {
                enabled: false,
                gains: [0.0; 9],
            },
            1.0,
        ));
        let mut output = MacOutput::new(None, &mixer).expect("output");
        output.set_volume(0.0);
        output.play();
        let started = std::time::Instant::now();
        while started.elapsed().as_secs_f64() < 1.5 {
            output.pump(&mixer);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // SAFETY: reading the clock.
        let played = unsafe { output.synchronizer.currentTime().seconds() };
        println!(
            "played {played:.2}s in {:.2}s, {:.2}s queued ahead, {:.2}s latency",
            started.elapsed().as_secs_f64(),
            output.ahead(),
            output.latency
        );
        assert!(!output.failed());
        // The clock runs in real time once the device's latency has passed.
        let elapsed = started.elapsed().as_secs_f64();
        assert!((played + output.latency - elapsed).abs() < 0.15, "{played}");
        let block = BLOCK as f64 / f64::from(RATE);
        assert!(output.ahead() <= output.latency + AHEAD + block);
        // A skip: the queue goes, and the clock runs again from the top.
        output.flush();
        assert_eq!(output.ahead(), 0.0);
        let restarted = std::time::Instant::now();
        let mut first = None;
        while restarted.elapsed().as_secs_f64() < 1.0 {
            output.pump(&mixer);
            // SAFETY: reading the clock.
            let now = unsafe { output.synchronizer.currentTime().seconds() };
            if first.is_none() && now > 0.0 {
                first = Some(restarted.elapsed().as_secs_f64());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let first = first.expect("the clock ran again");
        println!("after a flush the clock ran again in {first:.3}s");
        // The clock follows what reaches the ears: Bluetooth adds ~0.25 s.
        assert!(first < 0.4, "{first}");
    }
}
