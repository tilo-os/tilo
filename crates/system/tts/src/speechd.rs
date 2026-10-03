//! speech-dispatcher backend over SSIP — the **primary** TTS path.
//!
//! speech-dispatcher (a.k.a. `speechd`) is the system-wide speech broker Orca
//! itself uses. We talk to it over its **SSIP** socket protocol via the
//! pure-Rust [`ssip-client`](https://crates.io/crates/ssip-client) crate
//! (MIT OR Apache-2.0). speechd, and the espeak-ng / Piper output modules it
//! drives, are GPL — but they sit **behind the socket**, in a separate process,
//! so their GPL imposes no licensing constraint on this crate. The only thing we
//! *link* is the permissive `ssip-client`. See the crate-level license note.
//!
//! Why primary: speechd gives us one system-wide voice configuration,
//! server-side queueing, per-priority interrupt/cancel, and the choice of
//! espeak-ng (instant) or Piper (quality) as output modules — all without us
//! re-implementing any of it.

use ssip_client::Client;
use ssip_client::{
    ClientName, ClientScope, MessageScope, OK_BYE, OK_CANCELED, OK_LANGUAGE_SET,
    OK_OUTPUT_MODULE_SET, OK_PRIORITY_SET, OK_RATE_SET, OK_RECEIVING_DATA, OK_STOPPED,
    OK_VOICE_SET, fifo,
};
use std::os::unix::net::UnixStream;

use crate::error::TtsError;
use crate::speaker::{Speaker, Utterance, rate_to_ssip};

/// The application identity reported to speech-dispatcher (`SET self CLIENT_NAME
/// user:component:instance`). speechd uses this for per-client settings.
const SSIP_USER: &str = "tilo";
const SSIP_COMPONENT: &str = "screen-reader";

/// A [`Speaker`] that speaks through speech-dispatcher over SSIP.
pub struct SpeechDispatcherSpeaker {
    client: Client<UnixStream>,
    /// We track "did we send something the daemon may still be speaking" so
    /// [`is_speaking`] gives a useful answer without a round-trip on every call.
    in_flight: bool,
}

impl SpeechDispatcherSpeaker {
    /// Connect to the running speech-dispatcher daemon and perform the SSIP
    /// handshake (set client name).
    ///
    /// # Errors
    ///
    /// Returns [`TtsError::Ssip`] if the daemon socket can't be opened or the
    /// handshake fails — the factory uses this to fall through to espeak-ng.
    pub fn new() -> Result<Self, TtsError> {
        Self::with_identity(SSIP_USER, SSIP_COMPONENT)
    }

    /// Connect with an explicit `user:component` identity. Mainly for callers
    /// that want to distinguish multiple Tilo clients to the daemon.
    ///
    /// # Errors
    ///
    /// Returns [`TtsError::Ssip`] if the daemon socket can't be opened or the
    /// handshake fails.
    pub fn with_identity(user: &str, component: &str) -> Result<Self, TtsError> {
        Self::connect(&fifo::Builder::new(), user, component)
    }

    fn connect(builder: &fifo::Builder, user: &str, component: &str) -> Result<Self, TtsError> {
        let mut client = builder.build().map_err(|e| TtsError::Ssip {
            detail: format!("connect: {e}"),
        })?;

        client
            .set_client_name(ClientName::new(user, component))
            .map_err(|e| TtsError::Ssip {
                detail: format!("set_client_name: {e}"),
            })?
            .check_client_name_set()
            .map_err(|e| TtsError::Ssip {
                detail: format!("check_client_name_set: {e}"),
            })?;

        Ok(SpeechDispatcherSpeaker {
            client,
            in_flight: false,
        })
    }

    /// Select the speech-dispatcher output module (e.g. `"espeak-ng"`, `"piper"`).
    /// This is how the a11y settings UI switches between the instant and quality
    /// voices: both run behind the same socket, so it's a one-line change.
    pub fn set_output_module(&mut self, module: &str) {
        if let Err(e) = self
            .client
            .set_output_module(ClientScope::Current, module)
            .and_then(|c| c.check_status(OK_OUTPUT_MODULE_SET))
        {
            tracing::warn!("speechd: set_output_module({module}) failed: {e}");
        }
    }

