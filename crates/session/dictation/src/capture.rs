//! The microphone a dictation records with: `pw-record` on the `PipeWire`
//! microphone the machine has as its default source, 16 kHz mono S16LE, read
//! by a bounded reader thread.
//!
//! Nothing here opens the microphone on its own: [`Microphone::open`] is
//! called only after the person allowed this dictation, and it re-checks that
//! permission after the source probe, so a cancellation during the probe
//! cannot leave a recording running. Queue overflow and the source
//! disappearing are a *discontinuity* — a fatal error, never a silent gap
//! from which a recognizer could manufacture missing words. Drop terminates
//! the recorder's whole process group and joins its reader.

use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use zeroize::{Zeroize as _, Zeroizing};

const BYTES_PER_SAMPLE: usize = 2;
const PIPE_BUFFER: usize = 16 * 1024;
const QUEUED_CHUNKS: usize = 12;
/// What the device-name probe may say and take in a test fixture.
#[cfg(test)]
const PROBE_OUTPUT_LIMIT: usize = 1024 * 1024;
#[cfg(test)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What `pw-record` is asked for, whatever an app or a caller would rather
/// have: a recognizer wants 16 kHz mono samples and nothing else.
const FORMAT_ARGS: [&str; 6] = [
    "--rate", "16000", "--channels", "1", "--channel-map", "mono",
];

/// The source a dictation records from, named by `PipeWire` itself so that a
/// source removed before the stream starts cannot silently fall back to
/// another microphone.
const STREAM_PROPERTIES: &str = "{ node.name = tilo-dictation-microphone node.description = \"Tilo dictation microphone\" node.dont-reconnect = true node.dont-fallback = true node.dont-move = true }";

/// An open dictation microphone.
pub struct Microphone {
    child: Child,
    receiver: Receiver<Zeroizing<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    pending: Zeroizing<Vec<u8>>,
    frame_bytes: usize,
    discontinuity: Arc<AtomicBool>,
}

