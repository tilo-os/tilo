//! One `pw-record` capture, for every sound this device records: a screen
//! recording's tracks, a dictation's microphone, a level meter.
//!
//! `pw-record` gives raw 16-bit samples on stdout (`--raw`, or it writes a WAV
//! header) and lets `PipeWire` mix and resample. One capture is one process: its own
//! process group, bound to its owner's death, with a reader thread handing chunks
//! over a bounded queue. Callers differ in a [`Spec`] and in what a reader that falls
//! behind does ([`Overflow`]), never in the number of captures.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use zeroize::{Zeroize as _, Zeroizing};

const CHUNK_BYTES: usize = 16 * 1024;
const QUEUED_CHUNKS: usize = 12;
const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u8,
}

impl Format {
    pub const STEREO_48K: Self = Self {
        rate: 48_000,
        channels: 2,
    };
    pub const MONO_16K: Self = Self {
        rate: 16_000,
        channels: 1,
    };

    #[must_use]
    pub const fn frame_bytes(self) -> usize {
        (self.channels as usize).saturating_mul(2)
    }

    fn channel_map(self) -> Option<&'static str> {
        match self.channels {
            1 => Some("mono"),
            2 => Some("Stereo"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    /// Drop the stale chunk and keep recording: the consumer is drawing or
    /// mixing, and it must never wait for sound.
    Drop,
    /// End the capture as interrupted: a recognizer must never be handed a
    /// gap and invent the words that were in it.
    Fail,
}

#[derive(Clone, Copy, Debug)]
pub struct Spec<'a> {
    pub target: &'a str,
    /// The stream's properties, named so the person sees what is recording.
    pub properties: Option<&'a str>,
    pub format: Format,
    pub overflow: Overflow,
    pub label: &'static str,
}

#[must_use]
pub fn available() -> bool {
    bounded_command::run(
        "pw-record",
        &["--version"],
        None,
        PROBE_OUTPUT_LIMIT,
        PROBE_TIMEOUT,
    )
    .is_ok_and(|output| output.status.success())
}

fn arguments(spec: &Spec<'_>) -> Result<Vec<String>, String> {
    let channel_map = spec.format.channel_map().ok_or_else(|| {
        format!(
            "{} interleaved channels are not a PipeWire layout",
            spec.format.channels
        )
    })?;
    let mut args = vec![
        "--target".to_owned(),
        spec.target.to_owned(),
        "--rate".to_owned(),
        spec.format.rate.to_string(),
        "--channels".to_owned(),
        spec.format.channels.to_string(),
        "--channel-map".to_owned(),
        channel_map.to_owned(),
    ];
    if let Some(properties) = spec.properties {
        args.push("--properties".to_owned());
        args.push(properties.to_owned());
    }
    args.extend(
        [
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
        ]
        .map(str::to_owned),
    );
    Ok(args)
}

pub struct Capture {
    child: Child,
    reader: Option<JoinHandle<()>>,
}

impl Capture {
    /// Start the recorder for `spec`, and the stream its samples come on.
    ///
    /// # Errors
    ///
    /// When the arguments name no layout, `pw-record` cannot be started, or
    /// its reader thread cannot be spawned; a recorder already started is
    /// killed again.
    pub fn start(spec: &Spec<'_>) -> Result<(Self, Stream), String> {
        let args = arguments(spec)?;
        let mut command = Command::new("pw-record");
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        bounded_command::own_process_tree(&mut command);
        let mut command = bind_to_owner(command);
        let mut child = command
            .spawn()
            .map_err(|error| format!("start PipeWire capture of {}: {error}", spec.label))?;
        let Some(pipe) = child.stdout.take() else {
            bounded_command::terminate_tree(&mut child, Duration::ZERO);
            return Err("PipeWire recorder did not expose an audio pipe".to_owned());
        };
        let (sender, chunks) = sync_channel(QUEUED_CHUNKS);
        let interrupted = Arc::new(AtomicBool::new(false));
        let reading = Arc::clone(&interrupted);
        let overflow = spec.overflow;
        let reader = std::thread::Builder::new()
            .name(format!(
                "tilo-capture-{}",
                spec.label.to_lowercase().replace(' ', "-")
            ))
            .spawn(move || read_audio(pipe, &sender, overflow, &reading));
        let reader = match reader {
            Ok(reader) => reader,
            Err(error) => {
                bounded_command::terminate_tree(&mut child, Duration::ZERO);
                return Err(format!(
                    "start the reader of {} capture: {error}",
                    spec.label
                ));
            }
        };
        Ok((
            Self {
                child,
                reader: Some(reader),
            },
            Stream {
                chunks,
                pending: Zeroizing::new(Vec::new()),
                frame_bytes: spec.format.frame_bytes(),
                interrupted,
                label: spec.label,
            },
        ))
    }

