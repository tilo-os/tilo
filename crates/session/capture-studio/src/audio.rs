//! The machine's own sound and the microphone, as a screen recording takes
//! them.
//!
//! The session already treats `PipeWire`'s reference tools as its stable
//! graph boundary (`wpctl` for routing, `pw-dump` for privacy attribution).
//! Recording follows that same boundary: resolve the current default sink or
//! source with `wpctl`, then ask `pw-record` for raw 48 kHz stereo S16LE and
//! let `PipeWire` mix and resample. The frame loop never blocks on the audio
//! process; a bounded reader drops stale chunks if encoding stalls, and
//! pause drains instead of replaying sound captured while paused. Drop
//! terminates the recorder's whole process group and joins its reader.

use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use ui::widget::AudioCaptureMode;

/// Channels: stereo.
const CHANNELS: usize = 2;
/// Bits per sample: 16.
const BYTES_PER_SAMPLE: usize = 2;
/// One sample of both channels: what a reader and the muxer align on.
pub const SAMPLE_FRAME_BYTES: usize = CHANNELS * BYTES_PER_SAMPLE;
/// The low bits a length must drop to end on a whole sample frame.
const SAMPLE_FRAME_MASK: usize = SAMPLE_FRAME_BYTES - 1;
/// The rate every recording track is captured at before the muxer takes it.
pub const SAMPLE_RATE: usize = 48_000;
const PIPE_BUFFER: usize = 16 * 1024;
/// Chunks the reader may get ahead of the frame loop: about a second of
/// sound, after which stale chunks are dropped rather than stalling the
/// session.
const QUEUED_CHUNKS: usize = 12;
const PROBE_OUTPUT_LIMIT: usize = 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether the machine's own sound and a microphone can be recorded: the
/// `pw-record` reference tool is installed and `PipeWire` offers each default
/// device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Availability {
    pub system: bool,
    pub microphone: bool,
}

/// One of the two sounds a recording can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// What the machine plays: the default sink's monitor.
    System,
    Microphone,
}

impl Source {
    /// The name the recording's track carries.
    #[must_use]
    pub fn track_name(self) -> &'static str {
        match self {
            Self::System => "Desktop sound",
            Self::Microphone => "Microphone",
        }
    }

    fn default_node(self) -> media::audio::default_node::DefaultNode {
        match self {
            Self::System => media::audio::default_node::DefaultNode::Sink,
            Self::Microphone => media::audio::default_node::DefaultNode::Source,
        }
    }

    /// The stream's properties: named, so the person sees what is recording,
    /// and capturing the sink's monitor rather than a source's input for the
    /// machine's own sound.
    fn properties(self) -> &'static str {
        match self {
            Self::System => {
                "{ stream.capture.sink = true node.name = tilo-screen-recorder-desktop node.description = \"Tilo Desktop sound\" }"
            }
            Self::Microphone => {
                "{ node.name = tilo-screen-recorder-microphone node.description = \"Tilo microphone\" }"
            }
        }
    }
}

/// What `pw-record` is asked for, whatever an app or a caller would rather
/// have: the muxer's lossless stereo 48 kHz tracks and nothing else.
const FORMAT_ARGS: [&str; 6] = [
    "--rate",
    "48000",
    "--channels",
    "2",
    "--channel-map",
    "Stereo",
];

/// The sources `mode` records, system sound first.
#[must_use]
pub fn sources(mode: AudioCaptureMode) -> Vec<Source> {
    let mut sources = Vec::with_capacity(2);
    if mode.includes_system() {
        sources.push(Source::System);
    }
    if mode.includes_microphone() {
        sources.push(Source::Microphone);
    }
    sources
}

/// What this machine can record, probed once: the person's bar offers only
/// the sounds that would start.
#[must_use]
pub fn probe() -> Availability {
    if !pw_record_works() {
        return Availability::default();
    }
    Availability {
        system: default_target(Source::System).is_ok(),
        microphone: default_target(Source::Microphone).is_ok(),
    }
}

