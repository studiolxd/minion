//! What the Mac itself is playing, so Minion can ignore hearing it.
//!
//! Netflix, a song, the other side of a call: all of it leaves the speakers
//! and comes straight back into the microphone. Today the only thing
//! standing between that and an obeyed command is the speaker check — which
//! costs an ECAPA embedding per utterance and, with headphones off and a
//! voice-like clip playing, is not a guarantee. The question this module
//! answers is the one the microphone cannot: *was that sound the Mac's own
//! output?*
//!
//! macOS 14.2 added CoreAudio process taps, which capture the system output
//! mix with no kernel extension and no screen-recording detour. The shape of
//! it is: build a [`CATapDescription`] for "everything, mono", create the tap,
//! wrap it in a private aggregate device whose sub-device is the current
//! output, and read that aggregate's *input* stream with an IOProc. What
//! arrives is exactly what the speakers are playing.
//!
//! Two decisions worth writing down:
//!
//! * `kAudioAggregateDeviceTapAutoStartKey` means the aggregate only runs
//!   while something is actually playing. Silence therefore arrives as *no
//!   callbacks at all*, not as blocks of zeros — which is why [`Ring`] pads
//!   the gap since the last write instead of assuming the buffer is
//!   continuous. It also makes a quiet Mac cost nothing.
//! * The comparison is on **envelopes**, not on samples. The microphone
//!   hears the room's version of the output — delayed, filtered, quieter,
//!   mixed with everything else — so a sample-level correlation would find
//!   nothing. The syllable-scale loudness contour survives all of that.
//!
//! Nothing here is on by default: `[audio] ignore_own_audio` turns it on.

use std::ffi::{c_void, CStr};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AllocAnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceUID, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioObjectUnknown, kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey,
    kAudioSubTapUIDKey, kAudioTapPropertyFormat, AudioDeviceCreateIOProcID,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior,
};
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use crate::audio::{Resampler, TARGET_HZ};

/// How much of the Mac's output is kept. Long enough that the whole of an
/// utterance — up to eight seconds — is still in it when the utterance
/// arrives, with room to spare for a late comparison.
const BUFFER_SECONDS: usize = 12;
const BUFFER_SAMPLES: usize = BUFFER_SECONDS * TARGET_HZ as usize;

/// The envelope's resolution: one loudness figure per 20 ms, the same
/// analysis block the segmenter uses.
const FRAME_SAMPLES: usize = TARGET_HZ as usize / 50;

/// Quietest frame the envelope distinguishes. Below this everything is
/// "silence", so a silent stretch is flat rather than a noise pattern the
/// correlation could latch onto.
const ENVELOPE_FLOOR: f32 = 1e-4;

/// Below this average level the Mac was not really playing anything, and
/// there is nothing to compare against. Roughly −60 dBFS.
const OUTPUT_SILENCE: f32 = 1e-3;

/// How far either side of the expected position the utterance is looked
/// for, in envelope frames. The microphone path (preroll, blocking, the
/// segmenter's own tail) and the output tap do not share a clock, so the
/// alignment is known only to within a few hundred milliseconds — 20 frames
/// is 400 ms, comfortably more than the 0–300 ms the two paths differ by.
const SEARCH_FRAMES: isize = 20;

/// The default `own_audio_threshold`: how alike is alike enough.
///
/// Measured on this machine against real output taps (three ten-second
/// captures: two `say` voices, and a run of system alert sounds), with the
/// acoustic path back to the microphone simulated — attenuated to 2–20 %,
/// band-limited, 0–300 ms late, with room noise added.
///
/// * The same audio, heard back: **0.71 – 0.95** (worst case being the
///   quietest speaker with the noisiest room).
/// * Different audio, from a different voice at a different time:
///   **0.00 – 0.42**, and often no answer at all because the Mac was silent.
///
/// 0.6 sits in that gap, above every negative and below every positive,
/// and nearer the negatives than the middle: a wrong "own" costs a command
/// silently ignored, which is worse than one more trip through the speaker
/// check.
pub const DEFAULT_THRESHOLD: f32 = 0.6;