impl Microphone {
    /// Open the default microphone, unless `cancel` is set: before the source
    /// probe and again after it. The spawning thread must stay alive until
    /// capture stops — parent death kills the recorder.
    ///
    /// # Errors
    /// When dictation was cancelled, the source could not be resolved, or the
    /// recorder could not be started.
    pub fn open(cancel: &AtomicBool) -> Result<Self, String> {
        if cancel.load(Ordering::Acquire) {
            return Err("dictation canceled".into());
        }
        let target = default_source()?;
        if cancel.load(Ordering::Acquire) {
            return Err("dictation canceled".into());
        }
        let mut recorder = Command::new("/usr/bin/pw-record");
        recorder
            .args(["--target", &target, "--properties", STREAM_PROPERTIES])
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
        #[cfg(target_os = "linux")]
        bind_recorder_to_owner(&mut recorder);
        #[cfg(not(target_os = "linux"))]
        return Err("dictation capture requires Linux parent-death protection".into());
        let mut child = recorder
            .spawn()
            .map_err(|error| format!("start PipeWire dictation capture: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "PipeWire recorder did not expose an audio pipe".to_string())?;
        let (sender, receiver) = sync_channel(QUEUED_CHUNKS);
        let discontinuity = Arc::new(AtomicBool::new(false));
        let reader_discontinuity = Arc::clone(&discontinuity);
        let reader = match std::thread::Builder::new()
            .name("tilo-dictation-microphone".into())
            .spawn(move || read_audio(stdout, &sender, &reader_discontinuity))
        {
            Ok(reader) => reader,
            Err(error) => {
                bounded_command::terminate_tree(&mut child, Duration::ZERO);
                return Err(format!("start dictation audio reader: {error}"));
            }
        };
        Ok(Self {
            child,
            receiver,
            reader: Some(reader),
            pending: Zeroizing::new(Vec::new()),
            frame_bytes: BYTES_PER_SAMPLE,
            discontinuity,
        })
    }

    /// Every complete sample produced since the last call. Interrupted audio
    /// is an error: a caller stops dictation rather than guessing.
    ///
    /// # Errors
    /// When the audio queue overflowed, the source ended, or the recognizer
    /// fell behind the recorder.
    pub fn drain(&mut self) -> Result<Vec<u8>, String> {
        if self.discontinuity.load(Ordering::Acquire) {
            self.pending.zeroize();
            return Err("PipeWire dictation audio was interrupted".into());
        }
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| format!("check PipeWire dictation capture: {error}"))?
        {
            return Err(format!(
                "PipeWire dictation capture ended unexpectedly ({status})"
            ));
        }
        while let Ok(bytes) = self.receiver.try_recv() {
            self.pending.extend_from_slice(&bytes);
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

impl Drop for Microphone {
    fn drop(&mut self) {
        bounded_command::terminate_tree(&mut self.child, Duration::ZERO);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// A dictation recorder must not survive its owner disappearing, even when
/// Rust Drop cannot run. The exec target is an ordinary image binary; no
/// elevated or capability-bearing program may replace it.
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
            Err(TrySendError::Full(_)) => {
                // The recognizer is not keeping up: fail, never drop words.
                discontinuity.store(true, Ordering::Release);
                break;
            }
            Ok(()) => {}
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
    // The microphone is continuous until its owner stops it. EOF or a read
    // failure can precede the process exiting; never treat it as silence.
    discontinuity.store(true, Ordering::Release);
}

/// The default source's `object.serial`, resolved through `wpctl`.
fn default_source() -> Result<String, String> {
    media::audio::default_node::object_serial(media::audio::default_node::DefaultNode::Source)
        .map_err(|error| format!("default microphone: {error}"))
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
    fn overflow_is_reported_instead_of_silently_dropping_audio() {
        let (sender, receiver) = sync_channel(1);
        let discontinuity = AtomicBool::new(false);
        let mut audio = std::io::Cursor::new(vec![0u8; PIPE_BUFFER.saturating_mul(3)]);
        read_audio(&mut audio, &sender, &discontinuity);
        assert!(discontinuity.load(Ordering::Acquire));
        assert_eq!(
            audio.position(),
            u64::try_from(PIPE_BUFFER.saturating_mul(2)).unwrap()
        );
        assert_eq!(receiver.try_recv().unwrap().len(), PIPE_BUFFER);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn source_eof_is_reported_without_waiting_for_process_exit() {
        let (sender, receiver) = sync_channel(1);
        let discontinuity = AtomicBool::new(false);
        read_audio(std::io::empty(), &sender, &discontinuity);
        assert!(discontinuity.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn sixteen_kilohertz_mono_samples() {
        assert_eq!(BYTES_PER_SAMPLE, 2);
        assert_eq!(
            FORMAT_ARGS,
            [
                "--rate",
                "16000",
                "--channels",
                "1",
                "--channel-map",
                "mono"
            ]
        );
        let mut pending = vec![1, 2, 3, 4, 5, 6];
        let remainder = pending.len().checked_rem(BYTES_PER_SAMPLE).unwrap_or(0);
        let aligned = pending.len().saturating_sub(remainder);
        let tail = pending.split_off(aligned);
        assert_eq!(pending, [1, 2, 3, 4, 5, 6]);
        assert!(tail.is_empty());
    }

    #[test]
    fn dropping_capture_reaps_its_owned_process_and_unblocks_the_reader() {
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
            read_audio(stdout, &sender, &reader_discontinuity);
        });
        let capture = Microphone {
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
    fn the_stream_is_bound_to_the_source_pipewire_handed_out() {
        assert!(STREAM_PROPERTIES.contains("node.dont-fallback = true"));
        assert!(STREAM_PROPERTIES.contains("node.dont-reconnect = true"));
        assert!(STREAM_PROPERTIES.contains("node.name = tilo-dictation-microphone"));
    }
}
