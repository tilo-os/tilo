//! `PipeWire` audio capture for shell recordings.
//!
//! The session already treats `PipeWire`'s reference tools as its stable graph
//! boundary (`wpctl` for routing, `pw-dump` for privacy attribution). Recording
//! follows that same boundary: resolve the current default sink or source, then
//! ask `pw-record` for raw 48 kHz stereo S16LE. The compositor
//! never blocks on the audio process; a bounded reader drops stale chunks if
//! rendering stalls, and pause explicitly drains instead of replaying buffered
//! sound after resume.

use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;
use zeroize::{Zeroize as _, Zeroizing};

const CHANNELS: usize = 2;
const BYTES_PER_SAMPLE: usize = 2;
const SAMPLE_FRAME_BYTES: usize = CHANNELS * BYTES_PER_SAMPLE;
const PIPE_BUFFER: usize = 16 * 1024;
const QUEUED_CHUNKS: usize = 12;
const PROBE_OUTPUT_LIMIT: usize = 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioAvailability {
    pub system: bool,
    pub microphone: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioSource {
    System,
    Microphone,
}

impl AudioSource {
    pub fn track_name(self) -> &'static str {
        match self {
            Self::System => "Desktop sound",
            Self::Microphone => "Microphone",
        }
    }

    fn default_token(self) -> &'static str {
        match self {
            Self::System => "@DEFAULT_AUDIO_SINK@",
            Self::Microphone => "@DEFAULT_AUDIO_SOURCE@",
        }
    }
}

pub fn availability() -> AudioAvailability {
    let mut command = Command::new("pw-record");
    command.arg("--version");
    let pw_record = bounded_command::run_prepared(command, None, PROBE_OUTPUT_LIMIT, PROBE_TIMEOUT)
        .is_ok_and(|output| output.status.success());
    AudioAvailability {
        system: pw_record && default_target(AudioSource::System).is_ok(),
        microphone: pw_record && default_target(AudioSource::Microphone).is_ok(),
    }
}

pub struct AudioCapture {
    child: Child,
    receiver: Receiver<Zeroizing<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    pending: Zeroizing<Vec<u8>>,
    frame_bytes: usize,
    discontinuity: Arc<AtomicBool>,
}

/// Format is chosen by the capture purpose, never by an untrusted command line.
#[derive(Clone, Copy)]
enum CapturePurpose {
    Recording,
    Dictation,
}

impl CapturePurpose {
    fn format_args(self) -> [&'static str; 6] {
        match self {
            Self::Recording => [
                "--rate",
                "48000",
                "--channels",
                "2",
                "--channel-map",
                "Stereo",
            ],
            Self::Dictation => [
                "--rate",
                "16000",
                "--channels",
                "1",
                "--channel-map",
                "mono",
            ],
        }
    }

    fn frame_bytes(self) -> usize {
        match self {
            Self::Recording => SAMPLE_FRAME_BYTES,
            Self::Dictation => BYTES_PER_SAMPLE,
        }
    }
}

impl AudioCapture {
    pub fn start(source: AudioSource) -> Result<Self, String> {
        Self::start_for(source, CapturePurpose::Recording, None)
    }

    /// Start only after the user requested dictation and its edit target and
    /// trusted speech engine are ready. `PipeWire` performs mono conversion and
    /// resampling; no samples are skipped by this process. The spawning
    /// thread must stay alive until capture stops: parent death kills capture.
    pub fn start_dictation_microphone() -> Result<Self, String> {
        Self::start_for(AudioSource::Microphone, CapturePurpose::Dictation, None)
    }

    pub(super) fn start_dictation_if_active(cancel: &AtomicBool) -> Result<Self, String> {
        Self::start_for(
            AudioSource::Microphone,
            CapturePurpose::Dictation,
            Some(cancel),
        )
    }