/// Below this the output is quiet — roughly −46 dBFS, well under a spoken
/// notification but well above the tap's own noise floor.
pub const QUIET_RMS: f32 = 0.005;

/// At or above this the output is loud enough to be a chime, a word, or
/// music — not room tone bleeding into the tap.
pub const LOUD_RMS: f32 = 0.02;

/// How long the output must stay quiet before a return to loud counts as an
/// onset, rather than the ordinary rise and fall inside one sound.
const QUIET_HOLD: Duration = Duration::from_millis(300);

/// Tells a notification chime apart from music playing through the Mac's own
/// speakers: a chime is a quiet-to-loud *onset*, at the same instant a
/// microphone utterance opens; music is steady loud output throughout.
///
/// Silero cannot make this distinction — a chime scores as speech as often
/// as the owner's own voice does — but timing against the Mac's own output
/// can, since only the microphone's utterance and the output's onset need to
/// line up, not their content.
struct OnsetTracker {
    /// When the output became continuously quiet, if it has been for at
    /// least [`QUIET_HOLD`]. `None` while loud, or not yet quiet long
    /// enough for a later loud block to count as an onset.
    quiet_since: Option<Instant>,
    /// Whether the last block observed was loud, so a loud block right
    /// after another loud block is not mistaken for a fresh onset.
    was_loud: bool,
    /// The instant of the most recent onset, if there has been one.
    last_onset: Option<Instant>,
}

impl OnsetTracker {
    fn new() -> Self {
        Self { quiet_since: None, was_loud: false, last_onset: None }
    }

    /// Feeds one output block's loudness.
    ///
    /// A block between [`QUIET_RMS`] and [`LOUD_RMS`] is neither quiet nor
    /// loud: it breaks a quiet streak in progress (a blip too soft to be an
    /// onset should not let a shorter gap than [`QUIET_HOLD`] still count),
    /// but is not loud enough to raise an onset on its own.
    fn observe(&mut self, rms: f32, now: Instant) {
        if rms < QUIET_RMS {
            if self.quiet_since.is_none() {
                self.quiet_since = Some(now);
            }
            self.was_loud = false;
            return;
        }

        if rms >= LOUD_RMS {
            let quiet_long_enough = self
                .quiet_since
                .is_some_and(|since| now.saturating_duration_since(since) >= QUIET_HOLD);
            if quiet_long_enough && !self.was_loud {
                self.last_onset = Some(now);
            }
            self.was_loud = true;
        }
        self.quiet_since = None;
    }

    /// Whether an onset happened within `window` of `now`.
    fn onset_within(&self, window: Duration, now: Instant) -> bool {
        self.last_onset
            .is_some_and(|at| now.saturating_duration_since(at) <= window)
    }
}

/// The tracker for whichever [`Loopback`] tap is currently running, or
/// `None` when there is none — in which case [`output_onset_within`]
/// answers `false` rather than blocking anything on a tap that is not there.
static ONSET: Mutex<Option<OnsetTracker>> = Mutex::new(None);

/// Whether the Mac's output had a quiet-to-loud onset within `window` of
/// now — a chime, most likely, arriving at the same instant a microphone
/// utterance opened. `false` when the tap is not running.
pub fn output_onset_within(window: Duration) -> bool {
    ONSET
        .lock()
        .is_ok_and(|tracker| tracker.as_ref().is_some_and(|t| t.onset_within(window, Instant::now())))
}

/// A live process tap on the system output, and the recent history it fills.
///
/// Dropping it tears down the IOProc, the aggregate device and the tap, in
/// that order — CoreAudio objects outlive the process that made them if
/// they are not destroyed, and a leaked private aggregate shows up in
/// Audio MIDI Setup.
pub struct Loopback {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    io_proc: AudioDeviceIOProcID,
    /// The IOProc holds a second, raw-pointer reference to this; reclaimed
    /// in [`Drop`] once the IOProc can no longer run.
    capture: Arc<Mutex<Capture>>,
    /// What the tap said it would deliver, for `minion loopback` to print.
    pub source_hz: u32,
}

/// The rolling history and the resampler that fills it, under one lock:
/// the IOProc touches both, and so does the reader.
struct Capture {
    ring: Ring,
    resampler: Resampler,
}

