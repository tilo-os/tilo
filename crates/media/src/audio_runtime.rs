//! Short-lived, capability-gated PipeWire audio diagnostics and transforms.
//!
//! Every process or virtual node created here has exactly one owner. Dropping
//! its session kills/waits the process or restores the physical default and
//! unloads the Pulse compatibility module. Microphone testing only reads
//! samples into a level meter; it never creates a playback/monitor link.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub balance: bool,
    pub mono_output: bool,
    pub channel_test: bool,
    pub microphone_meter: bool,
}

pub trait BoundedSession: Send {
    fn running(&mut self) -> bool;
    fn stop(&mut self);
}

pub trait MicrophoneMeterSession: BoundedSession {
    /// Latest peak in the normalized `0..=1` range.
    fn level(&mut self) -> f32;
    fn remaining(&self) -> Duration;
}

pub trait AudioRuntimeControl: Send + Sync {
    fn capabilities(&self) -> RuntimeCapabilities;
    fn set_balance(&self, sink_name: &str, base_volume: f32, balance: f32) -> Result<(), String>;
    fn start_mono(&self, sink_name: &str) -> Result<Box<dyn BoundedSession>, String>;
    fn start_channel_test(
        &self,
        sink_name: &str,
        left: bool,
    ) -> Result<Box<dyn BoundedSession>, String>;
    fn start_microphone_meter(
        &self,
        source_name: &str,
        duration: Duration,
    ) -> Result<Box<dyn MicrophoneMeterSession>, String>;
}

/// The accessibility/output policy the desktop session owns. Settings only
/// persists this intent; keeping the transform here gives virtual nodes one
/// long-lived owner and prevents opening a second Settings window from
/// creating a second mono sink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PersistentAudioPreferences {
    /// UI-space balance: `0` is left, `0.5` is centred, `1` is right.
    pub balance: f32,
    pub mono_output: bool,
}

impl Default for PersistentAudioPreferences {
    fn default() -> Self {
        Self {
            balance: 0.5,
            mono_output: false,
        }
    }
}

/// Reconciles persisted audio intent against successive PipeWire snapshots.
///
/// The policy is deliberately independent of the settings store and polling
/// mechanism: the compositor owns those, while tests can inject a recording
/// runtime and exact graph snapshots. Dropping it restores the physical sink
/// through the [`BoundedSession`] RAII contract.
pub struct PersistentAudioPolicy {
    runtime: Arc<dyn AudioRuntimeControl>,
    capabilities: RuntimeCapabilities,
    mono_session: Option<Box<dyn BoundedSession>>,
    mono_sink: Option<String>,
    last_balance: Option<(String, u32, u32)>,
}

impl PersistentAudioPolicy {
    pub fn new(runtime: Arc<dyn AudioRuntimeControl>) -> Self {
        let capabilities = runtime.capabilities();
        Self {
            runtime,
            capabilities,
            mono_session: None,
            mono_sink: None,
            last_balance: None,
        }
    }