    /// Set the language (BCP-47-ish, e.g. `"en"`, `"de"`), per SSIP `SET … LANGUAGE`.
    pub fn set_language(&mut self, lang: &str) {
        if let Err(e) = self
            .client
            .set_language(ClientScope::Current, lang)
            .and_then(|c| c.check_status(OK_LANGUAGE_SET))
        {
            tracing::warn!("speechd: set_language({lang}) failed: {e}");
        }
    }
}

impl Speaker for SpeechDispatcherSpeaker {
    fn speak_utterance(&mut self, utterance: Utterance) {
        // Interrupt = cancel everything currently queued/spoken for this client
        // before speaking. This is the SSIP `CANCEL self` then `SPEAK` dance
        // that produces stop-and-speak focus narration.
        if utterance.interrupt
            && let Err(e) = self
                .client
                .cancel(MessageScope::All)
                .and_then(|c| c.check_status(OK_CANCELED))
        {
            tracing::debug!("speechd: cancel before interrupting speak failed: {e}");
        }

        // Apply this utterance's priority for the upcoming message.
        if let Err(e) = self
            .client
            .set_priority(utterance.priority.to_ssip())
            .and_then(|c| c.check_status(OK_PRIORITY_SET))
        {
            tracing::debug!("speechd: set_priority failed: {e}");
        }

        // SSIP SPEAK: `SPEAK`, the server's go-ahead, then the text lines and
        // `.`, answered with the queued message id. A line that starts with a
        // dot is sent with the dot doubled, so it cannot end the block early.
        let lines: Vec<String> = utterance
            .text
            .lines()
            .map(|line| {
                if line.starts_with('.') {
                    format!(".{line}")
                } else {
                    line.to_owned()
                }
            })
            .collect();
        let result = self
            .client
            .speak()
            .and_then(|c| c.check_status(OK_RECEIVING_DATA))
            .and_then(|c| c.send_lines(&lines))
            .and_then(|c| c.receive_message_id().map(|_id| c));

        match result {
            Ok(_) => self.in_flight = true,
            Err(e) => tracing::warn!("speechd: speak failed: {e}"),
        }
    }

    fn stop(&mut self) {
        // STOP cuts off speech immediately; CANCEL also flushes the queue. For a
        // silence key we want both: stop what's talking and drop what's pending.
        if let Err(e) = self
            .client
            .stop(MessageScope::All)
            .and_then(|c| c.check_status(OK_STOPPED))
        {
            tracing::debug!("speechd: stop failed: {e}");
        }
        if let Err(e) = self
            .client
            .cancel(MessageScope::All)
            .and_then(|c| c.check_status(OK_CANCELED))
        {
            tracing::debug!("speechd: cancel failed: {e}");
        }
        self.in_flight = false;
    }

    fn set_rate(&mut self, rate: f32) {
        let ssip_rate = rate_to_ssip(rate);
        if let Err(e) = self
            .client
            .set_rate(ClientScope::Current, ssip_rate)
            .and_then(|c| c.check_status(OK_RATE_SET))
        {
            tracing::warn!("speechd: set_rate({ssip_rate}) failed: {e}");
        }
    }

    fn set_voice(&mut self, voice: &str) {
        // SSIP `SET self SYNTHESIS_VOICE <name>` — a named voice within the
        // current output module.
        if let Err(e) = self
            .client
            .set_synthesis_voice(ClientScope::Current, voice)
            .and_then(|c| c.check_status(OK_VOICE_SET))
        {
            tracing::warn!("speechd: set_synthesis_voice({voice}) failed: {e}");
        }
    }

    fn is_speaking(&self) -> bool {
        // speechd is async; we don't get a synchronous "is talking" without
        // listening for BEGIN/END events on a notification socket. We report our
        // best local belief. The reader drives the SpeechQueue off the daemon's
        // END events in production, so it doesn't rely on this.
        self.in_flight
    }

