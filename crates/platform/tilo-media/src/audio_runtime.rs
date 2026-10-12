//! Short-lived, capability-gated `PipeWire` audio diagnostics and transforms.
//!
//! Every process or virtual node created here has exactly one owner. Dropping
//! its session kills/waits the process or restores the physical default and
//! unloads the Pulse compatibility module. Microphone testing only reads
//! samples into a level meter; it never creates a playback/monitor link.

#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(unix)]
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::mpsc::{self, Receiver};
#[cfg(unix)]
use std::thread::JoinHandle;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field reports a distinct, independently-detected runtime \
              capability, not a state machine"
)]
pub struct RuntimeCapabilities {
    pub balance: bool,
    pub mono_output: bool,
    pub channel_test: bool,
    pub microphone_meter: bool,
    /// The session can run the shipped echo-cancellation client
    /// ([`ECHO_CANCEL_CONFIG`]).
    pub echo_cancellation: bool,
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

    /// Set `sink_name`'s base volume and left/right balance.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic message if the underlying volume-control command
    /// is unavailable or rejects the sink name.
    fn set_balance(&self, sink_name: &str, base_volume: f32, balance: f32) -> Result<(), String>;

    /// Remap `sink_name` to a virtual mono-mix device and make it the
    /// default output.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic message if the remap module fails to load or the
    /// default sink cannot be switched to it.
    fn start_mono(&self, sink_name: &str) -> Result<Box<dyn BoundedSession>, String>;

    /// Play a short identification tone on `sink_name`'s left or right
    /// channel.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic message if the playback process cannot be
    /// spawned or its audio input cannot be written.
    fn start_channel_test(
        &self,
        sink_name: &str,
        left: bool,
    ) -> Result<Box<dyn BoundedSession>, String>;

    /// Start metering `source_name` for up to `duration`.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic message if the capture process cannot be spawned
    /// or its captured samples cannot be read.
    fn start_microphone_meter(
        &self,
        source_name: &str,
        duration: Duration,
    ) -> Result<Box<dyn MicrophoneMeterSession>, String>;

    /// Create the echo-cancelled microphone ([`ECHO_CANCEL_SOURCE_NAME`]).
    /// It lives until the returned session is stopped or dropped.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic message if the echo-cancellation client cannot be
    /// started.
    fn start_echo_cancellation(&self) -> Result<Box<dyn BoundedSession>, String> {
        Err("Echo cancellation is not supported by the active audio service".into())
    }
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
    /// Offer calls an echo-cancelled microphone.
    pub echo_cancellation: bool,
}

impl Default for PersistentAudioPreferences {
    fn default() -> Self {
        Self {
            balance: 0.5,
            mono_output: false,
            echo_cancellation: true,
        }
    }
}

