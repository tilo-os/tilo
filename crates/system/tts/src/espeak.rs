//! Direct `espeak-ng` subprocess backend — the low-latency fallback used when
//! speech-dispatcher isn't running.
//!
//! espeak-ng is GPL-3.0, but we reach it as a **separate process** (we `spawn`
//! the `espeak-ng` binary and pipe text to its stdin), so its GPL imposes no
//! licensing constraint on this crate. See the crate-level license note.
//!
//! Interrupt semantics: each utterance is its own process. [`EspeakSpeaker::stop`]
//! and any interrupting [`speak`] kill the currently-running child before
//! spawning the next, giving the "stop and speak the new thing" behaviour the
//! reader needs on focus changes. The children run, wait and queue on a
//! thread of their own: the caller is usually a UI thread, which must never
//! wait for speech to end.

use std::collections::VecDeque;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crate::error::TtsError;
use crate::speaker::{Speaker, Utterance};

/// The binary we invoke. `espeak-ng` is the maintained fork; on some systems the
/// command is still `espeak` (a symlink). We try `espeak-ng` first.
const ESPEAK_BIN: &str = "espeak-ng";
const ESPEAK_FALLBACK_BIN: &str = "espeak";

/// A [`Speaker`] backed by spawning `espeak-ng` once per utterance.
#[derive(Debug)]
pub struct EspeakSpeaker {
    /// Which binary name actually exists on this system.
    bin: &'static str,
    /// espeak-ng words-per-minute. Default voice speed is ~175 wpm.
    wpm: u32,
    /// `-v` voice argument (e.g. `"en-us"`), if set.
    voice: Option<String>,
    /// The thread that speaks, started with the first utterance.
    player: Option<Player>,
}

/// What the player thread is asked to do.
enum Job {
    /// Speak `text` with `args`; `interrupt` drops what is speaking or queued.
    Speak {
        args: Vec<String>,
        text: String,
        interrupt: bool,
    },
    Stop,
}

/// The handle on the thread that runs one `espeak-ng` at a time.
#[derive(Debug)]
struct Player {
    jobs: mpsc::Sender<Job>,
    /// Whether a child runs or an utterance waits for one.
    speaking: Arc<AtomicBool>,
}

/// How often the player looks whether the speaking child has finished.
const REAP_INTERVAL: Duration = Duration::from_millis(20);

impl Player {
    fn start(bin: &'static str) -> Option<Self> {
        let (jobs, inbox) = mpsc::channel();
        let speaking = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&speaking);
        std::thread::Builder::new()
            .name("espeak-ng".into())
            .spawn(move || play(bin, &inbox, &flag))
            .map_err(|e| tracing::warn!("espeak-ng: no thread to speak on: {e}"))
            .ok()?;
        Some(Self { jobs, speaking })
    }
}