/// The last [`BUFFER_SECONDS`] of output, at 16 kHz, as a circular buffer.
///
/// The tap only fires while audio is playing, so "how much time has passed"
/// is not "how many samples have arrived". Every write first pads the gap
/// since the previous one with silence, which keeps the buffer a true
/// timeline: the last sample is always *now*.
struct Ring {
    samples: Vec<f32>,
    cursor: usize,
    last_write: Instant,
}

impl Ring {
    fn new() -> Self {
        Self {
            samples: vec![0.0; BUFFER_SAMPLES],
            cursor: 0,
            last_write: Instant::now(),
        }
    }

    fn write(&mut self, block: &[f32]) {
        for &sample in block {
            self.samples[self.cursor] = sample;
            self.cursor = (self.cursor + 1) % BUFFER_SAMPLES;
        }
    }

    /// Fills in the silence between the last write and `now`, minus the
    /// `pending` samples that are about to be written for that same time.
    fn catch_up(&mut self, now: Instant, pending: usize) {
        let elapsed = now.saturating_duration_since(self.last_write);
        let expected = (elapsed.as_secs_f64() * f64::from(TARGET_HZ)) as usize;
        let gap = expected.saturating_sub(pending).min(BUFFER_SAMPLES);
        for _ in 0..gap {
            self.samples[self.cursor] = 0.0;
            self.cursor = (self.cursor + 1) % BUFFER_SAMPLES;
        }
        self.last_write = now;
    }

    /// The buffer oldest-first, ending at `now`.
    fn snapshot(&mut self, now: Instant) -> Vec<f32> {
        self.catch_up(now, 0);
        let mut out = Vec::with_capacity(BUFFER_SAMPLES);
        out.extend_from_slice(&self.samples[self.cursor..]);
        out.extend_from_slice(&self.samples[..self.cursor]);
        out
    }
}

fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Turns an `OSStatus` into something a log line can carry: CoreAudio's
/// codes are four-character constants far more often than they are numbers.
fn status_error(what: &str, status: i32) -> anyhow::Error {
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic()) {
        anyhow!("{what} failed: '{}' ({status})", String::from_utf8_lossy(&bytes))
    } else {
        anyhow!("{what} failed: {status}")
    }
}

/// The UID of the output device the Mac is playing through right now.
fn default_output_uid() -> Result<String> {
    let mut device: AudioObjectID = kAudioObjectUnknown;
    let mut addr = address(kAudioHardwarePropertyDefaultOutputDevice);
    let mut size = u32::try_from(size_of::<AudioObjectID>()).unwrap_or(4);
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked((&raw mut device).cast::<c_void>()),
        )
    };
    if status != 0 {
        return Err(status_error("reading the default output device", status));
    }

    let mut uid: CFStringRef = std::ptr::null();
    let mut addr = address(kAudioDevicePropertyDeviceUID);
    let mut size = u32::try_from(size_of::<CFStringRef>()).unwrap_or(8);
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked((&raw mut uid).cast::<c_void>()),
        )
    };
    if status != 0 || uid.is_null() {
        return Err(status_error("reading the output device UID", status));
    }
    // Copied out of a "Get" that follows the Create rule, so it is ours.
    Ok(unsafe { CFString::wrap_under_create_rule(uid) }.to_string())
}

/// The rate and channel count the tap will deliver.
fn tap_format(tap: AudioObjectID) -> Result<(u32, usize)> {
    // Zeroed, then filled in by CoreAudio: the struct is a plain C record
    // of numbers, with no niches and no invalid bit patterns.
    let mut format: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
    let mut addr = address(kAudioTapPropertyFormat);
    let mut size = u32::try_from(size_of::<AudioStreamBasicDescription>()).unwrap_or(40);
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked((&raw mut format).cast::<c_void>()),
        )
    };
    if status != 0 {
        return Err(status_error("reading the tap format", status));
    }
    let rate = format.mSampleRate as u32;
    if rate == 0 {
        return Err(anyhow!("the tap reported a sample rate of zero"));
    }
    Ok((rate, format.mChannelsPerFrame.max(1) as usize))
}