/// One recording track: a source and its open `pw-record`.
pub struct Track {
    source: Source,
    capture: Capture,
    mux: usize,
}

impl Track {
    /// Open every source `mode` asks for, in muxing order: system sound
    /// first, then the microphone.
    ///
    /// # Errors
    ///
    /// When a source cannot be resolved or its recorder cannot be started;
    /// a track already open is closed again.
    pub fn start(mode: AudioCaptureMode) -> Result<Vec<Self>, String> {
        sources(mode)
            .into_iter()
            .enumerate()
            .map(|(mux, source)| {
                Capture::start(source).map(|capture| Self {
                    source,
                    capture,
                    mux,
                })
            })
            .collect()
    }

    /// The track's name in the file.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.source.track_name()
    }

    /// The track's index in the file's audio tracks.
    #[must_use]
    pub fn mux(&self) -> usize {
        self.mux
    }

    /// Take every sample captured since the last drain. With `keep` false
    /// (pause) they are discarded, so the recording resumes at the current
    /// sound rather than appending what the pause heard.
    ///
    /// # Errors
    ///
    /// When the recorder ended or its pipe broke.
    pub fn drain(&mut self, keep: bool) -> Result<Vec<u8>, String> {
        self.capture.drain(keep)
    }
}

/// An open `pw-record` for one source.
struct Capture {
    child: Child,
    receiver: Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
    pending: Vec<u8>,
}

impl Capture {
    fn start(source: Source) -> Result<Self, String> {
        let target = default_target(source)?;
        let mut recorder = Command::new("pw-record");
        recorder
            .args(["--target", &target, "--properties", source.properties()])
            .args(FORMAT_ARGS)
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
        let mut child = recorder
            .spawn()
            .map_err(|error| format!("start PipeWire {} capture: {error}", source.track_name()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "PipeWire recorder did not expose an audio pipe".to_string())?;
        let (sender, receiver) = sync_channel(QUEUED_CHUNKS);
        let reader = match std::thread::Builder::new()
            .name(format!(
                "tilo-screen-recording-{}",
                source.track_name().to_lowercase().replace(' ', "-")
            ))
            .spawn(move || read_audio(stdout, &sender))
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
            pending: Vec::new(),
        })
    }

    /// Drain every chunk currently produced, as whole stereo sample frames.
    ///
    /// # Errors
    ///
    /// When the recorder ended or its pipe broke.
    fn drain(&mut self, keep: bool) -> Result<Vec<u8>, String> {
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| format!("check PipeWire audio capture: {error}"))?
        {
            return Err(format!("PipeWire audio capture ended early ({status})"));
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
        let aligned = self.pending.len() & !SAMPLE_FRAME_MASK;
        let tail = self.pending.split_off(aligned);
        Ok(std::mem::replace(&mut self.pending, tail))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        bounded_command::terminate_tree(&mut self.child, Duration::ZERO);
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
            // Behind: stale sound is dropped, never the person's frame loop.
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
}

fn pw_record_works() -> bool {
    bounded_command::run(
        "pw-record",
        &["--version"],
        None,
        PROBE_OUTPUT_LIMIT,
        PROBE_TIMEOUT,
    )
    .is_ok_and(|output| output.status.success())
}