/// Reconciles persisted audio intent against successive `PipeWire` snapshots.
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
    echo_session: Option<Box<dyn BoundedSession>>,
    /// Consecutive echo-cancellation client exits, used to back off restarts
    /// of a client that dies on start.
    echo_failures: u32,
    echo_retry_at: Option<std::time::Instant>,
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
            echo_session: None,
            echo_failures: 0,
            echo_retry_at: None,
        }
    }

    /// Apply `preferences` to the currently available physical sink. Returns
    /// a user-displayable diagnostic on failure; absence of a sink is a
    /// capability/state condition, never a panic.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic string if mono output or balance is requested but
    /// unsupported by the runtime, if no physical stereo sink is available,
    /// if the underlying runtime call to start the mono session or set the
    /// balance fails, or if the echo-cancelled microphone cannot be started.
    pub fn reconcile(
        &mut self,
        preferences: PersistentAudioPreferences,
        graph: &crate::audio::AudioGraph,
    ) -> Result<(), String> {
        let echo = self.reconcile_echo_cancellation(preferences.echo_cancellation, graph);
        self.reconcile_output(preferences, graph).and(echo)
    }

    /// Keep exactly one echo-cancelled microphone while the preference is on
    /// and a microphone exists. Unsupported runtimes are not an error: the
    /// preference defaults on, and Settings hides it where it cannot work.
    fn reconcile_echo_cancellation(
        &mut self,
        enabled: bool,
        graph: &crate::audio::AudioGraph,
    ) -> Result<(), String> {
        if self
            .echo_session
            .as_mut()
            .is_some_and(|session| !session.running())
        {
            self.echo_session = None;
            self.echo_failures = self.echo_failures.saturating_add(1);
            let backoff = ECHO_CANCEL_RESTART_BACKOFF
                .saturating_mul(self.echo_failures.min(ECHO_CANCEL_MAX_BACKOFF_STEPS));
            self.echo_retry_at = std::time::Instant::now().checked_add(backoff);
        }
        if !enabled || !self.capabilities.echo_cancellation || graph.sources.is_empty() {
            self.echo_session = None;
            self.echo_failures = 0;
            self.echo_retry_at = None;
            return Ok(());
        }
        if self.echo_session.is_some()
            || self
                .echo_retry_at
                .is_some_and(|at| std::time::Instant::now() < at)
        {
            return Ok(());
        }
        self.echo_session = Some(self.runtime.start_echo_cancellation()?);
        Ok(())
    }

    pub fn echo_cancellation_active(&self) -> bool {
        self.echo_session.is_some()
    }

    /// When a backed-off echo-cancellation restart falls due. Nothing in the
    /// graph changes at that moment, so an event-driven owner must reconcile
    /// again by then on its own.
    pub fn next_retry(&self) -> Option<std::time::Instant> {
        self.echo_retry_at
    }

    fn reconcile_output(
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
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "base is clamped to 0.0..=1.5 and preferences.balance to \
                      0.0..=1.0 just above, so both quantised values fit u32"
        )]
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

#[cfg(unix)]
fn command_works(bin: &str, args: &[&str]) -> bool {
    bounded_command::run(bin, args, None, 64 * 1024, Duration::from_secs(3))
        .is_ok_and(|output| output.status.success())
}

#[cfg(unix)]
fn run(bin: &str, args: &[String]) -> Result<String, String> {
    let mut command = Command::new(bin);
    command.args(args);
    let out = bounded_command::run_prepared(command, None, 64 * 1024, Duration::from_secs(3))
        .map_err(|e| format!("{bin} control failed: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(out.stderr_diagnostic())
    }
}

#[cfg(unix)]
impl AudioRuntimeControl for RealAudioRuntime {
    fn capabilities(&self) -> RuntimeCapabilities {
        let pulse = command_works("pactl", &["info"]);
        RuntimeCapabilities {
            balance: pulse,
            mono_output: pulse,
            channel_test: command_works("pw-play", &["--version"]),
            microphone_meter: command_works("pw-record", &["--version"]),
            echo_cancellation: echo_cancel_config_installed()
                && command_works("pipewire", &["--version"]),
        }
    }

    fn start_echo_cancellation(&self) -> Result<Box<dyn BoundedSession>, String> {
        let child = Command::new("pipewire")
            .args(["-c", ECHO_CANCEL_CONFIG])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("pipewire is unavailable: {e}"))?;
        Ok(Box::new(ChildSession {
            child: Some(child),
            deadline: None,
        }))
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
            .process_group(0)
            .spawn()
            .map_err(|e| format!("pw-play is unavailable: {e}"))?;
        // Keep the payload below a typical 64 KiB pipe buffer so starting a
        // test never waits for real-time playback on the UI thread.
        let pcm = channel_test_pcm(left, 48_000, Duration::from_millis(280));
        let Some(mut input) = child.stdin.take() else {
            bounded_command::terminate_tree(&mut child, std::time::Duration::ZERO);
            return Err("pw-play did not open its audio input".into());
        };
        if let Err(e) = input.write_all(&pcm) {
            bounded_command::terminate_tree(&mut child, std::time::Duration::ZERO);
            return Err(format!("could not send the channel test: {e}"));
        }
        drop(input);
        Ok(Box::new(ChildSession {
            child: Some(child),
            deadline: Some(
                Instant::now()
                    .checked_add(Duration::from_secs(2))
                    .unwrap_or_else(Instant::now),
            ),
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
            .process_group(0)
            .spawn()
            .map_err(|e| format!("pw-record is unavailable: {e}"))?;
        let Some(mut output) = child.stdout.take() else {
            bounded_command::terminate_tree(&mut child, std::time::Duration::ZERO);
            return Err("pw-record did not expose captured samples".into());
        };
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0_u8; 2048];
            while let Ok(n) = output.read(&mut bytes) {
                if n == 0 {
                    break;
                }
                let _ = tx.send(pcm_peak(bytes.get(..n).unwrap_or(&[])));
            }
        });
        Ok(Box::new(MicSession {
            child: Some(child),
            reader: Some(reader),
            levels: rx,
            latest: 0.0,
            deadline: Instant::now()
                .checked_add(duration)
                .unwrap_or_else(Instant::now),
        }))
    }
}