    /// Whether the recorder still runs.
    pub fn alive(&mut self) -> bool {
        self.child.try_wait().is_ok_and(|status| status.is_none())
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

pub struct Stream {
    chunks: Receiver<Zeroizing<Vec<u8>>>,
    /// Sound taken from the queue that is not yet a whole number of samples.
    pending: Zeroizing<Vec<u8>>,
    frame_bytes: usize,
    interrupted: Arc<AtomicBool>,
    label: &'static str,
}

impl Stream {
    /// Every whole sample captured since the last call.
    ///
    /// # Errors
    ///
    /// When the audio was interrupted: a reader that fell behind where its
    /// caller may not lose sound, or the recorder ending. A capture is
    /// continuous until its owner stops it, so the end of the pipe is never
    /// reported as silence.
    pub fn take(&mut self) -> Result<Vec<u8>, String> {
        if self.interrupted.load(Ordering::Acquire) {
            self.pending.zeroize();
            return Err(self.ended());
        }
        while let Ok(chunk) = self.chunks.try_recv() {
            self.pending.extend_from_slice(&chunk);
        }
        if self.interrupted.load(Ordering::Acquire) {
            self.pending.zeroize();
            return Err(self.ended());
        }
        let frames = self
            .pending
            .len()
            .checked_div(self.frame_bytes)
            .unwrap_or(0);
        let aligned = frames.saturating_mul(self.frame_bytes);
        let tail = Zeroizing::new(self.pending.split_off(aligned));
        let mut whole = std::mem::replace(&mut self.pending, tail);
        Ok(std::mem::take(&mut *whole))
    }

    /// Let go of everything heard so far: a paused recording resumes at the
    /// current sound instead of appending what the pause heard.
    pub fn discard(&mut self) {
        while self.chunks.try_recv().is_ok() {}
        self.pending.zeroize();
    }

    /// The next chunk, waiting until there is one; `None` once the recorder
    /// ended.
    pub fn next_chunk(&mut self) -> Option<Zeroizing<Vec<u8>>> {
        self.chunks.recv().ok()
    }

    fn ended(&self) -> String {
        format!("PipeWire capture of {} ended", self.label)
    }
}

/// Hand every chunk read from `pipe` on, until the pipe ends. With
/// [`Overflow::Fail`] a full queue ends the capture; with [`Overflow::Drop`]
/// the stale chunk goes.
fn read_audio(
    mut pipe: impl Read,
    sender: &SyncSender<Zeroizing<Vec<u8>>>,
    overflow: Overflow,
    interrupted: &AtomicBool,
) {
    let mut buffer = Zeroizing::new(vec![0u8; CHUNK_BYTES]);
    while let Ok(read) = pipe.read(&mut buffer) {
        if read == 0 {
            break;
        }
        let Some(bytes) = buffer.get(..read) else {
            break;
        };
        match sender.try_send(Zeroizing::new(bytes.to_vec())) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) if overflow == Overflow::Drop => {}
            Err(TrySendError::Full(_)) => {
                interrupted.store(true, Ordering::Release);
                return;
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
    // The microphone is continuous until its owner stops it. EOF or a read
    // failure can precede the process exiting; never report it as silence.
    interrupted.store(true, Ordering::Release);
}

/// A recorder must not survive its owner disappearing, even when Rust Drop
/// cannot run. The exec target is an ordinary image binary; no elevated or
/// capability-bearing program may replace it.
fn bind_to_owner(command: Command) -> Command {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        let mut command = command;
        let parent = unsafe { libc::getpid() };
        // SAFETY: this callback performs only async-signal-safe identity
        // queries and syscalls after fork. It allocates nothing and locks no
        // Rust state.
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
        command
    }
    #[cfg(not(target_os = "linux"))]
    {
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_test_env::SpawnUnderEnvLock as _;

    const STEREO: Spec<'_> = Spec {
        target: "42",
        properties: Some("{ node.name = test }"),
        format: Format::STEREO_48K,
        overflow: Overflow::Drop,
        label: "test recording",
    };

    #[test]
    fn every_capture_asks_pw_record_for_the_same_shape() {
        let args = arguments(&STEREO).unwrap();
        assert_eq!(
            args,
            [
                "--target",
                "42",
                "--rate",
                "48000",
                "--channels",
                "2",
                "--channel-map",
                "Stereo",
                "--properties",
                "{ node.name = test }",
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
            ]
        );
        assert_eq!(Format::STEREO_48K.frame_bytes(), 4);
        assert_eq!(Format::STEREO_48K.rate, 48_000);
        let mono = arguments(&Spec {
            target: "mic",
            properties: None,
            format: Format::MONO_16K,
            overflow: Overflow::Fail,
            label: "test dictation",
        })
        .unwrap();
        assert!(mono.windows(2).any(|pair| pair == ["--rate", "16000"]));
        assert!(mono.windows(2).any(|pair| pair == ["--channels", "1"]));
        assert!(
            mono.windows(2)
                .any(|pair| pair == ["--channel-map", "mono"])
        );
        assert!(!mono.iter().any(|arg| arg == "--properties"));
        assert_eq!(mono.last().map(String::as_str), Some("-"));
        assert_eq!(Format::MONO_16K.frame_bytes(), 2);
        let bad = arguments(&Spec {
            format: Format {
                rate: 48_000,
                channels: 3,
            },
            ..STEREO
        });
        assert!(bad.is_err());
    }

    fn stream(frame_bytes: usize) -> (Stream, SyncSender<Zeroizing<Vec<u8>>>, Arc<AtomicBool>) {
        let (sender, chunks) = sync_channel(QUEUED_CHUNKS);
        let interrupted = Arc::new(AtomicBool::new(false));
        (
            Stream {
                chunks,
                pending: Zeroizing::new(Vec::new()),
                frame_bytes,
                interrupted: Arc::clone(&interrupted),
                label: "test",
            },
            sender,
            interrupted,
        )
    }

    #[test]
    fn pause_discards_what_it_heard_and_each_take_is_whole_sample_frames() {
        let (mut stream, sender, _interrupted) = stream(Format::STEREO_48K.frame_bytes());
        sender.send(Zeroizing::new(vec![1; 8])).unwrap();
        sender.send(Zeroizing::new(vec![2; 6])).unwrap();
        // Paused: everything captured so far is discarded, not replayed.
        stream.discard();
        assert!(stream.pending.is_empty());
        assert!(stream.take().unwrap().is_empty());
        sender.send(Zeroizing::new(vec![3; 4])).unwrap();
        assert_eq!(stream.take().unwrap(), vec![3; 4]);
        assert!(stream.take().unwrap().is_empty());
        // Half a stereo sample is held back until its other half arrives.
        sender.send(Zeroizing::new(vec![4; 2])).unwrap();
        assert!(stream.take().unwrap().is_empty());
        assert_eq!(stream.pending.len(), 2);
        sender.send(Zeroizing::new(vec![4; 2])).unwrap();
        assert_eq!(stream.take().unwrap(), vec![4; 4]);
    }

    #[test]
    fn the_end_of_the_pipe_is_reported_rather_than_recorded_as_silence() {
        let (sender, chunks) = sync_channel(QUEUED_CHUNKS);
        let interrupted = Arc::new(AtomicBool::new(false));
        read_audio(
            std::io::empty(),
            &sender,
            Overflow::Drop,
            &Arc::clone(&interrupted),
        );
        drop(sender);
        let mut stream = Stream {
            chunks,
            pending: Zeroizing::new(Vec::new()),
            frame_bytes: 4,
            interrupted,
            label: "test",
        };
        assert_eq!(stream.take().unwrap_err(), "PipeWire capture of test ended");
        assert!(stream.next_chunk().is_none());
    }

    #[test]
    fn a_reader_that_falls_behind_drops_sound_or_ends_the_capture() {
        let (sender, chunks) = sync_channel(1);
        let interrupted = AtomicBool::new(false);
        let source = std::io::Cursor::new(vec![0u8; CHUNK_BYTES.saturating_mul(3)]);
        read_audio(source, &sender, Overflow::Drop, &interrupted);
        assert_eq!(chunks.try_recv().unwrap().len(), CHUNK_BYTES);
        assert!(
            chunks.try_recv().is_err(),
            "the chunks after a full queue never block the reader"
        );

        let (sender, chunks) = sync_channel(1);
        let interrupted = AtomicBool::new(false);
        let mut source = std::io::Cursor::new(vec![0u8; CHUNK_BYTES.saturating_mul(3)]);
        read_audio(&mut source, &sender, Overflow::Fail, &interrupted);
        assert!(interrupted.load(Ordering::Acquire));
        assert_eq!(
            source.position(),
            u64::try_from(CHUNK_BYTES.saturating_mul(2)).unwrap(),
            "a capture that may not lose words stops at the overflow"
        );
        assert_eq!(chunks.try_recv().unwrap().len(), CHUNK_BYTES);
        assert!(chunks.try_recv().is_err());
    }

    #[test]
    fn a_capture_with_nobody_listening_ends_its_reader() {
        let (sender, chunks) = sync_channel(QUEUED_CHUNKS);
        drop(chunks);
        let interrupted = AtomicBool::new(false);
        let source = std::io::Cursor::new(vec![0u8; CHUNK_BYTES]);
        read_audio(source, &sender, Overflow::Drop, &interrupted);
        assert!(
            !interrupted.load(Ordering::Acquire),
            "an owner that left is not an interruption to report"
        );
    }

    #[test]
    fn dropping_a_capture_reaps_its_process_and_ends_its_stream() {
        let mut command = Command::new("cat");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        bounded_command::own_process_tree(&mut command);
        let mut child = command.spawn_locked().unwrap();
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        assert!(child.try_wait().unwrap().is_none());
        let pipe = child.stdout.take().unwrap();
        let (sender, chunks) = sync_channel(QUEUED_CHUNKS);
        let interrupted = Arc::new(AtomicBool::new(false));
        let reading = Arc::clone(&interrupted);
        let reader = std::thread::spawn(move || {
            read_audio(pipe, &sender, Overflow::Drop, &reading);
        });
        let capture = Capture {
            child,
            reader: Some(reader),
        };
        let mut stream = Stream {
            chunks,
            pending: Zeroizing::new(Vec::new()),
            frame_bytes: 2,
            interrupted,
            label: "test",
        };
        drop(capture);
        assert!(stream.next_chunk().is_none());
        // SAFETY: signal 0 only checks the PID of the child this test spawned;
        // it cannot send a signal or affect another process.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_recorder_does_not_survive_its_owner() {
        let mut recorder = bind_to_owner(Command::new("true"));
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut recorder, || {
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
}
