//! The session's audio policy helper (`tilo-audio`, crate
//! `tilo-audio-service`), started with the session.
//!
//! The helper owns the persistent audio transforms and hotplug policy; the
//! compositor only reads the devices it reports arriving and leaving, and
//! sounds them under its own quiet rules. Dropping [`AudioService`] closes
//! the helper's standard input, which stops it, and reaps it.

use std::io::BufRead as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};

use settings_store::system_sounds::SystemSoundEvent;
use tilo_audio_service::DeviceEvent;

pub struct AudioService {
    child: Child,
    devices: Mutex<Receiver<DeviceEvent>>,
}

impl AudioService {
    /// Start the helper; `None` when it is not installed here or cannot run,
    /// and the session runs without persistent audio transforms.
    pub fn start() -> Option<Self> {
        let helper = helper()?;
        let mut child = Command::new(&helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .inspect_err(|error| {
                tracing::warn!(
                    target: "tilo::audio_policy",
                    %error,
                    helper = %helper.display(),
                    "the session audio helper did not start"
                );
            })
            .ok()?;
        let stdout = child.stdout.take()?;
        let (sender, devices) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("tilo-audio-events".into())
            .spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Some(event) = DeviceEvent::parse(&line)
                        && sender.send(event).is_err()
                    {
                        break;
                    }
                }
            });
        if let Err(error) = reader {
            tracing::warn!(target: "tilo::audio_policy", %error, "the audio helper's events go unread");
        }
        Some(Self {
            child,
            devices: Mutex::new(devices),
        })
    }

    /// Audio devices that arrived or left since the last call, as the
    /// system sounds they stand for.
    pub fn drain_device_sounds(&self) -> Vec<SystemSoundEvent> {
        self.devices
            .lock()
            .map(|events| {
                events
                    .try_iter()
                    .map(|event| match event {
                        DeviceEvent::Connected => SystemSoundEvent::DeviceConnected,
                        DeviceEvent::Disconnected => SystemSoundEvent::DeviceDisconnected,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for AudioService {
    fn drop(&mut self) {
        // Closing its standard input is the helper's signal to restore what
        // it changed and stop.
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

impl std::fmt::Debug for AudioService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioService")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

/// The helper: beside the running compositor, else where the image installs
/// it. Never in a unit test, which must not change the machine's audio.
fn helper() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tilo-audio")));
    beside
        .into_iter()
        .chain([PathBuf::from("/usr/libexec/tilo-audio")])
        .find(|path| path.exists())
}