// PipeWire process control and session teardown require Unix process groups.
// Keep this public runtime available for portable callers without advertising
// or attempting unsupported operations on other hosts.
#[cfg(not(unix))]
impl AudioRuntimeControl for RealAudioRuntime {
    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities::default()
    }

    fn set_balance(
        &self,
        _sink_name: &str,
        _base_volume: f32,
        _balance: f32,
    ) -> Result<(), String> {
        Err("PipeWire audio runtime is unsupported on this platform".into())
    }

    fn start_mono(&self, _sink_name: &str) -> Result<Box<dyn BoundedSession>, String> {
        Err("PipeWire audio runtime is unsupported on this platform".into())
    }

    fn start_channel_test(
        &self,
        _sink_name: &str,
        _left: bool,
    ) -> Result<Box<dyn BoundedSession>, String> {
        Err("PipeWire audio runtime is unsupported on this platform".into())
    }

    fn start_microphone_meter(
        &self,
        _source_name: &str,
        _duration: Duration,
    ) -> Result<Box<dyn MicrophoneMeterSession>, String> {
        Err("PipeWire audio runtime is unsupported on this platform".into())
    }
}

/// Stable node name used to distinguish our virtual accessibility sink from
/// real hardware during subsequent graph enumerations.
pub const ACCESSIBILITY_MONO_SINK_NAME: &str = "tilo_accessibility_mono";

/// The `PipeWire` client configuration, installed in `PipeWire`'s config search
/// path, that creates the echo-cancelled microphone.
pub const ECHO_CANCEL_CONFIG: &str = "tilo-echo-cancel.conf";

pub use crate::audio::{ECHO_CANCEL_NODE_PREFIX, ECHO_CANCEL_SOURCE_NAME};

const ECHO_CANCEL_RESTART_BACKOFF: Duration = Duration::from_secs(5);
const ECHO_CANCEL_MAX_BACKOFF_STEPS: u32 = 12;

/// Whether [`ECHO_CANCEL_CONFIG`] is somewhere `pipewire -c` looks: the
/// user's and the administrator's override directories, then the system one.
#[cfg(unix)]
fn echo_cancel_config_installed() -> bool {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("PIPEWIRE_CONFIG_DIR") {
        dirs.push(std::path::PathBuf::from(dir));
    }
    if let Some(config) = tilo_paths::xdg::config_home() {
        dirs.push(config.join("pipewire"));
    }
    dirs.push("/etc/pipewire".into());
    dirs.push("/usr/share/pipewire".into());
    dirs.iter()
        .any(|dir| dir.join(ECHO_CANCEL_CONFIG).is_file())
}

pub fn balance_channel_percent(base_volume: f32, balance: f32) -> (f32, f32) {
    let base = base_volume.clamp(0.0, 1.5) * 100.0;
    let b = balance.clamp(-1.0, 1.0);
    (base * (1.0 - b.max(0.0)), base * (1.0 + b.min(0.0)))
}

