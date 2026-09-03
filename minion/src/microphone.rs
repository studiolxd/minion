//! Who else is capturing the microphone right now.
//!
//! Minion listens all the time, which is exactly what nobody wants during a
//! video call. The old answer was a list of bundle IDs to pause in front of,
//! which is a guess dressed up as a signal: Teams in front while reading a
//! chat is not a call, and a call in a background window is one. macOS 14
//! made the real signal readable — CoreAudio publishes one object per
//! process that has touched audio, and each of those says whether it is
//! running an input stream right now. That is the question being asked here:
//! *is another process recording?*, not *which app is in front?*.
//!
//! Nothing in here may be called from the audio callback: reading a
//! CoreAudio property talks to the HAL, which can block. It is called from
//! the idle tick (every ~250 ms) and the answer is cached for a second, so a
//! busy tick costs one property read per process at most once per second.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use objc2_core_audio::{
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioProcessPropertyBundleID,
    kAudioProcessPropertyIsRunningInput, kAudioProcessPropertyPID, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectHasProperty, AudioObjectID,
    AudioObjectPropertyAddress,
};

/// How long an answer is reused before asking CoreAudio again.
const CACHE_FOR: Duration = Duration::from_secs(1);

/// A process that is recording from an input device right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub pid: i32,
    /// Absent for a process CoreAudio knows only by pid — a command-line
    /// tool, or one that has gone away between the two reads.
    pub bundle_id: Option<String>,
}

impl std::fmt::Display for Capture {
    /// What the log and `minion mic` call it: the bundle ID if there is
    /// one, since that is the name the user would recognise.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.bundle_id {
            Some(id) => write!(f, "{id} (pid {})", self.pid),
            None => write!(f, "pid {}", self.pid),
        }
    }
}

/// A value worth reusing for a moment, with the moment it was taken.
///
/// Pure, so the "don't ask CoreAudio sixty times a second" rule can be
/// tested without CoreAudio: [`Instant`]s go in and out, nothing is read
/// from a clock in here.
struct Cache<T> {
    ttl: Duration,
    entry: Option<(Instant, T)>,
}

impl<T: Clone> Cache<T> {
    const fn new(ttl: Duration) -> Self {
        Self { ttl, entry: None }
    }

    /// The remembered value, if it was taken recently enough.
    fn get(&self, now: Instant) -> Option<T> {
        self.entry.as_ref().and_then(|(taken, value)| {
            (now.duration_since(*taken) < self.ttl).then(|| value.clone())
        })
    }

    fn put(&mut self, now: Instant, value: T) {
        self.entry = Some((now, value));
    }
}

/// The last answer, so the 250 ms tick does not become a 250 ms HAL query.
static CACHE: Mutex<Cache<Option<Vec<Capture>>>> = Mutex::new(Cache::new(CACHE_FOR));

/// The address of a property on an object, at global scope.
fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Whether this macOS knows about audio process objects at all.
///
/// The property arrived in macOS 14.0; on 13 the HAL simply does not have
/// it, and every caller has to fall back to doing nothing rather than
/// reading "nobody is recording" into a question that was never answered.
pub fn available() -> bool {
    let mut want = address(kAudioHardwarePropertyProcessObjectList);
    // Safety: a stack address that outlives the call, and the system object
    // is always a valid object ID.
    unsafe {
        AudioObjectHasProperty(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut want),
        )
    }
}

/// Read a fixed-size property off an object.
fn read<T: Default>(object: AudioObjectID, selector: u32) -> Option<T> {
    let mut want = address(selector);
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    // Safety: `value` is one `T`, `size` says so, and both pointers are to
    // live stack slots.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut want),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(std::ptr::from_mut(&mut value).cast::<c_void>())?,
        )
    };
    (status == 0).then_some(value)
}

/// The bundle ID of a process object, if CoreAudio has one for it.
fn bundle_id(process: AudioObjectID) -> Option<String> {
    // The header says the caller owns the returned CFString, so it is
    // wrapped under the create rule and released with the wrapper.
    let raw: CFStringRef = read(process, kAudioProcessPropertyBundleID)?;
    if raw.is_null() {
        return None;
    }
    // Safety: a CFString the HAL just created for us, owned from here on.
    let string = unsafe { CFString::wrap_under_create_rule(raw) }.to_string();
    (!string.is_empty()).then_some(string)
}

/// Every process object the HAL currently knows about.
fn process_objects() -> Option<Vec<AudioObjectID>> {
    if !available() {
        return None;
    }
    let mut want = address(kAudioHardwarePropertyProcessObjectList);
    let mut bytes: u32 = 0;
    // Safety: both pointers are to live stack slots; no qualifier is needed
    // for this property.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut want),
            0,
            std::ptr::null(),
            NonNull::from(&mut bytes),
        )
    };
    if status != 0 {
        return None;
    }
    let count = bytes as usize / std::mem::size_of::<AudioObjectID>();
    let mut objects: Vec<AudioObjectID> = vec![0; count];
    if count == 0 {
        return Some(objects);
    }
    // Safety: the buffer holds exactly `bytes` bytes, which is what the HAL
    // was just asked for and what `bytes` still says.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut want),
            0,
            std::ptr::null(),
            NonNull::from(&mut bytes),
            NonNull::new(objects.as_mut_ptr().cast::<c_void>())?,
        )
    };
    if status != 0 {
        return None;
    }
    objects.truncate(bytes as usize / std::mem::size_of::<AudioObjectID>());
    Some(objects)
}