    fn backend_name(&self) -> &'static str {
        "speech-dispatcher"
    }
}

impl Drop for SpeechDispatcherSpeaker {
    fn drop(&mut self) {
        // Politely close the SSIP session so the daemon frees the client slot.
        let _ = self.client.quit().and_then(|c| c.check_status(OK_BYE));
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::speaker::Priority;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    /// Answer each request line with the reply speech-dispatcher would send,
    /// and return every line received.
    fn fake_daemon(stream: UnixStream) -> Vec<String> {
        let mut writer = stream.try_clone().unwrap();
        let mut received = Vec::new();
        let mut in_block = false;
        for line in BufReader::new(stream).lines() {
            let line = line.unwrap();
            let reply = if in_block {
                (line == ".").then(|| {
                    in_block = false;
                    "225-17\r\n225 OK MESSAGE QUEUED\r\n"
                })
            } else {
                Some(match line.as_str() {
                    "SPEAK" => {
                        in_block = true;
                        "230 OK RECEIVING DATA\r\n"
                    }
                    "QUIT" => "231 HAPPY HACKING\r\n",
                    "CANCEL all" => "213 OK CANCELED\r\n",
                    "STOP all" => "210 OK STOPPED\r\n",
                    l if l.starts_with("SET self CLIENT_NAME") => "208 OK CLIENT NAME SET\r\n",
                    l if l.starts_with("SET self PRIORITY") => "202 OK PRIORITY SET\r\n",
                    l if l.starts_with("SET self RATE") => "409 ERR RATE TOO HIGH\r\n",
                    l if l.starts_with("SET self SYNTHESIS_VOICE") => "209 OK VOICE SET\r\n",
                    _ => "500 ERR UNKNOWN\r\n",
                })
            };
            received.push(line);
            if let Some(reply) = reply {
                writer.write_all(reply.as_bytes()).unwrap();
            }
        }
        received
    }

    #[test]
    fn speaker_reads_every_reply_and_stays_in_step() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("speechd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = std::thread::spawn(move || fake_daemon(listener.accept().unwrap().0));
        let mut speaker = SpeechDispatcherSpeaker::connect(
            fifo::Builder::new().path(&socket),
            SSIP_USER,
            SSIP_COMPONENT,
        )
        .unwrap();

        speaker.speak_utterance(
            Utterance::new("Save, button\n.hidden file").with_priority(Priority::Important),
        );
        assert!(speaker.is_speaking());
        // A refused setting is logged; the next command still reads its own reply.
        speaker.set_rate(2.0);
        speaker.set_voice("en-us");
        speaker.stop();
        assert!(!speaker.is_speaking());
        drop(speaker);

        assert_eq!(
            daemon.join().unwrap(),
            [
                "SET self CLIENT_NAME tilo:screen-reader:main",
                "CANCEL all",
                "SET self PRIORITY important",
                "SPEAK",
                "Save, button",
                "..hidden file",
                ".",
                "SET self RATE 100",
                "SET self SYNTHESIS_VOICE en-us",
                "STOP all",
                "CANCEL all",
                "QUIT",
            ]
        );
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "milli only ever reaches 4000, far inside f32's exact-integer range"
    )]
    fn speechd_identity_and_rate_translation_match_ssip() {
        // Phase `identity_constants_are_sane`.
        {
            assert_eq!(super::SSIP_USER, "tilo");
            assert_eq!(super::SSIP_COMPONENT, "screen-reader");
        }
        // Phase `rate_translation_matches_ssip_range`.
        {
            // The SSIP RATE command takes -100..=100; prove our translation stays
            // inside it for a sweep of normalized rates.
            for milli in 1..=4000u32 {
                let r = milli as f32 / 1000.0; // 0.001 ..= 4.0
                let v = rate_to_ssip(r);
                assert!(
                    (-100..=100).contains(&v),
                    "rate {r} -> {v} out of SSIP range"
                );
            }
            assert_eq!(rate_to_ssip(1.0), 0);
        }
    }
}