#[cfg(any(unix, test))]
fn mono_load_args(master: &str) -> Vec<String> {
    vec![
        "load-module".into(),
        "module-remap-sink".into(),
        format!("sink_name={ACCESSIBILITY_MONO_SINK_NAME}"),
        "sink_properties=device.description=Tilo_Mono_Output".into(),
        format!("master={master}"),
        "channels=2".into(),
        "channel_map=mono,mono".into(),
        "master_channel_map=front-left,front-right".into(),
        "remix=yes".into(),
    ]
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "synthesizes a short identification tone from code-controlled \
              rate/duration (never from untrusted media); frame counts and \
              sample amplitudes stay well within usize/i16 range for any \
              realistic call"
)]
#[cfg(any(unix, test))]
fn channel_test_pcm(left: bool, rate: u32, duration: Duration) -> Vec<u8> {
    let frames = (duration.as_secs_f32() * rate as f32) as usize;
    let mut out = Vec::with_capacity(frames * 4);
    for i in 0..frames {
        let fade = ((frames - i).min(i).min((rate / 100) as usize) as f32 / (rate / 100) as f32)
            .clamp(0.0, 1.0);
        let sample = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin()
            * 0.18
            * fade
            * f32::from(i16::MAX)) as i16;
        let (l, r) = if left { (sample, 0) } else { (0, sample) };
        out.extend_from_slice(&l.to_le_bytes());
        out.extend_from_slice(&r.to_le_bytes());
    }
    out
}

