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
//! reader needs on focus changes.

use std::io::Write;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
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
    /// The currently-running child, if any (so we can kill it to interrupt).
    current: Option<Child>,
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
            current: None,
        })
    }

    /// Reap the current child if it has already exited, so [`is_speaking`] is
    /// accurate and we don't leave zombies.
    fn poll_current(&mut self) {
        if let Some(child) = self.current.as_mut() {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    self.current = None;
                }
                Ok(None) => { /* still running */ }
                Err(_) => {
                    self.current = None;
                }
            }
        }
    }

    /// Kill and reap the current child, if any.
    fn kill_current(&mut self) {
        if let Some(mut child) = self.current.take() {
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

    fn spawn(&mut self, text: &str) -> Result<(), TtsError> {
        let mut cmd = Command::new(self.bin);
        cmd.arg("-s").arg(self.wpm.to_string());
        if let Some(v) = &self.voice {
            cmd.arg("-v").arg(v);
        }
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
        self.current = Some(child);
        Ok(())
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
        self.poll_current();
        if utterance.interrupt {
            self.kill_current();
        } else if self.current.is_some() {
            // Non-interrupting while something is speaking: espeak-ng has no
            // server-side queue, so we wait for the current child to finish.
            // (The reader's SpeechQueue normally serializes for us; this is the
            // safety net for direct callers.)
            if let Some(mut child) = self.current.take() {
                let _ = child.wait();
            }
        }
        if let Err(e) = self.spawn(&utterance.text) {
            tracing::warn!("espeak-ng: failed to speak: {e}");
        }
    }

    fn stop(&mut self) {
        self.kill_current();
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
        // Const-eval is impossible here without &mut to reap; report optimistic
        // truthiness based on whether we believe a child is live. Callers that
        // need precision should drive the SpeechQueue instead.
        self.current.is_some()
    }

    fn backend_name(&self) -> &'static str {
        "espeak-ng"
    }
}

impl Drop for EspeakSpeaker {
    fn drop(&mut self) {
        self.kill_current();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: these tests must NOT require espeak-ng to be installed. They test the
    // pure logic (rate mapping, voice arg construction) and tolerate absence.

    #[test]
    fn espeak_maps_rate_voice_name_and_idle_state() {
        // Phase `rate_maps_to_clamped_wpm`.
        {
            // Construct a speaker by hand (bypassing the PATH probe) to test pure
            // state transitions without needing the binary.
            let mut s = EspeakSpeaker {
                bin: ESPEAK_BIN,
                wpm: ESPEAK_DEFAULT_WPM,
                voice: None,
                current: None,
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
        // Phase `voice_setter_stores_value`.
        {
            let mut s = EspeakSpeaker {
                bin: ESPEAK_BIN,
                wpm: ESPEAK_DEFAULT_WPM,
                voice: None,
                current: None,
            };
            assert_eq!(s.voice, None);
            s.set_voice("en-us");
            assert_eq!(s.voice.as_deref(), Some("en-us"));
        }
        // Phase `backend_name`.
        {
            let s = EspeakSpeaker {
                bin: ESPEAK_BIN,
                wpm: ESPEAK_DEFAULT_WPM,
                voice: None,
                current: None,
            };
            assert_eq!(s.backend_name(), "espeak-ng");
        }
        // Phase `not_speaking_when_no_child`.
        {
            let s = EspeakSpeaker {
                bin: ESPEAK_BIN,
                wpm: ESPEAK_DEFAULT_WPM,
                voice: None,
                current: None,
            };
            assert!(!s.is_speaking());
        }
    }
}
