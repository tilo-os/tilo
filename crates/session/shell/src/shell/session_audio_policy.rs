//! One session-lifetime owner for persistent audio transforms.
//!
//! Settings writes intent to the shared settings store. This worker reads it,
//! observes `PipeWire` off the render thread, and owns any virtual mono node
//! and the echo-cancelled microphone until the desktop session ends. It is
//! intentionally not a `DBus` service: neither operation is privileged, and
//! one owner per graphical session is the security/correctness boundary we
//! need. The same observation reports audio devices arriving and leaving, for
//! the "device connected/disconnected" system sounds.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use media::audio_runtime::{PersistentAudioPolicy, PersistentAudioPreferences, RealAudioRuntime};
use settings_store::system_sounds::SystemSoundEvent;

const POLL_INTERVAL: Duration = Duration::from_millis(1200);

#[derive(Debug)]
pub struct SessionAudioPolicyOwner {
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
    device_sounds: Mutex<Receiver<SystemSoundEvent>>,
}

impl SessionAudioPolicyOwner {
    /// Spawns the worker thread.
    ///
    /// # Errors
    ///
    /// Returns the OS error if the thread could not be spawned (resource
    /// exhaustion) — the caller runs the session without persistent audio
    /// transforms rather than tearing the whole desktop down over it.
    pub fn spawn() -> std::io::Result<Self> {
        let (stop, rx) = mpsc::channel();
        let (device_tx, device_sounds) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("tilo-session-audio-policy".into())
            .spawn(move || {
                let mut policy = PersistentAudioPolicy::new(Arc::new(RealAudioRuntime));
                let mut last_error = None;
                let mut devices = DeviceRoster::default();
                loop {
                    let prefs = load_preferences();
                    let graph = media::audio::enumerate();
                    for event in devices.observe(&graph) {
                        let _ = device_tx.send(event);
                    }
                    let error = policy.reconcile(prefs, &graph).err();
                    if error != last_error {
                        match error.as_deref() {
                            Some(error) => tracing::warn!(
                                target: "tilo::audio_policy",
                                %error,
                                "persistent audio preference could not be applied"
                            ),
                            None if last_error.is_some() => tracing::info!(
                                target: "tilo::audio_policy",
                                "persistent audio preference recovered"
                            ),
                            None => {}
                        }
                        last_error = error;
                    }
                    if rx.recv_timeout(POLL_INTERVAL).is_ok() {
                        break;
                    }
                }
                // `policy` drops here: its bounded mono session restores the
                // physical default before the compositor exits.
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
            device_sounds: Mutex::new(device_sounds),
        })
    }

    /// Audio devices that arrived or left since the last call, as the
    /// system sounds they stand for.
    pub fn drain_device_sounds(&self) -> Vec<SystemSoundEvent> {
        self.device_sounds
            .lock()
            .map(|events| events.try_iter().collect())
            .unwrap_or_default()
    }
}

/// The audio hardware present at the last observation, by description.
///
/// Devices, not nodes: a Bluetooth headset switching between its music and
/// call profiles replaces its nodes but stays one device. The first
/// observation is the baseline, and a graph with no hardware at all is
/// treated as "the audio service could not be read" rather than every device
/// leaving at once.
#[derive(Debug, Default)]
struct DeviceRoster {
    known: Option<Vec<String>>,
}

impl DeviceRoster {
    fn observe(&mut self, graph: &media::audio::AudioGraph) -> Vec<SystemSoundEvent> {
        if graph.hardware.is_empty() {
            return Vec::new();
        }
        let mut now: Vec<String> = graph
            .hardware
            .iter()
            .map(|device| device.description.clone())
            .collect();
        now.sort();
        let Some(before) = self.known.replace(now.clone()) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        if multiset_difference(&now, &before) {
            events.push(SystemSoundEvent::DeviceConnected);
        }
        if multiset_difference(&before, &now) {
            events.push(SystemSoundEvent::DeviceDisconnected);
        }
        events
    }
}

/// Whether sorted `a` holds an item (counting repeats) that sorted `b` lacks.
fn multiset_difference(a: &[String], b: &[String]) -> bool {
    let mut rest = b.iter().peekable();
    for item in a {
        while rest.peek().is_some_and(|other| *other < item) {
            rest.next();
        }
        if rest.peek() == Some(&item) {
            rest.next();
        } else {
            return true;
        }
    }
    false
}

impl Drop for SessionAudioPolicyOwner {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn load_preferences() -> PersistentAudioPreferences {
    let doc = settings_store::load();
    preferences_from(&doc)
}

fn preferences_from(
    doc: &serde_json::Map<String, serde_json::Value>,
) -> PersistentAudioPreferences {
    // Clamped to 0.0..=1.0 above; f32's precision loss over that tiny
    // range is imperceptible for a pan-balance control.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "value is clamped to 0.0..=1.0 by f64_in; narrowing loses no meaningful precision"
    )]
    let balance = settings_store::f64_in(doc, "audio", "balance", 0.0..=1.0, 0.5) as f32;
    PersistentAudioPreferences {
        balance,
        mono_output: settings_store::bool_at(doc, "audio", "mono_output").unwrap_or(false),
        echo_cancellation: settings_store::bool_at(doc, "audio", "echo_cancellation")
            .unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn malformed_or_out_of_range_preferences_are_safe_defaults() {
        let mut doc = serde_json::Map::new();
        doc.insert(
            "audio".into(),
            json!({"balance": 8.0, "mono_output": "yes", "echo_cancellation": "no"}),
        );
        assert_eq!(
            preferences_from(&doc),
            PersistentAudioPreferences::default()
        );
    }

    fn graph(devices: &[&str]) -> media::audio::AudioGraph {
        media::audio::AudioGraph {
            hardware: devices
                .iter()
                .enumerate()
                .map(|(id, name)| media::audio::AudioHardwareDevice {
                    id: u32::try_from(id).unwrap_or(0),
                    description: (*name).into(),
                    active_profile: None,
                    profiles: Vec::new(),
                })
                .collect(),
            ..media::audio::AudioGraph::default()
        }
    }

    #[test]
    fn devices_arriving_and_leaving_become_system_sounds() {
        let mut roster = DeviceRoster::default();
        assert!(
            roster.observe(&graph(&["Built-in Audio"])).is_empty(),
            "baseline"
        );
        assert!(roster.observe(&graph(&["Built-in Audio"])).is_empty());
        assert_eq!(
            roster.observe(&graph(&["Built-in Audio", "Headset"])),
            [SystemSoundEvent::DeviceConnected]
        );
        assert_eq!(
            roster.observe(&graph(&["Built-in Audio", "Headset", "Headset"])),
            [SystemSoundEvent::DeviceConnected],
            "a second identical headset is a new device"
        );
        assert!(
            roster.observe(&graph(&[])).is_empty(),
            "an unreadable graph is not every device leaving"
        );
        assert_eq!(
            roster.observe(&graph(&["Built-in Audio"])),
            [SystemSoundEvent::DeviceDisconnected]
        );
        assert_eq!(
            roster.observe(&graph(&["Dock", "Headset"])),
            [
                SystemSoundEvent::DeviceConnected,
                SystemSoundEvent::DeviceDisconnected
            ]
        );
    }

    #[test]
    fn calls_are_echo_cancelled_unless_the_person_turned_it_off() {
        assert!(preferences_from(&serde_json::Map::new()).echo_cancellation);
        let mut doc = serde_json::Map::new();
        doc.insert("audio".into(), json!({"echo_cancellation": false}));
        assert!(!preferences_from(&doc).echo_cancellation);
    }
}