#[cfg(any(unix, test))]
fn pcm_peak(bytes: &[u8]) -> f32 {
    bytes
        .chunks_exact(2)
        .map(|b| {
            let &[lo, hi] = b else { return 0.0 };
            f32::from(i16::from_le_bytes([lo, hi]).unsigned_abs()) / f32::from(i16::MAX)
        })
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

#[cfg(unix)]
struct ChildSession {
    child: Option<Child>,
    /// `None` for a client that lives until its owner stops it.
    deadline: Option<Instant>,
}
#[cfg(unix)]
impl BoundedSession for ChildSession {
    fn running(&mut self) -> bool {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.stop();
            return false;
        }
        self.child
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_none())
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            bounded_command::terminate_tree(&mut child, std::time::Duration::ZERO);
        }
    }
}
#[cfg(unix)]
impl Drop for ChildSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
struct MonoSession {
    module: Option<u32>,
    restore_sink: String,
}
#[cfg(unix)]
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
#[cfg(unix)]
impl Drop for MonoSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
struct MicSession {
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    levels: Receiver<f32>,
    latest: f32,
    deadline: Instant,
}
#[cfg(unix)]
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
            bounded_command::terminate_tree(&mut child, std::time::Duration::ZERO);
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
#[cfg(unix)]
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
#[cfg(unix)]
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

    #[cfg(not(unix))]
    #[test]
    fn real_runtime_reports_unsupported_without_starting_sessions() {
        let runtime = RealAudioRuntime;
        assert_eq!(runtime.capabilities(), RuntimeCapabilities::default());
        let expected = Some("PipeWire audio runtime is unsupported on this platform".to_string());
        assert_eq!(runtime.set_balance("speakers", 1.0, 0.0).err(), expected);
        assert_eq!(runtime.start_mono("speakers").err(), expected);
        assert_eq!(runtime.start_channel_test("speakers", true).err(), expected);
        assert_eq!(
            runtime
                .start_microphone_meter("microphone", Duration::from_secs(1))
                .err(),
            expected
        );
    }

    struct TestSession {
        calls: Arc<Mutex<Vec<String>>>,
        label: &'static str,
        alive: Arc<Mutex<bool>>,
    }
    impl BoundedSession for TestSession {
        fn running(&mut self) -> bool {
            *self.alive.lock().unwrap()
        }
        fn stop(&mut self) {}
    }
    impl Drop for TestSession {
        fn drop(&mut self) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}-drop", self.label));
        }
    }

    struct RecordingRuntime(Arc<Mutex<Vec<String>>>);
    impl RecordingRuntime {
        fn session(&self, label: &'static str) -> Box<dyn BoundedSession> {
            Box::new(TestSession {
                calls: self.0.clone(),
                label,
                alive: Arc::new(Mutex::new(true)),
            })
        }
    }
    impl AudioRuntimeControl for RecordingRuntime {
        fn capabilities(&self) -> RuntimeCapabilities {
            RuntimeCapabilities {
                balance: true,
                mono_output: true,
                echo_cancellation: true,
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
            Ok(self.session("mono"))
        }
        fn start_echo_cancellation(&self) -> Result<Box<dyn BoundedSession>, String> {
            self.0.lock().unwrap().push("echo".into());
            Ok(self.session("echo"))
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

    fn microphone() -> AudioDevice {
        AudioDevice {
            channels: 1,
            ..sink("alsa_input.mic", true, 0.5)
        }
    }

    #[test]
    fn echo_cancellation_is_one_session_that_follows_the_preference() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut policy = PersistentAudioPolicy::new(Arc::new(RecordingRuntime(calls.clone())));
        let graph = AudioGraph {
            sinks: vec![sink("speakers", true, 0.8)],
            sources: vec![microphone()],
            ..AudioGraph::default()
        };
        let on = PersistentAudioPreferences::default();
        assert!(
            on.echo_cancellation,
            "calls get echo cancellation by default"
        );
        policy.reconcile(on, &graph).unwrap();
        policy.reconcile(on, &graph).unwrap();
        assert!(policy.echo_cancellation_active());
        let off = PersistentAudioPreferences {
            echo_cancellation: false,
            ..on
        };
        policy.reconcile(off, &graph).unwrap();
        assert!(!policy.echo_cancellation_active());
        policy.reconcile(on, &graph).unwrap();
        let echo: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.starts_with("echo"))
            .cloned()
            .collect();
        assert_eq!(echo, ["echo", "echo-drop", "echo"]);
    }

    #[test]
    fn echo_cancellation_waits_for_a_microphone_and_ends_with_the_policy() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut policy = PersistentAudioPolicy::new(Arc::new(RecordingRuntime(calls.clone())));
        let prefs = PersistentAudioPreferences::default();
        policy.reconcile(prefs, &AudioGraph::default()).unwrap();
        assert!(calls.lock().unwrap().is_empty());
        let with_mic = AudioGraph {
            sources: vec![microphone()],
            ..AudioGraph::default()
        };
        policy.reconcile(prefs, &with_mic).unwrap();
        policy.reconcile(prefs, &AudioGraph::default()).unwrap();
        policy.reconcile(prefs, &with_mic).unwrap();
        drop(policy);
        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded, ["echo", "echo-drop", "echo", "echo-drop"]);
    }

    #[test]
    fn a_dead_echo_cancellation_client_is_restarted_after_a_backoff() {
        struct DyingRuntime(Arc<Mutex<Vec<String>>>);
        impl AudioRuntimeControl for DyingRuntime {
            fn capabilities(&self) -> RuntimeCapabilities {
                RuntimeCapabilities {
                    echo_cancellation: true,
                    ..RuntimeCapabilities::default()
                }
            }
            fn set_balance(&self, _: &str, _: f32, _: f32) -> Result<(), String> {
                Ok(())
            }
            fn start_mono(&self, _: &str) -> Result<Box<dyn BoundedSession>, String> {
                unreachable!()
            }
            fn start_channel_test(
                &self,
                _: &str,
                _: bool,
            ) -> Result<Box<dyn BoundedSession>, String> {
                unreachable!()
            }
            fn start_microphone_meter(
                &self,
                _: &str,
                _: Duration,
            ) -> Result<Box<dyn MicrophoneMeterSession>, String> {
                unreachable!()
            }
            fn start_echo_cancellation(&self) -> Result<Box<dyn BoundedSession>, String> {
                self.0.lock().unwrap().push("echo".into());
                Ok(Box::new(TestSession {
                    calls: self.0.clone(),
                    label: "echo",
                    alive: Arc::new(Mutex::new(false)),
                }))
            }
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut policy = PersistentAudioPolicy::new(Arc::new(DyingRuntime(calls.clone())));
        let graph = AudioGraph {
            sources: vec![microphone()],
            ..AudioGraph::default()
        };
        let prefs = PersistentAudioPreferences::default();
        policy.reconcile(prefs, &graph).unwrap();
        policy.reconcile(prefs, &graph).unwrap();
        policy.reconcile(prefs, &graph).unwrap();
        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded, ["echo", "echo-drop"]);
        assert!(!policy.echo_cancellation_active());
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
            ..PersistentAudioPreferences::default()
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
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        {
            let _s = ChildSession {
                child: Some(child),
                deadline: Some(Instant::now() + Duration::from_secs(30)),
            };
        }
        let alive = i32::try_from(pid).is_ok_and(|pid| unsafe { libc::kill(pid, 0) } == 0);
        assert!(!alive);
    }
}