    /// Apply `preferences` to the currently available physical sink. Returns
    /// a user-displayable diagnostic on failure; absence of a sink is a
    /// capability/state condition, never a panic.
    pub fn reconcile(
        &mut self,
        preferences: PersistentAudioPreferences,
        graph: &crate::audio::AudioGraph,
    ) -> Result<(), String> {
        let preferences = PersistentAudioPreferences {
            balance: preferences.balance.clamp(0.0, 1.0),
            ..preferences
        };
        if self
            .mono_session
            .as_mut()
            .is_some_and(|session| !session.running())
        {
            self.mono_session = None;
            self.mono_sink = None;
        }

        let physical = graph
            .default_sink()
            .filter(|sink| sink.name != ACCESSIBILITY_MONO_SINK_NAME)
            .or_else(|| {
                self.mono_sink
                    .as_deref()
                    .and_then(|name| graph.sinks.iter().find(|sink| sink.name == name))
            })
            .or_else(|| {
                graph
                    .sinks
                    .iter()
                    .find(|sink| sink.name != ACCESSIBILITY_MONO_SINK_NAME)
            })
            .cloned();

        if preferences.mono_output && !self.capabilities.mono_output {
            self.mono_session = None;
            self.mono_sink = None;
            return Err("Mono output is not supported by the active audio service".into());
        }
        if !preferences.mono_output {
            self.mono_session = None;
            self.mono_sink = None;
        } else if let Some(sink) = physical.as_ref() {
            // A one-channel device already presents one mix. Do not stack a
            // virtual remap on top of it; retain the preference so moving back
            // to stereo activates the transform automatically.
            if sink.channels < 2 {
                self.mono_session = None;
                self.mono_sink = None;
            } else {
                let changed = self.mono_sink.as_deref() != Some(sink.name.as_str());
                if changed {
                    self.mono_session = None;
                    self.mono_sink = None;
                }
                if self.mono_session.is_none() {
                    self.mono_session = Some(self.runtime.start_mono(&sink.name)?);
                    self.mono_sink = Some(sink.name.clone());
                }
            }
        } else {
            self.mono_session = None;
            self.mono_sink = None;
            return Err("No physical stereo output is available for mono audio".into());
        }

        let Some(sink) = physical.as_ref() else {
            self.last_balance = None;
            return Ok(());
        };
        if sink.channels != 2 {
            self.last_balance = None;
            return if (preferences.balance - 0.5).abs() > 0.001 {
                Err("Balance requires a stereo output".into())
            } else {
                Ok(())
            };
        }
        if !self.capabilities.balance {
            self.last_balance = None;
            return if (preferences.balance - 0.5).abs() > 0.001 {
                Err("Balance is not supported by the active audio service".into())
            } else {
                Ok(())
            };
        }
        let base = sink.volume.unwrap_or(1.0).clamp(0.0, 1.5);
        // Quantise the comparison only; the runtime still receives the full
        // values. This prevents harmless PipeWire float jitter from issuing a
        // command every poll while still following real volume changes.
        let signature = (
            sink.name.clone(),
            (base * 10_000.0).round() as u32,
            (preferences.balance * 10_000.0).round() as u32,
        );
        if self.last_balance.as_ref() != Some(&signature) {
            self.runtime
                .set_balance(&sink.name, base, preferences.balance * 2.0 - 1.0)?;
            self.last_balance = Some(signature);
        }
        Ok(())
    }

    pub fn mono_active(&self) -> bool {
        self.mono_session.is_some()
    }
}

pub struct RealAudioRuntime;

