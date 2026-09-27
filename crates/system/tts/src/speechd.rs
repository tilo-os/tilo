//! speech-dispatcher backend over SSIP — the **primary** TTS path.
//!
//! speech-dispatcher (a.k.a. `speechd`) is the system-wide speech broker Orca
//! itself uses. We talk to it over its **SSIP** socket, a CRLF line protocol:
//! each command gets one reply, `NNN-data` lines followed by a `NNN text`
//! status line, where codes 300 and up are errors. speechd, and the espeak-ng /
//! Piper output modules it drives, are GPL — but they sit **behind the
//! socket**, in a separate process, so their GPL imposes no licensing
//! constraint on this crate. See the crate-level license note.
//!
//! Why primary: speechd gives us one system-wide voice configuration,
//! server-side queueing, per-priority interrupt/cancel, and the choice of
//! espeak-ng (instant) or Piper (quality) as output modules — all without us
//! re-implementing any of it.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::TtsError;
use crate::speaker::{Speaker, Utterance, rate_to_ssip};

/// The application identity reported to speech-dispatcher (`SET self CLIENT_NAME
/// user:component:instance`). speechd uses this for per-client settings.
const SSIP_USER: &str = "tilo";
const SSIP_COMPONENT: &str = "screen-reader";

/// How long a reply may take before the daemon counts as gone.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// The daemon's socket under the XDG runtime directory.
fn socket_path() -> io::Result<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("speech-dispatcher").join("speechd.sock"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))
}

/// One SSIP connection: requests go out on `output`, replies come back on `input`.
struct Session<S: io::Read + Write> {
    input: BufReader<S>,
    output: S,
}

impl<S: io::Read + Write> Session<S> {
    fn new(input: S, output: S) -> Self {
        Self {
            input: BufReader::new(input),
            output,
        }
    }

    /// Send one command line and read its reply.
    fn command(&mut self, line: &str) -> io::Result<Vec<String>> {
        write!(self.output, "{line}\r\n")?;
        self.output.flush()?;
        self.reply()
    }

    /// Read one reply: its `NNN-` data lines, up to and including the status line.
    fn reply(&mut self) -> io::Result<Vec<String>> {
        let mut data = Vec::new();
        loop {
            let mut line = String::new();
            if self.input.read_line(&mut line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "speech-dispatcher closed the connection",
                ));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            let code = line
                .get(..3)
                .and_then(|code| code.parse::<u16>().ok())
                .ok_or_else(|| invalid(line))?;
            let rest = line.get(4..).unwrap_or_default();
            match line.get(3..4) {
                Some("-") => data.push(rest.to_owned()),
                Some(" ") if code >= 300 => {
                    return Err(io::Error::other(format!("{code} {rest}")));
                }
                Some(" ") => return Ok(data),
                _ => return Err(invalid(line)),
            }
        }
    }

    /// `SPEAK`, then the text as a dot-terminated data block, whose reply
    /// carries the queued message id.
    fn speak(&mut self, text: &str) -> io::Result<()> {
        self.command("SPEAK")?;
        for line in text.lines() {
            // A line that starts with a dot is escaped by doubling it, so no
            // text line can end the block early.
            let escape = if line.starts_with('.') { "." } else { "" };
            write!(self.output, "{escape}{line}\r\n")?;
        }
        self.command(".").map(drop)
    }

    /// A command whose reply only reports success.
    fn set(&mut self, line: &str) -> io::Result<()> {
        self.command(line).map(drop)
    }
}

fn invalid(line: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("malformed SSIP reply: {line:?}"),
    )
}

/// A [`Speaker`] that speaks through speech-dispatcher over SSIP.
pub struct SpeechDispatcherSpeaker {
    session: Session<UnixStream>,
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
        let connect = || -> io::Result<Session<UnixStream>> {
            let stream = UnixStream::connect(socket_path()?)?;
            stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
            Ok(Session::new(stream.try_clone()?, stream))
        };
        let session = connect().map_err(|e| TtsError::Ssip {
            detail: format!("connect: {e}"),
        })?;
        Self::handshake(session, user, component)
    }

    fn handshake(
        mut session: Session<UnixStream>,
        user: &str,
        component: &str,
    ) -> Result<Self, TtsError> {
        session
            .set(&format!("SET self CLIENT_NAME {user}:{component}:main"))
            .map_err(|e| TtsError::Ssip {
                detail: format!("set client name: {e}"),
            })?;
        Ok(SpeechDispatcherSpeaker {
            session,
            in_flight: false,
        })
    }

    /// Select the speech-dispatcher output module (e.g. `"espeak-ng"`, `"piper"`).
    /// This is how the a11y settings UI switches between the instant and quality
    /// voices: both run behind the same socket, so it's a one-line change.
    pub fn set_output_module(&mut self, module: &str) {
        if let Err(e) = self
            .session
            .set(&format!("SET self OUTPUT_MODULE {module}"))
        {
            tracing::warn!("speechd: set_output_module({module}) failed: {e}");
        }
    }

    /// Set the language (BCP-47-ish, e.g. `"en"`, `"de"`), per SSIP `SET … LANGUAGE`.
    pub fn set_language(&mut self, lang: &str) {
        if let Err(e) = self.session.set(&format!("SET self LANGUAGE {lang}")) {
            tracing::warn!("speechd: set_language({lang}) failed: {e}");
        }
    }
}