fn ns(key: &CStr) -> Retained<NSString> {
    NSString::from_str(&key.to_string_lossy())
}

/// Builds the aggregate device description.
///
/// It is written as an `NSDictionary` and handed over as a `CFDictionary`:
/// the two are the same object, and Foundation's collections are far less
/// ceremony to build than Core Foundation's.
fn aggregate_description(uid: &str, tap_uuid: &str, aggregate_uid: &str) -> Retained<NSDictionary<NSString, AnyObject>> {
    let device_uid = NSString::from_str(uid);
    let sub_device: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_retained_objects(
        &[&*ns(kAudioSubDeviceUIDKey)],
        &[unsafe { Retained::cast_unchecked::<AnyObject>(device_uid.clone()) }],
    );
    let sub_tap: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_retained_objects(
        &[&*ns(kAudioSubTapUIDKey), &*ns(kAudioSubTapDriftCompensationKey)],
        &[
            unsafe { Retained::cast_unchecked::<AnyObject>(NSString::from_str(tap_uuid)) },
            unsafe { Retained::cast_unchecked::<AnyObject>(NSNumber::new_bool(true)) },
        ],
    );

    let keys = [
        ns(kAudioAggregateDeviceNameKey),
        ns(kAudioAggregateDeviceUIDKey),
        ns(kAudioAggregateDeviceMainSubDeviceKey),
        ns(kAudioAggregateDeviceIsPrivateKey),
        ns(kAudioAggregateDeviceIsStackedKey),
        ns(kAudioAggregateDeviceTapAutoStartKey),
        ns(kAudioAggregateDeviceSubDeviceListKey),
        ns(kAudioAggregateDeviceTapListKey),
    ];
    let values: [Retained<AnyObject>; 8] = unsafe {
        [
            Retained::cast_unchecked(NSString::from_str("Minion Loopback")),
            Retained::cast_unchecked(NSString::from_str(aggregate_uid)),
            Retained::cast_unchecked(NSString::from_str(uid)),
            Retained::cast_unchecked(NSNumber::new_bool(true)),
            Retained::cast_unchecked(NSNumber::new_bool(false)),
            Retained::cast_unchecked(NSNumber::new_bool(true)),
            Retained::cast_unchecked(NSArray::from_retained_slice(&[sub_device])),
            Retained::cast_unchecked(NSArray::from_retained_slice(&[sub_tap])),
        ]
    };
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    NSDictionary::from_retained_objects(&key_refs, &values)
}

/// The IOProc: called on CoreAudio's real-time thread with whatever the
/// speakers just played.
///
/// It resamples to 16 kHz and appends. The lock it takes is held for the
/// length of a memcpy and contended at most once per utterance, which is
/// the cheapest arrangement that keeps one resampler (and so one filter
/// history) rather than allocating a new one per block.
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client_data: *mut c_void,
) -> i32 {
    if client_data.is_null() {
        return 0;
    }
    let capture = unsafe { &*(client_data.cast::<Mutex<Capture>>()) };
    let list = unsafe { input.as_ref() };
    if list.mNumberBuffers == 0 {
        return 0;
    }
    let buffer = list.mBuffers[0];
    if buffer.mData.is_null() || buffer.mDataByteSize == 0 {
        return 0;
    }
    let count = buffer.mDataByteSize as usize / size_of::<f32>();
    let samples = unsafe { std::slice::from_raw_parts(buffer.mData.cast::<f32>(), count) };
    let channels = buffer.mNumberChannels.max(1) as usize;

    let Ok(mut capture) = capture.lock() else {
        return 0;
    };
    let capture = &mut *capture;
    let resampled = capture.resampler.process(samples, channels);
    let now = Instant::now();
    capture.ring.catch_up(now, resampled.len());
    let block: Vec<f32> = resampled.to_vec();
    capture.ring.write(&block);

    let level = (block.iter().map(|s| s * s).sum::<f32>() / block.len().max(1) as f32).sqrt();
    if let Ok(mut tracker) = ONSET.lock() {
        if let Some(tracker) = tracker.as_mut() {
            tracker.observe(level, now);
        }
    }
    0
}