fn command_works(bin: &str, args: &[&str]) -> bool {
    Command::new(bin)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn run(bin: &str, args: &[String]) -> Result<String, String> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| format!("{bin} is unavailable: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

impl AudioRuntimeControl for RealAudioRuntime {
    fn capabilities(&self) -> RuntimeCapabilities {
        let pulse = command_works("pactl", &["info"]);
        RuntimeCapabilities {
            balance: pulse,
            mono_output: pulse,
            channel_test: command_works("pw-play", &["--version"]),
            microphone_meter: command_works("pw-record", &["--version"]),
        }
    }

    fn set_balance(&self, sink_name: &str, base_volume: f32, balance: f32) -> Result<(), String> {
        let (left, right) = balance_channel_percent(base_volume, balance);
        run(
            "pactl",
            &[
                "set-sink-volume".into(),
                sink_name.into(),
                format!("{left:.1}%"),
                format!("{right:.1}%"),
            ],
        )
        .map(|_| ())
    }

    fn start_mono(&self, sink_name: &str) -> Result<Box<dyn BoundedSession>, String> {
        // A two-channel virtual sink accepting MONO twice makes the Pulse
        // channel mixer sum stereo input and deliver the same mix to FL/FR.
        let args = mono_load_args(sink_name);
        let module = run("pactl", &args)?
            .parse::<u32>()
            .map_err(|_| "pactl did not return a mono module id".to_string())?;
        if let Err(error) = run(
            "pactl",
            &[
                "set-default-sink".into(),
                ACCESSIBILITY_MONO_SINK_NAME.into(),
            ],
        ) {
            let _ = run("pactl", &["unload-module".into(), module.to_string()]);
            return Err(error);
        }
        Ok(Box::new(MonoSession {
            module: Some(module),
            restore_sink: sink_name.to_string(),
        }))
    }

    fn start_channel_test(
        &self,
        sink_name: &str,
        left: bool,
    ) -> Result<Box<dyn BoundedSession>, String> {
        let mut child = Command::new("pw-play")
            .args([
                "--target",
                sink_name,
                "--rate",
                "48000",
                "--channels",
                "2",
                "--channel-map",
                "stereo",
                "--format",
                "s16",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("pw-play is unavailable: {e}"))?;
        // Keep the payload below a typical 64 KiB pipe buffer so starting a
        // test never waits for real-time playback on the UI thread.
        let pcm = channel_test_pcm(left, 48_000, Duration::from_millis(280));
        let Some(mut input) = child.stdin.take() else {
            let _ = child.kill();
            return Err("pw-play did not open its audio input".into());
        };
        if let Err(e) = input.write_all(&pcm) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not send the channel test: {e}"));
        }
        drop(input);
        Ok(Box::new(ChildSession {
            child: Some(child),
            deadline: Instant::now() + Duration::from_secs(2),
        }))
    }

    fn start_microphone_meter(
        &self,
        source_name: &str,
        duration: Duration,
    ) -> Result<Box<dyn MicrophoneMeterSession>, String> {
        let duration = duration.clamp(Duration::from_secs(1), Duration::from_secs(15));
        let mut child = Command::new("pw-record")
            .args([
                "--target",
                source_name,
                "--rate",
                "16000",
                "--channels",
                "1",
                "--channel-map",
                "mono",
                "--format",
                "s16",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("pw-record is unavailable: {e}"))?;
        let Some(mut output) = child.stdout.take() else {
            let _ = child.kill();
            return Err("pw-record did not expose captured samples".into());
        };
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0_u8; 2048];
            while let Ok(n) = output.read(&mut bytes) {
                if n == 0 {
                    break;
                }
                let _ = tx.send(pcm_peak(&bytes[..n]));
            }
        });
        Ok(Box::new(MicSession {
            child: Some(child),
            reader: Some(reader),
            levels: rx,
            latest: 0.0,
            deadline: Instant::now() + duration,
        }))
    }
}

/// Stable node name used to distinguish our virtual accessibility sink from
/// real hardware during subsequent graph enumerations.
pub const ACCESSIBILITY_MONO_SINK_NAME: &str = "qos_accessibility_mono";

pub fn balance_channel_percent(base_volume: f32, balance: f32) -> (f32, f32) {
    let base = base_volume.clamp(0.0, 1.5) * 100.0;
    let b = balance.clamp(-1.0, 1.0);
    (base * (1.0 - b.max(0.0)), base * (1.0 + b.min(0.0)))
}

fn mono_load_args(master: &str) -> Vec<String> {
    vec![
        "load-module".into(),
        "module-remap-sink".into(),
        format!("sink_name={ACCESSIBILITY_MONO_SINK_NAME}"),
        "sink_properties=device.description=qOS_Mono_Output".into(),
        format!("master={master}"),
        "channels=2".into(),
        "channel_map=mono,mono".into(),
        "master_channel_map=front-left,front-right".into(),
        "remix=yes".into(),
    ]
}

fn channel_test_pcm(left: bool, rate: u32, duration: Duration) -> Vec<u8> {
    let frames = (duration.as_secs_f32() * rate as f32) as usize;
    let mut out = Vec::with_capacity(frames * 4);
    for i in 0..frames {
        let fade = ((frames - i).min(i).min((rate / 100) as usize) as f32 / (rate / 100) as f32)
            .clamp(0.0, 1.0);
        let sample = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin()
            * 0.18
            * fade
            * i16::MAX as f32) as i16;
        let (l, r) = if left { (sample, 0) } else { (0, sample) };
        out.extend_from_slice(&l.to_le_bytes());
        out.extend_from_slice(&r.to_le_bytes());
    }
    out
}