/// Ask CoreAudio, without the cache. Every process recording right now
/// except this one.
fn capturing_now(own_pid: i32) -> Option<Vec<Capture>> {
    let objects = process_objects()?;
    let mut capturing = Vec::new();
    for process in objects {
        // A UInt32, 1 while the process is running an input stream.
        if read::<u32>(process, kAudioProcessPropertyIsRunningInput) != Some(1) {
            continue;
        }
        let pid: i32 = read(process, kAudioProcessPropertyPID).unwrap_or(-1);
        if pid == own_pid {
            continue;
        }
        capturing.push(Capture { pid, bundle_id: bundle_id(process) });
    }
    Some(capturing)
}

/// Every *other* process capturing from an input device, or `None` when
/// this macOS cannot say (13 and older).
///
/// Cached for a second: the caller polls four times a second, and a
/// meeting that started 800 ms ago is not late news.
pub fn others_capturing() -> Option<Vec<Capture>> {
    let own_pid = std::process::id() as i32;
    let now = Instant::now();
    let mut cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(remembered) = cache.get(now) {
        return remembered;
    }
    let fresh = capturing_now(own_pid);
    cache.put(now, fresh.clone());
    fresh
}

/// Whether any other process is capturing the microphone right now.
///
/// `None` means the question could not be asked — macOS 13 or older.
pub fn in_use_by_others() -> Option<bool> {
    others_capturing().map(|capturing| !capturing.is_empty())
}

/// `minion mic`: what is recording, right now, on this machine.
pub fn report() {
    let Some(capturing) = others_capturing() else {
        println!(
            "Este macOS no puede decir qué aplicaciones usan el micrófono \
             (hace falta macOS 14 o posterior)."
        );
        return;
    };
    if capturing.is_empty() {
        println!("Ninguna otra aplicación está usando el micrófono.");
        return;
    }
    println!("Usando el micrófono ahora mismo:");
    for capture in capturing {
        match capture.bundle_id {
            Some(id) => println!("  {id}  (pid {})", capture.pid),
            None => println!("  (sin bundle ID)  (pid {})", capture.pid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The selectors are taken from the framework bindings rather than
    // typed out, so nothing here is needed at run time — but the headers
    // write them as four-character codes ('piri'), and these two turn a
    // code into a number and back so the tests below can check the
    // bindings say what AudioHardware.h says.

    /// A four-character code, as CoreAudio selectors are written in the
    /// headers: `'piri'` is the four bytes of "piri" in a `u32`.
    const fn four_char_code(code: &[u8; 4]) -> u32 {
        ((code[0] as u32) << 24)
            | ((code[1] as u32) << 16)
            | ((code[2] as u32) << 8)
            | (code[3] as u32)
    }

    /// The four characters a selector is made of.
    fn code_name(code: u32) -> String {
        code.to_be_bytes().iter().map(|byte| *byte as char).collect()
    }

    #[test]
    fn four_char_codes_are_the_selectors_in_the_headers() {
        // Guards against a transcription slip in either direction: the
        // constants come from the framework, the strings from
        // AudioHardware.h.
        assert_eq!(four_char_code(b"prs#"), kAudioHardwarePropertyProcessObjectList);
        assert_eq!(four_char_code(b"piri"), kAudioProcessPropertyIsRunningInput);
        assert_eq!(four_char_code(b"ppid"), kAudioProcessPropertyPID);
        assert_eq!(four_char_code(b"pbid"), kAudioProcessPropertyBundleID);
        assert_eq!(four_char_code(b"glob"), kAudioObjectPropertyScopeGlobal);
    }

    #[test]
    fn a_code_reads_back_as_its_four_characters() {
        assert_eq!(code_name(four_char_code(b"piri")), "piri");
        assert_eq!(code_name(kAudioHardwarePropertyProcessObjectList), "prs#");
        assert_eq!(code_name(kAudioProcessPropertyPID), "ppid");
        assert_eq!(code_name(kAudioProcessPropertyBundleID), "pbid");
    }

    #[test]
    fn a_cached_answer_is_reused_until_it_expires() {
        let mut cache: Cache<Option<Vec<Capture>>> = Cache::new(Duration::from_secs(1));
        let start = Instant::now();
        assert_eq!(cache.get(start), None);

        let busy = Some(vec![Capture { pid: 42, bundle_id: Some("com.example".into()) }]);
        cache.put(start, busy.clone());
        assert_eq!(cache.get(start), Some(busy.clone()));
        assert_eq!(cache.get(start + Duration::from_millis(999)), Some(busy));

        // A second later the world may have changed, so it must be asked
        // again rather than answered from memory.
        assert_eq!(cache.get(start + Duration::from_secs(1)), None);
    }

    #[test]
    fn an_unavailable_answer_is_cached_too() {
        // On macOS 13 the answer is "cannot say", and that is worth
        // remembering as much as any other: it will not change while the
        // process runs, and asking costs a HAL round trip.
        let mut cache: Cache<Option<Vec<Capture>>> = Cache::new(Duration::from_secs(1));
        let start = Instant::now();
        cache.put(start, None);
        assert_eq!(cache.get(start), Some(None));
    }

    #[test]
    fn asking_the_real_hal_answers_without_crashing() {
        // The one test that touches CoreAudio. It asserts only what is true
        // on any machine: the call returns, and on macOS 14+ (where this is
        // developed and run) it returns an answer rather than a shrug.
        let answer = in_use_by_others();
        if available() {
            assert!(answer.is_some(), "macOS 14+ should be able to answer");
        }
        // Whatever it said, asking twice must be as harmless as once — the
        // second answer comes from the cache.
        assert_eq!(answer, in_use_by_others());
    }
}
