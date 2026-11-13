//! Output-only service borrowed from the local host. Apps receive neither audio sockets nor capture authority.
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "decode", feature = "voice")
))]
pub mod gst;

pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const MAX_SAMPLES: usize = 1920;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    pub rendered_frames: u64,
    pub queued_frames: u32,
    pub ready: bool,
}

pub trait Output: Send {
    /// Submit at most 20 ms of stereo 48 kHz S16 PCM, with bounded backpressure.
    fn write(&mut self, samples: &[i16]) -> Result<Observation, String>;
    fn observation(&mut self) -> Result<Observation, String>;
    fn cached_observation(&self) -> Observation;
    fn cached_error(&self) -> Option<String>;
    /// Pause this output only.
    fn pause(&mut self, paused: bool) -> Result<(), String>;
    fn flush(&mut self) -> Result<(), String>;
    fn volume(&mut self, percent: u8) -> Result<(), String>;
    fn close(&mut self);
}

pub trait Factory: Send + Sync {
    /// Open output for the factory's admitted local application route.
    ///
    /// # Errors
    /// Returns an admission, service or device failure; never falls back to capture access.
    fn open(&self) -> Result<Box<dyn Output>, String>;
}

static REQUIRED: AtomicBool = AtomicBool::new(false);
static FACTORY: OnceLock<Mutex<Option<Arc<dyn Factory>>>> = OnceLock::new();
static DISABLED: AtomicBool = AtomicBool::new(false);

/// The person's lockdown switch for sound: while it is on nothing opens and
/// every open stream refuses its next write, so turning it on silences what
/// is already playing instead of only what starts later.
pub fn set_disabled(disabled: bool) {
    DISABLED.store(disabled, Ordering::Release);
}

pub fn is_disabled() -> bool {
    DISABLED.load(Ordering::Acquire)
}

const DISABLED_MESSAGE: &str = "Sound output is disabled by the person's lockdown policy";

/// A hosted app must fail closed until its governed service is connected.
pub fn require_governed() {
    REQUIRED.store(true, Ordering::Release);
}

pub fn is_governed() -> bool {
    REQUIRED.load(Ordering::Acquire)
}

pub fn install(factory: Arc<dyn Factory>) {
    require_governed();
    if let Ok(mut current) = FACTORY.get_or_init(|| Mutex::new(None)).lock() {
        *current = Some(factory);
    }
}

/// Open an output-only local stream.
///
/// # Errors
/// Nothing is served, the host refused, or the person's lockdown policy
/// silences output.
pub fn open() -> Result<Box<dyn Output>, String> {
    if is_disabled() {
        return Err(DISABLED_MESSAGE.into());
    }
    let factory = FACTORY
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Governed audio output service is unavailable")?
        .clone()
        .ok_or("Governed audio output service is unavailable")?;
    factory
        .open()
        .map(|output| Box::new(Gated { output }) as Box<dyn Output>)
}

/// A stream that closes itself the moment the person silences output: the
/// decision is asked at every write, so no player has to know about the
/// policy or be restarted for it.
struct Gated {
    output: Box<dyn Output>,
}

impl Output for Gated {
    fn write(&mut self, samples: &[i16]) -> Result<Observation, String> {
        if is_disabled() {
            self.output.close();
            return Err(DISABLED_MESSAGE.into());
        }
        self.output.write(samples)
    }

    fn observation(&mut self) -> Result<Observation, String> {
        self.output.observation()
    }

    fn cached_observation(&self) -> Observation {
        self.output.cached_observation()
    }

    fn cached_error(&self) -> Option<String> {
        self.output.cached_error()
    }

    fn pause(&mut self, paused: bool) -> Result<(), String> {
        self.output.pause(paused)
    }

    fn flush(&mut self) -> Result<(), String> {
        self.output.flush()
    }

    fn volume(&mut self, percent: u8) -> Result<(), String> {
        self.output.volume(percent)
    }

    fn close(&mut self) {
        self.output.close();
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Counter(Arc<AtomicUsize>);

    impl Output for Counter {
        fn write(&mut self, _samples: &[i16]) -> Result<Observation, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Observation::default())
        }
        fn observation(&mut self) -> Result<Observation, String> {
            Ok(Observation::default())
        }
        fn cached_observation(&self) -> Observation {
            Observation::default()
        }
        fn cached_error(&self) -> Option<String> {
            None
        }
        fn pause(&mut self, _paused: bool) -> Result<(), String> {
            Ok(())
        }
        fn flush(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn volume(&mut self, _percent: u8) -> Result<(), String> {
            Ok(())
        }
        fn close(&mut self) {}
    }

    struct Counting(Arc<AtomicUsize>);

    impl Factory for Counting {
        fn open(&self) -> Result<Box<dyn Output>, String> {
            Ok(Box::new(Counter(Arc::clone(&self.0))))
        }
    }

    #[test]
    fn the_lockdown_switch_refuses_new_output_and_silences_a_running_one() {
        let writes = Arc::new(AtomicUsize::new(0));
        install(Arc::new(Counting(Arc::clone(&writes))));
        set_disabled(false);
        let mut output = open().expect("output");
        output.write(&[0; 8]).expect("audible");
        assert_eq!(writes.load(Ordering::SeqCst), 1);

        set_disabled(true);
        let refused = output.write(&[0; 8]);
        set_disabled(false);
        assert!(refused.is_err(), "the running stream is silenced");
        assert_eq!(writes.load(Ordering::SeqCst), 1, "nothing more is played");
        assert!(open().is_ok(), "the switch only lasts while it is on");
        set_disabled(true);
        assert!(open().is_err(), "nothing opens while it is on");
        set_disabled(false);
    }
}