/// The player thread: one child at a time, the next utterance once it ends,
/// an interrupt or stop at once. Ends, killing its child, with the speaker.
fn play(bin: &'static str, inbox: &mpsc::Receiver<Job>, speaking: &AtomicBool) {
    let mut current: Option<Child> = None;
    let mut queue: VecDeque<(Vec<String>, String)> = VecDeque::new();
    loop {
        if current
            .as_mut()
            .is_some_and(|child| !matches!(child.try_wait(), Ok(None)))
        {
            current = None;
        }
        if current.is_none()
            && let Some((args, text)) = queue.pop_front()
        {
            match spawn(bin, &args, &text) {
                Ok(child) => current = Some(child),
                Err(e) => tracing::warn!("espeak-ng: failed to speak: {e}"),
            }
        }
        speaking.store(current.is_some() || !queue.is_empty(), Ordering::Release);
        let job = if current.is_some() {
            match inbox.recv_timeout(REAP_INTERVAL) {
                Ok(job) => Some(job),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match inbox.recv() {
                Ok(job) => Some(job),
                Err(_) => break,
            }
        };
        match job {
            Some(Job::Speak {
                args,
                text,
                interrupt,
            }) => {
                if interrupt {
                    kill(&mut current);
                    queue.clear();
                }
                queue.push_back((args, text));
            }
            Some(Job::Stop) => {
                kill(&mut current);
                queue.clear();
            }
            None => {}
        }
    }
    kill(&mut current);
    speaking.store(false, Ordering::Release);
}

/// Kill and reap the child, if any, with its process group.
fn kill(current: &mut Option<Child>) {
    if let Some(mut child) = current.take() {
        #[cfg(unix)]
        if let Some(pid) = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Start `bin` with `args` speaking `text`, read from its stdin.
fn spawn(bin: &str, args: &[String], text: &str) -> Result<Child, TtsError> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    // Read text from stdin (`--stdin`) so we don't have to worry about shell
    // quoting / arg-length limits for long utterances.
    cmd.arg("--stdin");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().map_err(|e| TtsError::Spawn {
        backend: "espeak-ng",
        source: e,
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        // Best-effort: if the write fails the child will just speak nothing.
        let _ = stdin.write_all(text.as_bytes());
        // espeak-ng's pulse output (pcaudiolib) exits before its final audio
        // buffer fully drains, so the last phoneme gets clipped/stretched —
        // a word like "button" is heard as "buttooo". Appending a
        // sentence-final pause makes that ragged tail land on generated
        // silence instead of on a real phoneme. Only added when the text
        // doesn't already end in terminal punctuation.
        let needs_pause = !text.trim_end().ends_with(['.', '!', '?', ',', ';', ':']);
        if needs_pause {
            let _ = stdin.write_all(b" .");
        }
        // Drop closes stdin -> espeak-ng sees EOF and speaks.
    }
    Ok(child)
}

/// espeak-ng's default speaking rate in words per minute.
const ESPEAK_DEFAULT_WPM: u32 = 175;
/// espeak-ng accepts roughly 80..=450 wpm; clamp to a usable band. Production
/// code clamps in `f32` through the mirrors below; only the tests want the
/// integer band, to assert the value that actually reaches the binary.
#[cfg(test)]
const ESPEAK_MIN_WPM: u32 = 80;
#[cfg(test)]
const ESPEAK_MAX_WPM: u32 = 450;

// `f32` mirrors of the constants above, kept in sync by hand: `set_rate`'s
// math is floating-point, and `u32 as f32` has no lossless `From` conversion to
// reach it through.
const ESPEAK_DEFAULT_WPM_F32: f32 = 175.0;
const ESPEAK_MIN_WPM_F32: f32 = 80.0;
const ESPEAK_MAX_WPM_F32: f32 = 450.0;

impl EspeakSpeaker {
    /// Construct a speaker if an `espeak-ng` (or `espeak`) binary is on `PATH`.
    /// This probes by spawning `--version`, so it does not start any speech.
    ///
    /// # Errors
    ///
    /// Returns [`TtsError::BackendUnavailable`] if neither binary can be found —
    /// the factory uses this to fall through to [`crate::null::NullSpeaker`].
    pub fn new() -> Result<Self, TtsError> {
        let bin = if probe(ESPEAK_BIN) {
            ESPEAK_BIN
        } else if probe(ESPEAK_FALLBACK_BIN) {
            ESPEAK_FALLBACK_BIN
        } else {
            return Err(TtsError::BackendUnavailable {
                backend: "espeak-ng",
                detail: "neither `espeak-ng` nor `espeak` found on PATH".into(),
            });
        };
        Ok(EspeakSpeaker {
            bin,
            wpm: ESPEAK_DEFAULT_WPM,
            voice: None,
            player: None,
        })
    }

    /// The arguments before `--stdin`: rate and voice.
    fn args(&self) -> Vec<String> {
        let mut args = vec!["-s".to_owned(), self.wpm.to_string()];
        if let Some(v) = &self.voice {
            args.extend(["-v".to_owned(), v.clone()]);
        }
        args
    }

    /// Hand `job` to the player, starting it first if need be.
    fn send(&mut self, job: Job) {
        if self.player.is_none() {
            if matches!(job, Job::Stop) {
                return;
            }
            self.player = Player::start(self.bin);
        }
        let Some(player) = &self.player else {
            return;
        };
        if matches!(job, Job::Speak { .. }) {
            player.speaking.store(true, Ordering::Release);
        }
        if player.jobs.send(job).is_err() {
            self.player = None;
        }
    }
}

/// Probe whether `bin --version` runs (i.e. the binary exists & is executable).
fn probe(bin: &str) -> bool {
    bounded_command::run(bin, &["--version"], None, 64 * 1024, Duration::from_secs(2))
        // Spawning at all proves the binary exists & is executable; the exit
        // code of `--version` varies across espeak-ng builds, so we ignore it.
        .is_ok()
}

impl Speaker for EspeakSpeaker {
    fn speak_utterance(&mut self, utterance: Utterance) {
        // A non-interrupting utterance waits on the player for the one
        // speaking, never here.
        let job = Job::Speak {
            args: self.args(),
            text: utterance.text,
            interrupt: utterance.interrupt,
        };
        self.send(job);
    }

    fn stop(&mut self) {
        self.send(Job::Stop);
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to [ESPEAK_MIN_WPM_F32, ESPEAK_MAX_WPM_F32] and checked finite \
                  just above, so always a small, non-negative, in-range value"
    )]
    fn set_rate(&mut self, rate: f32) {
        // Normalize: 1.0 -> default wpm, scaled linearly, then clamped.
        let wpm = (ESPEAK_DEFAULT_WPM_F32 * rate).round();
        let wpm = if wpm.is_finite() {
            wpm.clamp(ESPEAK_MIN_WPM_F32, ESPEAK_MAX_WPM_F32) as u32
        } else {
            ESPEAK_DEFAULT_WPM
        };
        self.wpm = wpm;
    }

    fn set_voice(&mut self, voice: &str) {
        self.voice = Some(voice.to_string());
    }

    fn is_speaking(&self) -> bool {
        self.player
            .as_ref()
            .is_some_and(|player| player.speaking.load(Ordering::Acquire))
    }

    fn backend_name(&self) -> &'static str {
        "espeak-ng"
    }
}

// Dropping the speaker drops the player's sender: the thread kills its child
// and ends.

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: these tests must NOT require espeak-ng to be installed. They test the
    // pure logic (rate mapping, voice arg construction) and tolerate absence.

    #[test]
    fn espeak_maps_rate_voice_name_and_idle_state() {
        {
            // Construct a speaker by hand (bypassing the PATH probe) to test pure
            // state transitions without needing the binary.
            let mut s = EspeakSpeaker {
                bin: ESPEAK_BIN,
                wpm: ESPEAK_DEFAULT_WPM,
                voice: None,
                player: None,
            };
            s.set_rate(1.0);
            assert_eq!(s.wpm, ESPEAK_DEFAULT_WPM);
            s.set_rate(2.0);
            assert_eq!(s.wpm, (ESPEAK_DEFAULT_WPM * 2).min(ESPEAK_MAX_WPM));
            s.set_rate(100.0); // saturates high
            assert_eq!(s.wpm, ESPEAK_MAX_WPM);
            s.set_rate(0.01); // saturates low
            assert_eq!(s.wpm, ESPEAK_MIN_WPM);
            s.set_rate(f32::NAN); // guarded
            assert_eq!(s.wpm, ESPEAK_DEFAULT_WPM);
        }
        {
            let mut s = EspeakSpeaker {
                bin: ESPEAK_BIN,
                wpm: ESPEAK_DEFAULT_WPM,
                voice: None,
                player: None,
            };
            assert_eq!(s.voice, None);
            s.set_voice("en-us");
            assert_eq!(s.voice.as_deref(), Some("en-us"));
        }
        {
            let s = EspeakSpeaker {
                bin: ESPEAK_BIN,
                wpm: ESPEAK_DEFAULT_WPM,
                voice: None,
                player: None,
            };
            assert_eq!(s.backend_name(), "espeak-ng");
        }
        {
            let s = EspeakSpeaker {
                bin: ESPEAK_BIN,
                wpm: ESPEAK_DEFAULT_WPM,
                voice: None,
                player: None,
            };
            assert!(!s.is_speaking());
        }
    }

    #[cfg(unix)]
    #[test]
    fn speech_queues_and_stops_on_its_own_thread() {
        // `sh -c 'sleep 5' --stdin` stands in for a voice that takes a while.
        let player = Player::start("sh").expect("a thread to speak on");
        let long = || Job::Speak {
            args: vec!["-c".into(), "sleep 5".into()],
            text: "a long sentence".into(),
            interrupt: false,
        };
        let started = std::time::Instant::now();
        player.speaking.store(true, Ordering::Release);
        player.jobs.send(long()).unwrap();
        player.jobs.send(long()).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the caller waited"
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(player.speaking.load(Ordering::Acquire));
        player.jobs.send(Job::Stop).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while player.speaking.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "stop left it speaking"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