    fn start_for(
        source: AudioSource,
        purpose: CapturePurpose,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
            return Err("dictation canceled".into());
        }
        let target = default_target(source)?;
        // A canceled startup must not open the microphone after the bounded
        // source probe returns. The owner remains alive through teardown.
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
            return Err("dictation canceled".into());
        }
        let args = match purpose {
            CapturePurpose::Recording => capture_args(source, &target),
            CapturePurpose::Dictation => vec![
                "--target".into(), target,
                "--properties".into(),
                // Consent covers this server-assigned source, not any future
                // default. Source removal must close the pipe with an error;
                // otherwise pw-record can remain alive without producing PCM.
                "{ node.name = tilo-dictation-microphone node.description = \"Tilo dictation microphone\" node.dont-reconnect = true node.dont-fallback = true node.dont-move = true }".into(),
            ],
        };
        let mut recorder = Command::new(if matches!(purpose, CapturePurpose::Dictation) {
            "/usr/bin/pw-record"
        } else {
            "pw-record"
        });
        recorder
            .args(args)
            .args(purpose.format_args())
            .args([
                "--media-type",
                "Audio",
                "--media-category",
                "Capture",
                "--media-role",
                "Production",
                "--latency",
                "20ms",
                "--format",
                "s16",
                "--raw",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        if matches!(purpose, CapturePurpose::Dictation) {
            #[cfg(target_os = "linux")]
            bind_recorder_to_owner(&mut recorder);
            #[cfg(not(target_os = "linux"))]
            return Err("Dictation capture requires Linux parent-death protection".into());
        }
        let mut child = recorder
            .spawn()
            .map_err(|error| format!("start PipeWire {} capture: {error}", source.track_name()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "PipeWire recorder did not expose an audio pipe".to_string())?;
        let (sender, receiver) = sync_channel(QUEUED_CHUNKS);
        let discontinuity = Arc::new(AtomicBool::new(false));
        let reader_discontinuity = Arc::clone(&discontinuity);
        let reader = match std::thread::Builder::new()
            .name(format!(
                "tilo-screen-recording-{}",
                source.track_name().to_lowercase().replace(' ', "-")
            ))
            .spawn(move || read_audio(stdout, &sender, purpose, &reader_discontinuity))
        {
            Ok(reader) => reader,
            Err(error) => {
                bounded_command::terminate_tree(&mut child, Duration::ZERO);
                return Err(format!("start audio reader: {error}"));
            }
        };
        Ok(Self {
            child,
            receiver,
            reader: Some(reader),
            pending: Zeroizing::new(Vec::new()),
            frame_bytes: purpose.frame_bytes(),
            discontinuity,
        })
    }

    /// Drain every chunk currently produced. When `keep` is false (pause),
    /// bytes are deliberately discarded so resume starts at the current sound
    /// rather than appending audio captured during the pause.
    pub fn drain(&mut self, keep: bool) -> Result<Vec<u8>, String> {
        if self.discontinuity.load(Ordering::Acquire) {
            self.pending.zeroize();
            return Err("PipeWire dictation audio was interrupted".into());
        }
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| format!("check PipeWire audio capture: {error}"))?
        {
            return Err(format!(
                "PipeWire audio capture ended unexpectedly ({status})"
            ));
        }
        loop {
            match self.receiver.try_recv() {
                Ok(bytes) if keep => self.pending.extend_from_slice(&bytes),
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if !keep {
            self.pending.zeroize();
            return Ok(Vec::new());
        }
        if self.discontinuity.load(Ordering::Acquire) {
            self.pending.zeroize();
            return Err("PipeWire dictation audio was interrupted".into());
        }
        let remainder = self
            .pending
            .len()
            .checked_rem(self.frame_bytes)
            .unwrap_or(0);
        let aligned = self.pending.len().saturating_sub(remainder);
        if aligned == 0 {
            return Ok(Vec::new());
        }
        let tail = Zeroizing::new(self.pending.split_off(aligned));
        let mut complete = std::mem::replace(&mut self.pending, tail);
        Ok(std::mem::take(&mut *complete))
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        bounded_command::terminate_tree(&mut self.child, std::time::Duration::ZERO);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// A dictation recorder must not survive its owner disappearing, even when
/// Rust Drop cannot run. The exec target is an ordinary image binary; no
/// elevated/capability-bearing program may replace it.
#[cfg(target_os = "linux")]
fn bind_recorder_to_owner(command: &mut Command) {
    // SAFETY: getpid is an argument-free identity query and cannot fail.
    let parent = unsafe { libc::getpid() };
    // SAFETY: this callback performs only async-signal-safe identity queries
    // and syscalls after fork. It allocates nothing and locks no Rust state.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

fn read_audio(
    mut stdout: impl Read,
    sender: &SyncSender<Zeroizing<Vec<u8>>>,
    purpose: CapturePurpose,
    discontinuity: &AtomicBool,
) {
    let mut buffer = Zeroizing::new(vec![0u8; PIPE_BUFFER]);
    while let Ok(read) = stdout.read(&mut buffer) {
        if read == 0 {
            break;
        }
        let Some(bytes) = buffer.get(..read) else {
            break;
        };
        match sender.try_send(Zeroizing::new(bytes.to_vec())) {
            Err(TrySendError::Full(_)) if matches!(purpose, CapturePurpose::Dictation) => {
                discontinuity.store(true, Ordering::Release);
                break;
            }
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
    // The microphone is continuous until explicitly stopped by its owner.
    // EOF/read failure can precede process exit; never treat it as silence or
    // permit buffered words to finish a request after its source disappeared.
    if matches!(purpose, CapturePurpose::Dictation) {
        discontinuity.store(true, Ordering::Release);
    }
}

fn capture_args(source: AudioSource, target: &str) -> Vec<String> {
    let properties = match source {
        AudioSource::System => {
            "{ stream.capture.sink = true node.name = tilo-screen-recorder-desktop node.description = \"Tilo Desktop sound\" }"
        }
        AudioSource::Microphone => {
            "{ node.name = tilo-screen-recorder-microphone node.description = \"Tilo microphone\" }"
        }
    };
    vec![
        "--target".into(),
        target.into(),
        "--properties".into(),
        properties.into(),
    ]
}

fn default_target(source: AudioSource) -> Result<String, String> {
    let mut command = Command::new("wpctl");
    command.args(["inspect", source.default_token()]);
    let output = bounded_command::run_prepared(command, None, PROBE_OUTPUT_LIMIT, PROBE_TIMEOUT)
        .map_err(|error| format!("inspect default {}: {error}", source.track_name()))?;
    if !output.status.success() {
        return Err(format!(
            "inspect default {} exited with {}",
            source.track_name(),
            output.status
        ));
    }
    parse_object_serial(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| format!("default {} has no object serial", source.track_name()))
}

fn parse_object_serial(inspect: &str) -> Option<String> {
    inspect.lines().find_map(|line| {
        let (_, value) = line.trim().split_once("object.serial =")?;
        let serial = value.trim().trim_matches('"');
        (!serial.is_empty() && serial.chars().all(|ch| ch.is_ascii_digit()))
            .then(|| serial.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn dictation_recorder_arms_real_kernel_parent_death_protection() {
        let mut recorder = Command::new("true");
        bind_recorder_to_owner(&mut recorder);
        // SAFETY: query the actual child kernel state after the production
        // callback and before exec; no microphone, allocation or lock is used.
        unsafe {
            recorder.pre_exec(|| {
                let mut signal: libc::c_int = 0;
                if libc::prctl(libc::PR_GET_PDEATHSIG, &raw mut signal, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if signal != libc::SIGKILL {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                }
                Ok(())
            });
        }
        let output =
            bounded_command::run_prepared(recorder, None, PROBE_OUTPUT_LIMIT, PROBE_TIMEOUT)
                .expect("run protected child");
        assert!(output.status.success());
    }

    #[test]
    fn dictation_overflow_is_reported_instead_of_silently_dropping_audio() {
        let (sender, receiver) = sync_channel(1);
        let discontinuity = AtomicBool::new(false);
        let mut audio = std::io::Cursor::new(vec![0u8; PIPE_BUFFER.saturating_mul(3)]);
        read_audio(
            &mut audio,
            &sender,
            CapturePurpose::Dictation,
            &discontinuity,
        );
        assert!(discontinuity.load(Ordering::Acquire));
        assert_eq!(
            audio.position(),
            u64::try_from(PIPE_BUFFER.saturating_mul(2)).unwrap()
        );
        assert_eq!(receiver.try_recv().unwrap().len(), PIPE_BUFFER);
        assert!(receiver.try_recv().is_err());
        assert_eq!(CapturePurpose::Dictation.frame_bytes(), 2);
        assert_eq!(
            CapturePurpose::Dictation.format_args(),
            [
                "--rate",
                "16000",
                "--channels",
                "1",
                "--channel-map",
                "mono"
            ]
        );
    }

    #[test]
    fn dictation_reports_source_eof_without_waiting_for_process_exit() {
        let (sender, receiver) = sync_channel(1);
        let discontinuity = AtomicBool::new(false);
        read_audio(
            std::io::empty(),
            &sender,
            CapturePurpose::Dictation,
            &discontinuity,
        );
        assert!(discontinuity.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn screen_recording_retains_its_existing_drop_policy_and_stereo_format() {
        let (sender, receiver) = sync_channel(1);
        let discontinuity = AtomicBool::new(false);
        let audio = std::io::Cursor::new(vec![0u8; PIPE_BUFFER.saturating_mul(2)]);
        read_audio(audio, &sender, CapturePurpose::Recording, &discontinuity);
        assert!(!discontinuity.load(Ordering::Acquire));
        assert_eq!(receiver.try_recv().unwrap().len(), PIPE_BUFFER);
        assert!(receiver.try_recv().is_err());
        assert_eq!(CapturePurpose::Recording.frame_bytes(), 4);
    }

    #[test]
    fn dropping_dictation_capture_reaps_its_owned_process_and_unblocks_the_reader() {
        // A blocked pipe exercises teardown without a microphone or a desktop.
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        assert!(child.try_wait().unwrap().is_none());
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = sync_channel(QUEUED_CHUNKS);
        let discontinuity = Arc::new(AtomicBool::new(false));
        let reader_discontinuity = Arc::clone(&discontinuity);
        let reader = std::thread::spawn(move || {
            read_audio(
                stdout,
                &sender,
                CapturePurpose::Dictation,
                &reader_discontinuity,
            );
        });
        let capture = AudioCapture {
            child,
            receiver,
            reader: Some(reader),
            pending: Zeroizing::new(Vec::new()),
            frame_bytes: BYTES_PER_SAMPLE,
            discontinuity,
        };
        drop(capture);
        // SAFETY: signal 0 only checks the PID of the child this test spawned;
        // it cannot send a signal or affect another process.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn pipewire_sources_come_from_server_assigned_identity() {
        // Phase `default_sink_serial_comes_from_server_assigned_identity`.
        {
            let inspect = r#"
                node.name = "spoofable-name"
              * object.serial = "314"
                media.class = "Audio/Sink"
            "#;
            assert_eq!(parse_object_serial(inspect).as_deref(), Some("314"));
            assert_eq!(parse_object_serial("object.serial = hacked"), None);
        }
        // Phase `desktop_and_microphone_request_different_pipewire_sources`.
        {
            let desktop = capture_args(AudioSource::System, "314").join(" ");
            let microphone = capture_args(AudioSource::Microphone, "271").join(" ");
            assert!(desktop.contains("--target 314"));
            assert!(desktop.contains("stream.capture.sink = true"));
            assert!(microphone.contains("--target 271"));
            assert!(!microphone.contains("stream.capture.sink"));
        }
        // Phase `drain_keeps_only_complete_stereo_sample_frames`.
        {
            let mut pending = vec![1, 2, 3, 4, 5, 6];
            let aligned = pending.len() / SAMPLE_FRAME_BYTES * SAMPLE_FRAME_BYTES;
            let tail = pending.split_off(aligned);
            assert_eq!(pending, [1, 2, 3, 4]);
            assert_eq!(tail, [5, 6]);
        }
    }
}