impl Loopback {
    /// Opens a tap on everything the Mac plays.
    ///
    /// Fails, rather than degrading quietly, if any step of it does: a
    /// loopback that silently records nothing would mean every utterance
    /// scores zero and the setting appears to work while doing nothing.
    pub fn start() -> Result<Self> {
        let uid = default_output_uid()?;

        let description = unsafe {
            let empty = NSArray::<NSNumber>::from_retained_slice(&[]);
            let description =
                CATapDescription::initMonoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &empty);
            // Private: nobody else's Audio MIDI Setup needs to see it.
            // Unmuted: tapping the output must not silence it.
            description.setPrivate(true);
            description.setMuteBehavior(CATapMuteBehavior::Unmuted);
            description.setName(&NSString::from_str("Minion"));
            description
        };
        let tap_uuid = unsafe { description.UUID().UUIDString() }.to_string();

        let mut tap: AudioObjectID = kAudioObjectUnknown;
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &raw mut tap) };
        if status != 0 {
            return Err(status_error("AudioHardwareCreateProcessTap", status));
        }

        let aggregate_uid = format!("com.studiolxd.minion.loopback.{}", std::process::id());
        let plan = aggregate_description(&uid, &tap_uuid, &aggregate_uid);
        let mut aggregate: AudioObjectID = kAudioObjectUnknown;
        // NSDictionary and CFDictionary are the same object; the binding for
        // `AudioHardwareCreateAggregateDevice` names the Core Foundation one.
        let plan_ref = unsafe { &*(Retained::as_ptr(&plan).cast::<CFDictionary>()) };
        let status =
            unsafe { AudioHardwareCreateAggregateDevice(plan_ref, NonNull::from(&mut aggregate)) };
        if status != 0 {
            unsafe { AudioHardwareDestroyProcessTap(tap) };
            return Err(status_error("AudioHardwareCreateAggregateDevice", status));
        }

        let (source_hz, _channels) = match tap_format(tap) {
            Ok(format) => format,
            Err(e) => {
                unsafe { AudioHardwareDestroyAggregateDevice(aggregate) };
                unsafe { AudioHardwareDestroyProcessTap(tap) };
                return Err(e);
            }
        };

        let capture = Arc::new(Mutex::new(Capture {
            ring: Ring::new(),
            resampler: Resampler::new(source_hz),
        }));
        // The IOProc's own reference, as a raw pointer. Reclaimed in `drop`,
        // after the IOProc has been destroyed and so can no longer read it.
        let client_data = Arc::into_raw(Arc::clone(&capture)).cast_mut().cast::<c_void>();

        let mut io_proc_id: AudioDeviceIOProcID = None;
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                aggregate,
                Some(io_proc),
                client_data,
                NonNull::from(&mut io_proc_id),
            )
        };
        if status != 0 {
            unsafe { drop(Arc::from_raw(client_data.cast::<Mutex<Capture>>())) };
            unsafe { AudioHardwareDestroyAggregateDevice(aggregate) };
            unsafe { AudioHardwareDestroyProcessTap(tap) };
            return Err(status_error("AudioDeviceCreateIOProcID", status));
        }

        let status = unsafe { AudioDeviceStart(aggregate, io_proc_id) };
        if status != 0 {
            unsafe { AudioDeviceDestroyIOProcID(aggregate, io_proc_id) };
            unsafe { drop(Arc::from_raw(client_data.cast::<Mutex<Capture>>())) };
            unsafe { AudioHardwareDestroyAggregateDevice(aggregate) };
            unsafe { AudioHardwareDestroyProcessTap(tap) };
            return Err(status_error("AudioDeviceStart", status));
        }

        if let Ok(mut tracker) = ONSET.lock() {
            *tracker = Some(OnsetTracker::new());
        }

        Ok(Self {
            tap,
            aggregate,
            io_proc: io_proc_id,
            capture,
            source_hz,
        })
    }

    /// The last [`BUFFER_SECONDS`] the Mac played, oldest first, ending now.
    pub fn recent(&self) -> Vec<f32> {
        match self.capture.lock() {
            Ok(mut capture) => capture.ring.snapshot(Instant::now()),
            Err(_) => Vec::new(),
        }
    }

    /// How much like the Mac's own output an utterance sounds.
    ///
    /// `late` is how long ago the utterance was handed over, which is where
    /// in the output history to start looking. `None` means the Mac was not
    /// playing anything then, so the question does not arise.
    pub fn resemblance(&self, utterance: &[f32], late: Duration) -> Option<f32> {
        resemblance_in(&self.recent(), utterance, late)
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        if let Ok(mut tracker) = ONSET.lock() {
            *tracker = None;
        }
        unsafe {
            AudioDeviceStop(self.aggregate, self.io_proc);
            AudioDeviceDestroyIOProcID(self.aggregate, self.io_proc);
            AudioHardwareDestroyAggregateDevice(self.aggregate);
            AudioHardwareDestroyProcessTap(self.tap);
            // The IOProc is gone, so nothing else holds this pointer.
            drop(Arc::from_raw(Arc::as_ptr(&self.capture)));
        }
    }
}