fn pcm_peak(bytes: &[u8]) -> f32 {
    bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs() as f32 / i16::MAX as f32)
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

struct ChildSession {
    child: Option<Child>,
    deadline: Instant,
}
impl BoundedSession for ChildSession {
    fn running(&mut self) -> bool {
        if Instant::now() >= self.deadline {
            self.stop();
            return false;
        }
        self.child
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_none())
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for ChildSession {
    fn drop(&mut self) {
        self.stop();
    }
}

struct MonoSession {
    module: Option<u32>,
    restore_sink: String,
}
impl BoundedSession for MonoSession {
    fn running(&mut self) -> bool {
        self.module.is_some()
    }
    fn stop(&mut self) {
        if let Some(id) = self.module.take() {
            let _ = run(
                "pactl",
                &["set-default-sink".into(), self.restore_sink.clone()],
            );
            let _ = run("pactl", &["unload-module".into(), id.to_string()]);
        }
    }
}
impl Drop for MonoSession {
    fn drop(&mut self) {
        self.stop();
    }
}

struct MicSession {
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    levels: Receiver<f32>,
    latest: f32,
    deadline: Instant,
}
impl BoundedSession for MicSession {
    fn running(&mut self) -> bool {
        if Instant::now() >= self.deadline {
            self.stop();
            return false;
        }
        self.child
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_none())
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
impl MicrophoneMeterSession for MicSession {
    fn level(&mut self) -> f32 {
        for value in self.levels.try_iter() {
            self.latest = value;
        }
        self.latest
    }
    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}
impl Drop for MicSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioDevice, AudioGraph};
    use std::sync::Mutex;

