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
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

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
    receiver: Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
    pending: Vec<u8>,
}

impl AudioCapture {
    pub fn start(source: AudioSource) -> Result<Self, String> {
        let target = default_target(source)?;
        let args = capture_args(source, &target);
        let mut child = Command::new("pw-record")
            .args(args)
            .args([
                "--media-type",
                "Audio",
                "--media-category",
                "Capture",
                "--media-role",
                "Production",
                "--latency",
                "20ms",
                "--rate",
                "48000",
                "--channels",
                "2",
                "--channel-map",
                "Stereo",
                "--format",
                "s16",
                "--raw",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|error| format!("start PipeWire {} capture: {error}", source.track_name()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "PipeWire recorder did not expose an audio pipe".to_string())?;
        let (sender, receiver) = sync_channel(QUEUED_CHUNKS);
        let reader = std::thread::Builder::new()
            .name(format!(
                "tilo-screen-recording-{}",
                source.track_name().to_lowercase().replace(' ', "-")
            ))
            .spawn(move || read_audio(stdout, &sender))
            .map_err(|error| format!("start audio reader: {error}"))?;
        Ok(Self {
            child,
            receiver,
            reader: Some(reader),
            pending: Vec::new(),
        })
    }

    /// Drain every chunk currently produced. When `keep` is false (pause),
    /// bytes are deliberately discarded so resume starts at the current sound
    /// rather than appending audio captured during the pause.
    pub fn drain(&mut self, keep: bool) -> Result<Vec<u8>, String> {
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
            self.pending.clear();
            return Ok(Vec::new());
        }
        let aligned = self
            .pending
            .len()
            .saturating_sub(self.pending.len() % SAMPLE_FRAME_BYTES);
        if aligned == 0 {
            return Ok(Vec::new());
        }
        let tail = self.pending.split_off(aligned);
        Ok(std::mem::replace(&mut self.pending, tail))
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

fn read_audio(mut stdout: impl Read, sender: &SyncSender<Vec<u8>>) {
    let mut buffer = vec![0u8; PIPE_BUFFER];
    while let Ok(read) = stdout.read(&mut buffer) {
        if read == 0 {
            break;
        }
        let Some(bytes) = buffer.get(..read) else {
            break;
        };
        match sender.try_send(bytes.to_vec()) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => break,
        }
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