impl Speaker for SpeechDispatcherSpeaker {
    fn speak_utterance(&mut self, utterance: Utterance) {
        // Interrupt = cancel everything currently queued/spoken before
        // speaking. This is the SSIP `CANCEL` then `SPEAK` dance that
        // produces stop-and-speak focus narration.
        if utterance.interrupt
            && let Err(e) = self.session.set("CANCEL all")
        {
            tracing::debug!("speechd: cancel before interrupting speak failed: {e}");
        }

        // Apply this utterance's priority for the upcoming message.
        let priority = utterance.priority.to_ssip();
        if let Err(e) = self.session.set(&format!("SET self PRIORITY {priority}")) {
            tracing::debug!("speechd: set_priority failed: {e}");
        }

        match self.session.speak(&utterance.text) {
            Ok(()) => self.in_flight = true,
            Err(e) => tracing::warn!("speechd: speak failed: {e}"),
        }
    }

    fn stop(&mut self) {
        // STOP cuts off speech immediately; CANCEL also flushes the queue. For a
        // silence key we want both: stop what's talking and drop what's pending.
        if let Err(e) = self.session.set("STOP all") {
            tracing::debug!("speechd: stop failed: {e}");
        }
        if let Err(e) = self.session.set("CANCEL all") {
            tracing::debug!("speechd: cancel failed: {e}");
        }
        self.in_flight = false;
    }

    fn set_rate(&mut self, rate: f32) {
        let ssip_rate = rate_to_ssip(rate);
        if let Err(e) = self.session.set(&format!("SET self RATE {ssip_rate}")) {
            tracing::warn!("speechd: set_rate({ssip_rate}) failed: {e}");
        }
    }

    fn set_voice(&mut self, voice: &str) {
        // SSIP `SET self SYNTHESIS_VOICE <name>` — a named voice within the
        // current output module.
        if let Err(e) = self
            .session
            .set(&format!("SET self SYNTHESIS_VOICE {voice}"))
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
        let _ = self.session.set("QUIT");
    }
}

#[cfg(test)]
mod tests {
    // These tests must NOT require a running speech-dispatcher: a thread on the
    // far end of a socket pair plays the daemon.
    use super::*;
    use crate::speaker::Priority;
    use std::thread;

    /// Answer each request line with the reply the daemon would send, and
    /// return every line received.
    fn fake_daemon(stream: UnixStream) -> thread::JoinHandle<Vec<String>> {
        thread::spawn(move || {
            let mut writer = stream.try_clone().unwrap();
            let mut received = Vec::new();
            let mut in_block = false;
            for line in BufReader::new(stream).lines() {
                let line = line.unwrap();
                let reply: Option<&str> = if in_block {
                    (line == ".").then(|| {
                        in_block = false;
                        "225-17\r\n225 OK MESSAGE QUEUED\r\n"
                    })
                } else if line == "SPEAK" {
                    in_block = true;
                    Some("230 OK RECEIVING DATA\r\n")
                } else if line == "QUIT" {
                    Some("231 HAPPY HACKING\r\n")
                } else if line.starts_with("SET self RATE") {
                    Some("409 ERR RATE TOO HIGH\r\n")
                } else {
                    Some("200 OK\r\n")
                };
                received.push(line);
                if let Some(reply) = reply {
                    writer.write_all(reply.as_bytes()).unwrap();
                }
            }
            received
        })
    }

    #[test]
    fn speaker_sends_ssip_commands_and_reads_each_reply() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let daemon = fake_daemon(theirs);
        let session = Session::new(ours.try_clone().unwrap(), ours);
        let mut speaker =
            SpeechDispatcherSpeaker::handshake(session, SSIP_USER, SSIP_COMPONENT).unwrap();

        speaker.speak_utterance(
            Utterance::new("Save, button\n.hidden file").with_priority(Priority::Important),
        );
        assert!(speaker.is_speaking());
        // A refused setting is logged; the session stays in step.
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
    fn replies_parse_data_lines_and_errors() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        theirs
            .write_all(
                b"249-voice a\r\n249 OK VOICE LIST SENT\r\n409 ERR RATE TOO HIGH\r\nnope\r\n",
            )
            .unwrap();
        drop(theirs);
        let mut session = Session::new(ours.try_clone().unwrap(), ours);
        assert_eq!(session.reply().unwrap(), ["voice a"]);
        assert!(session.reply().unwrap_err().to_string().contains("409"));
        assert_eq!(
            session.reply().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            session.reply().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "milli only ever reaches 4000, far inside f32's exact-integer range"
    )]
    fn rate_translation_stays_in_ssip_range() {
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