    struct TestSession(Arc<Mutex<Vec<String>>>);
    impl BoundedSession for TestSession {
        fn running(&mut self) -> bool {
            true
        }
        fn stop(&mut self) {}
    }
    impl Drop for TestSession {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("mono-drop".into());
        }
    }

    struct RecordingRuntime(Arc<Mutex<Vec<String>>>);
    impl AudioRuntimeControl for RecordingRuntime {
        fn capabilities(&self) -> RuntimeCapabilities {
            RuntimeCapabilities {
                balance: true,
                mono_output: true,
                ..RuntimeCapabilities::default()
            }
        }
        fn set_balance(&self, sink: &str, base: f32, balance: f32) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .push(format!("balance:{sink}:{base:.2}:{balance:.2}"));
            Ok(())
        }
        fn start_mono(&self, sink: &str) -> Result<Box<dyn BoundedSession>, String> {
            self.0.lock().unwrap().push(format!("mono:{sink}"));
            Ok(Box::new(TestSession(self.0.clone())))
        }
        fn start_channel_test(&self, _: &str, _: bool) -> Result<Box<dyn BoundedSession>, String> {
            unreachable!()
        }
        fn start_microphone_meter(
            &self,
            _: &str,
            _: Duration,
        ) -> Result<Box<dyn MicrophoneMeterSession>, String> {
            unreachable!()
        }
    }

    fn sink(name: &str, default: bool, volume: f32) -> AudioDevice {
        AudioDevice {
            id: 1,
            serial: 1,
            name: name.into(),
            description: name.into(),
            is_default: default,
            volume: Some(volume),
            muted: Some(false),
            channels: 2,
            codec: None,
        }
    }
    #[test]
    fn balance_preserves_loud_side_and_attenuates_other() {
        assert_eq!(balance_channel_percent(0.8, 0.5), (40.0, 80.0));
        assert_eq!(balance_channel_percent(0.8, -0.5), (80.0, 40.0));
    }
    #[test]
    fn channel_test_is_stereo_and_isolated() {
        let p = channel_test_pcm(true, 1000, Duration::from_millis(100));
        assert_eq!(p.len(), 400);
        assert!(p.chunks_exact(4).all(|f| f[2] == 0 && f[3] == 0));
    }
    #[test]
    fn peak_reads_signed_pcm_without_storing_audio() {
        assert!((pcm_peak(&i16::MAX.to_le_bytes()) - 1.0).abs() < 0.001);
    }
    #[test]
    fn mono_module_is_bound_to_explicit_physical_sink() {
        let a = mono_load_args("alsa_output.card.stereo");
        assert!(a.contains(&"master=alsa_output.card.stereo".to_string()));
        assert!(a.contains(&"channel_map=mono,mono".to_string()));
    }
    #[test]
    fn persistent_policy_is_idempotent_and_owns_one_mono_session() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut policy = PersistentAudioPolicy::new(Arc::new(RecordingRuntime(calls.clone())));
        let graph = AudioGraph {
            sinks: vec![sink("speakers", true, 0.8)],
            ..AudioGraph::default()
        };
        let prefs = PersistentAudioPreferences {
            balance: 0.75,
            mono_output: true,
        };
        policy.reconcile(prefs, &graph).unwrap();
        policy.reconcile(prefs, &graph).unwrap();
        assert!(policy.mono_active());
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["mono:speakers", "balance:speakers:0.80:0.50"]
        );
        policy
            .reconcile(
                PersistentAudioPreferences {
                    mono_output: false,
                    ..prefs
                },
                &graph,
            )
            .unwrap();
        assert_eq!(calls.lock().unwrap().last().unwrap(), "mono-drop");
    }

    #[test]
    fn virtual_default_does_not_replace_the_physical_mono_master() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut policy = PersistentAudioPolicy::new(Arc::new(RecordingRuntime(calls.clone())));
        let prefs = PersistentAudioPreferences {
            mono_output: true,
            ..PersistentAudioPreferences::default()
        };
        policy
            .reconcile(
                prefs,
                &AudioGraph {
                    sinks: vec![sink("speakers", true, 0.8)],
                    ..AudioGraph::default()
                },
            )
            .unwrap();
        policy
            .reconcile(
                prefs,
                &AudioGraph {
                    sinks: vec![
                        sink(ACCESSIBILITY_MONO_SINK_NAME, true, 0.8),
                        sink("speakers", false, 0.8),
                    ],
                    ..AudioGraph::default()
                },
            )
            .unwrap();
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call.starts_with("mono:"))
                .count(),
            1
        );
    }
    #[test]
    fn a_new_physical_default_migrates_the_owned_mono_route() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut policy = PersistentAudioPolicy::new(Arc::new(RecordingRuntime(calls.clone())));
        let prefs = PersistentAudioPreferences {
            mono_output: true,
            ..PersistentAudioPreferences::default()
        };
        policy
            .reconcile(
                prefs,
                &AudioGraph {
                    sinks: vec![sink("speakers", true, 0.8)],
                    ..AudioGraph::default()
                },
            )
            .unwrap();
        let mut old = sink("speakers", false, 0.8);
        old.id = 1;
        let mut new = sink("headphones", true, 0.6);
        new.id = 2;
        policy
            .reconcile(
                prefs,
                &AudioGraph {
                    sinks: vec![old, new],
                    ..AudioGraph::default()
                },
            )
            .unwrap();
        let mono: Vec<_> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.starts_with("mono:"))
            .cloned()
            .collect();
        assert_eq!(mono, ["mono:speakers", "mono:headphones"]);
    }
    #[cfg(unix)]
    #[test]
    fn child_session_drop_terminates_process() {
        let child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .spawn()
            .unwrap();
        let pid = child.id();
        {
            let _s = ChildSession {
                child: Some(child),
                deadline: Instant::now() + Duration::from_secs(30),
            };
        }
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success();
        assert!(!alive);
    }
}