/// One loudness figure per 20 ms, on a log scale.
///
/// Log, because the microphone hears the room's much quieter copy of what
/// the speakers played: on a linear scale the quiet half of a phrase would
/// weigh almost nothing, and the correlation would be decided by its loudest
/// syllable alone.
fn envelope(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(FRAME_SAMPLES)
        .map(|frame| {
            let power: f32 = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
            power.sqrt().max(ENVELOPE_FLOOR).ln()
        })
        .collect()
}

/// Pearson's correlation: how alike two shapes are, whatever their level.
fn correlation(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let n = a.len() as f32;
    let mean_a = a.iter().sum::<f32>() / n;
    let mean_b = b.iter().sum::<f32>() / n;
    let mut covariance = 0.0;
    let mut variance_a = 0.0;
    let mut variance_b = 0.0;
    for (x, y) in a.iter().zip(b) {
        let dx = x - mean_a;
        let dy = y - mean_b;
        covariance += dx * dy;
        variance_a += dx * dx;
        variance_b += dy * dy;
    }
    let spread = (variance_a * variance_b).sqrt();
    if spread <= f32::EPSILON {
        // One of the two is flat — silence, most often. Nothing to match.
        return 0.0;
    }
    covariance / spread
}

/// The comparison itself, with no CoreAudio anywhere near it.
///
/// `output` is the rolling buffer, oldest first, ending now; `utterance` is
/// what the microphone heard, ending `late` ago. The utterance therefore
/// sits at a known place in the output — give or take the few hundred
/// milliseconds the two paths differ by, which is what the search covers.
///
/// `None` when the Mac played nothing worth comparing against.
pub fn resemblance_in(output: &[f32], utterance: &[f32], late: Duration) -> Option<f32> {
    let heard = envelope(utterance);
    let played = envelope(output);
    if heard.len() < 4 || played.len() <= heard.len() {
        return None;
    }

    let late_frames = (late.as_secs_f32() * 1000.0 / 20.0).round() as isize;
    let expected = played.len() as isize - heard.len() as isize - late_frames;
    let first = (expected - SEARCH_FRAMES).max(0);
    let last = (expected + SEARCH_FRAMES).min(played.len() as isize - heard.len() as isize);
    if last < first {
        return None;
    }

    // Was anything playing at all over the stretch being compared? Asked on
    // the samples rather than the envelope, since the envelope has a floor.
    let from = (first as usize) * FRAME_SAMPLES;
    let to = ((last as usize + heard.len()) * FRAME_SAMPLES).min(output.len());
    let window = &output[from.min(to)..to];
    if window.is_empty() {
        return None;
    }
    let level = (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt();
    if level < OUTPUT_SILENCE {
        return None;
    }

    let mut best = 0.0_f32;
    for start in first..=last {
        let at = start as usize;
        let score = correlation(&heard, &played[at..at + heard.len()]);
        best = best.max(score);
    }
    Some(best)
}

/// `minion loopback`: proof, in five seconds, that the tap works.
pub fn probe(seconds: u64) -> Result<()> {
    let loopback = Loopback::start()?;
    println!(
        "Escuchando la salida del Mac durante {seconds} s ({} Hz en el grifo, \
         remuestreado a {} Hz). Pon música o di algo con «say».",
        loopback.source_hz, TARGET_HZ
    );
    for second in 1..=seconds {
        std::thread::sleep(Duration::from_secs(1));
        let recent = loopback.recent();
        let tail = recent.len().saturating_sub(TARGET_HZ as usize);
        let last_second = &recent[tail..];
        let level = (last_second.iter().map(|s| s * s).sum::<f32>() / last_second.len() as f32).sqrt();
        let peak = last_second.iter().fold(0.0_f32, |max, s| max.max(s.abs()));
        let onset = if output_onset_within(Duration::from_millis(400)) { "yes" } else { "no" };
        println!("{second} s: RMS {level:.5}  pico {peak:.5}  onset: {onset}");
    }
    println!("Listo. Un RMS de 0.00000 con el sonido puesto significa que el grifo no recibe nada.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A crude stand-in for speech: bursts of noise of varying length and
    /// loudness, separated by pauses.
    ///
    /// Deliberately *not* periodic. A steady four-syllables-a-second train
    /// correlates with itself at every lag, so it would say more about the
    /// generator than about the measure.
    fn speech_like(seed: u32, seconds: f32) -> Vec<f32> {
        let total = (seconds * TARGET_HZ as f32) as usize;
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 16) as f32 / f32::from(u16::MAX)
        };

        let mut out = Vec::with_capacity(total);
        while out.len() < total {
            // A syllable of 80–280 ms at some level, then 40–240 ms of quiet.
            let syllable = (0.08 + next() * 0.2) * TARGET_HZ as f32;
            let level = 0.05 + next() * 0.3;
            for i in 0..syllable as usize {
                let shape = (std::f32::consts::PI * i as f32 / syllable).sin();
                out.push((next() * 2.0 - 1.0) * level * shape);
            }
            let pause = (0.04 + next() * 0.2) * TARGET_HZ as f32;
            out.extend(std::iter::repeat_n(0.0, pause as usize));
        }
        out.truncate(total);
        out
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * TARGET_HZ as f32) as usize]
    }

    #[test]
    fn the_same_sound_quieter_and_delayed_still_matches() {
        let played = speech_like(3, 2.0);
        // What the microphone hears: a tenth of the level, 100 ms late, on
        // top of a little room noise.
        let heard: Vec<f32> = played.iter().map(|s| s * 0.1).collect();

        let mut output = silence(8.0);
        output.extend_from_slice(&played);
        output.extend_from_slice(&silence(2.0));

        // The utterance ended two seconds ago, matching the trailing silence.
        let score = resemblance_in(&output, &heard, Duration::from_millis(2_000)).unwrap();
        assert!(score > 0.9, "same sound scored {score}");
    }

    #[test]
    fn a_different_sound_does_not_match() {
        let played = speech_like(3, 2.0);
        let other = speech_like(17, 2.0);

        let mut output = silence(8.0);
        output.extend_from_slice(&played);
        output.extend_from_slice(&silence(2.0));

        let score = resemblance_in(&output, &other, Duration::from_millis(2_000)).unwrap();
        assert!(score < DEFAULT_THRESHOLD, "different sounds scored {score}");
    }

    #[test]
    fn a_silent_mac_has_nothing_to_say() {
        let output = silence(12.0);
        let heard = speech_like(5, 1.5);
        assert_eq!(resemblance_in(&output, &heard, Duration::ZERO), None);
    }

    #[test]
    fn an_utterance_longer_than_the_history_is_not_judged() {
        let output = speech_like(2, 1.0);
        let heard = speech_like(2, 2.0);
        assert_eq!(resemblance_in(&output, &heard, Duration::ZERO), None);
    }

    #[test]
    fn alignment_is_searched_not_assumed() {
        let played = speech_like(7, 2.0);
        let heard: Vec<f32> = played.iter().map(|s| s * 0.2).collect();
        let mut output = silence(8.0);
        output.extend_from_slice(&played);
        output.extend_from_slice(&silence(2.0));

        // Told the utterance ended 2.0 s ago when it really ended 2.25 s
        // ago: within the 400 ms the search covers, so still found.
        let score = resemblance_in(&output, &heard, Duration::from_millis(2_250)).unwrap();
        assert!(score > 0.9, "a 250 ms error cost the match: {score}");
    }

    #[test]
    fn silence_then_loud_is_an_onset() {
        let mut tracker = OnsetTracker::new();
        let start = Instant::now();
        tracker.observe(0.0, start);
        tracker.observe(0.0, start + Duration::from_millis(300));
        tracker.observe(0.03, start + Duration::from_millis(320));
        assert!(tracker.onset_within(Duration::from_millis(400), start + Duration::from_millis(320)));
    }

    #[test]
    fn steady_loud_output_is_not_an_onset() {
        let mut tracker = OnsetTracker::new();
        let start = Instant::now();
        for ms in (0..1_000).step_by(20) {
            tracker.observe(0.05, start + Duration::from_millis(ms));
        }
        assert!(!tracker.onset_within(Duration::from_millis(400), start + Duration::from_millis(1_000)));
    }

    #[test]
    fn steady_quiet_is_not_an_onset() {
        let mut tracker = OnsetTracker::new();
        let start = Instant::now();
        for ms in (0..1_000).step_by(20) {
            tracker.observe(0.0, start + Duration::from_millis(ms));
        }
        assert!(!tracker.onset_within(Duration::from_millis(400), start + Duration::from_millis(1_000)));
    }

    #[test]
    fn a_quiet_gap_after_loud_output_is_an_onset_again() {
        let mut tracker = OnsetTracker::new();
        let start = Instant::now();
        tracker.observe(0.05, start);
        tracker.observe(0.0, start + Duration::from_millis(20));
        tracker.observe(0.0, start + Duration::from_millis(320));
        tracker.observe(0.05, start + Duration::from_millis(340));
        assert!(tracker.onset_within(Duration::from_millis(400), start + Duration::from_millis(340)));
    }

    #[test]
    fn a_short_blip_below_loud_does_not_shorten_the_quiet_hold() {
        // A stretch that would otherwise satisfy the 300 ms quiet hold, but
        // is interrupted partway by a blip that never reaches LOUD_RMS: the
        // hold must restart, so the loud block right after is not an onset.
        let mut tracker = OnsetTracker::new();
        let start = Instant::now();
        tracker.observe(0.0, start);
        tracker.observe(0.0, start + Duration::from_millis(200));
        tracker.observe(0.01, start + Duration::from_millis(220)); // blip, below LOUD_RMS
        tracker.observe(0.03, start + Duration::from_millis(240));
        assert!(
            !tracker.onset_within(Duration::from_millis(400), start + Duration::from_millis(240)),
            "the blip should have reset the quiet streak"
        );
    }

    #[test]
    fn an_onset_expires_outside_the_window() {
        let mut tracker = OnsetTracker::new();
        let start = Instant::now();
        tracker.observe(0.0, start);
        tracker.observe(0.0, start + Duration::from_millis(300));
        tracker.observe(0.03, start + Duration::from_millis(320));
        assert!(!tracker.onset_within(Duration::from_millis(400), start + Duration::from_millis(1_000)));
    }

    #[test]
    fn no_running_tap_has_no_onset() {
        assert!(!output_onset_within(Duration::from_millis(400)));
    }

    #[test]
    fn the_ring_pads_the_silence_the_tap_does_not_send() {
        let mut ring = Ring::new();
        let start = ring.last_write;
        let block = vec![1.0_f32; 160]; // 10 ms
        ring.catch_up(start + Duration::from_millis(10), block.len());
        ring.write(&block);
        // A second of nothing, then another block: the gap must be silence.
        ring.catch_up(start + Duration::from_millis(1_010), block.len());
        ring.write(&block);

        let taken = ring.snapshot(start + Duration::from_millis(1_010));
        let ones = taken.iter().filter(|s| **s > 0.5).count();
        assert_eq!(ones, 320, "both blocks should be there, and nothing else");
        // The two blocks are a second apart, not touching.
        let first = taken.iter().position(|s| *s > 0.5).unwrap();
        let last = taken.iter().rposition(|s| *s > 0.5).unwrap();
        assert!(last - first > TARGET_HZ as usize - 400, "the gap was not padded");
    }
}
