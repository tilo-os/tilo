//! One session-lifetime owner for persistent audio transforms.
//!
//! Settings writes intent to the shared settings store. This worker reads it,
//! observes `PipeWire` off the render thread, and owns any virtual mono node
//! until the desktop session ends. It is intentionally not a `DBus` service:
//! neither operation is privileged, and one owner per graphical session is the
//! security/correctness boundary we need.

use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use media::audio_runtime::{PersistentAudioPolicy, PersistentAudioPreferences, RealAudioRuntime};

const POLL_INTERVAL: Duration = Duration::from_millis(1200);

#[derive(Debug)]
pub struct SessionAudioPolicyOwner {
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
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
        let worker = std::thread::Builder::new()
            .name("qos-session-audio-policy".into())
            .spawn(move || {
                let mut policy = PersistentAudioPolicy::new(Arc::new(RealAudioRuntime));
                let mut last_error = None;
                loop {
                    let prefs = load_preferences();
                    let graph = media::audio::enumerate();
                    let error = policy.reconcile(prefs, &graph).err();
                    if error != last_error {
                        match error.as_deref() {
                            Some(error) => tracing::warn!(
                                target: "qos::audio_policy",
                                %error,
                                "persistent audio preference could not be applied"
                            ),
                            None if last_error.is_some() => tracing::info!(
                                target: "qos::audio_policy",
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
        })
    }
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
            json!({"balance": 8.0, "mono_output": "yes"}),
        );
        assert_eq!(
            preferences_from(&doc),
            PersistentAudioPreferences::default()
        );
    }
}
