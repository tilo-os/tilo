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
    ///
    /// # Errors
    /// Returns a terminal service, device or capacity failure.
    fn write(&mut self, samples: &[i16]) -> Result<Observation, String>;
    /// Read actual host device progress.
    ///
    /// # Errors
    /// Returns a service or device failure.
    fn observation(&mut self) -> Result<Observation, String>;
    /// Last host observation, without waiting for an RPC or decoder lock.
    fn cached_observation(&self) -> Observation;
    /// Terminal host failure observed by the output worker, without waiting.
    fn cached_error(&self) -> Option<String>;
    /// Pause this output only.
    ///
    /// # Errors
    /// Returns a service or device failure.
    fn pause(&mut self, paused: bool) -> Result<(), String>;
    /// Drop queued PCM before a seek or replacement.
    ///
    /// # Errors
    /// Returns a service or device failure.
    fn flush(&mut self) -> Result<(), String>;
    /// Set this application's stream gain.
    ///
    /// # Errors
    /// Returns a service or device failure.
    fn volume(&mut self, percent: u8) -> Result<(), String>;
    /// Fence this exact output generation; safe to repeat.
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

/// A hosted app must fail closed until its governed service is connected.
pub fn require_governed() {
    REQUIRED.store(true, Ordering::Release);
}

pub fn is_governed() -> bool {
    REQUIRED.load(Ordering::Acquire)
}

/// Install the host's admitted output route. Existing streams retain their original factory/route.
pub fn install(factory: Arc<dyn Factory>) {
    require_governed();
    if let Ok(mut current) = FACTORY.get_or_init(|| Mutex::new(None)).lock() {
        *current = Some(factory);
    }
}

/// Open an output-only local stream.
///
/// # Errors
/// Returns unavailable until the host installs an admitted route.
pub fn open() -> Result<Box<dyn Output>, String> {
    let factory = FACTORY
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Governed audio output service is unavailable")?
        .clone()
        .ok_or("Governed audio output service is unavailable")?;
    factory.open()
}