fn default_target(source: Source) -> Result<String, String> {
    media::audio::default_node::object_serial(source.default_node())
        .map_err(|error| format!("default {}: {error}", source.track_name()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_test_env::SpawnUnderEnvLock as _;

    #[test]
    fn the_recording_takes_the_sounds_the_person_chose_as_stereo_tracks() {
        assert_eq!(
            sources(AudioCaptureMode::SystemAndMicrophone),
            [Source::System, Source::Microphone],
            "the machine's sound first, so its track stays the same one"
        );
        assert_eq!(sources(AudioCaptureMode::Microphone), [Source::Microphone]);
        assert_eq!(sources(AudioCaptureMode::System), [Source::System]);
        assert!(sources(AudioCaptureMode::None).is_empty());
        assert_eq!(Source::System.track_name(), "Desktop sound");
        assert_eq!(Source::Microphone.track_name(), "Microphone");
        assert_eq!(
            FORMAT_ARGS,
            [
                "--rate",
                "48000",
                "--channels",
                "2",
                "--channel-map",
                "Stereo"
            ]
        );
        assert_eq!(
            SAMPLE_RATE, 48_000,
            "the muxer's tracks are 48 kHz stereo S16LE"
        );
        assert_eq!(SAMPLE_FRAME_BYTES, 4);
    }

    #[test]
    fn pipewire_sources_come_from_server_assigned_identity() {
        let desktop = Source::System.properties();
        let microphone = Source::Microphone.properties();
        assert!(desktop.contains("stream.capture.sink = true"));
        assert!(desktop.contains("node.name = tilo-screen-recorder-desktop"));
        assert!(!microphone.contains("stream.capture.sink"));
        assert!(microphone.contains("node.name = tilo-screen-recorder-microphone"));
    }

    /// A capture over a pipe the test feeds, with a process that stays alive
    /// until it drops.
    fn fed_capture() -> (Capture, SyncSender<Vec<u8>>) {
        // A process that stays alive until the capture drops, so draining
        // sees a running recorder.
        let child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn_locked()
            .unwrap();
        let (sender, receiver) = sync_channel(QUEUED_CHUNKS);
        (
            Capture {
                child,
                receiver,
                reader: None,
                pending: Vec::new(),
            },
            sender,
        )
    }

    #[test]
    fn pause_discards_what_it_heard_and_each_take_is_whole_sample_frames() {
        let (mut capture, sender) = fed_capture();
        sender.send(vec![1; SAMPLE_FRAME_BYTES * 2]).unwrap();
        sender
            .send(vec![2; SAMPLE_FRAME_BYTES + BYTES_PER_SAMPLE])
            .unwrap();
        // Paused: everything captured so far is discarded, not replayed.
        assert!(capture.drain(false).unwrap().is_empty());
        assert!(capture.pending.is_empty());
        sender.send(vec![3; SAMPLE_FRAME_BYTES]).unwrap();
        assert_eq!(capture.drain(true).unwrap(), vec![3; SAMPLE_FRAME_BYTES]);
        assert!(capture.drain(true).unwrap().is_empty());
        // Half a stereo sample is held back until its other half arrives.
        sender.send(vec![4; BYTES_PER_SAMPLE]).unwrap();
        assert!(capture.drain(true).unwrap().is_empty());
        assert_eq!(capture.pending.len(), BYTES_PER_SAMPLE);
        sender.send(vec![4; BYTES_PER_SAMPLE]).unwrap();
        assert_eq!(capture.drain(true).unwrap(), vec![4; SAMPLE_FRAME_BYTES]);
    }

    #[test]
    fn a_recorder_that_ended_is_reported_rather_than_silently_recorded() {
        let mut child = Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn_locked()
            .unwrap();
        child.wait().unwrap();
        let (_sender, receiver) = sync_channel(1);
        let mut capture = Capture {
            child,
            receiver,
            reader: None,
            pending: Vec::new(),
        };
        assert!(capture.drain(true).is_err());
    }

    #[test]
    fn overflow_drops_stale_sound_instead_of_stalling_the_frame_loop() {
        let (sender, receiver) = sync_channel(1);
        let audio = std::io::Cursor::new(vec![0u8; PIPE_BUFFER.saturating_mul(3)]);
        read_audio(audio, &sender);
        assert_eq!(receiver.try_recv().unwrap().len(), PIPE_BUFFER);
        assert!(
            receiver.try_recv().is_err(),
            "the chunks after a full queue never block the reader"
        );
    }
}
