//! An app's sound on its device's output, over the connection its launcher handed it.
//!
//! A sandboxed app receives neither an audio socket nor the device's mixer: its media stack
//! decodes its own sound and hands it over 20 ms at a time ([`media::audio_output`]). This
//! process's one connection to the daemon ([`tilo_action_line::handed`]) carries those
//! commands to the device's output, which plays them on the person's speakers, one stream per
//! open output, named after the app. A write's PCM travels as the frame's binary body, never as
//! JSON; a full device queue answers with the sequence it last accepted, which the decoder
//! retries for a bounded time ([`audio_output::DEADLINE`]) instead of holding everything else.
//!
//! The person's lockdown silences app sound on both ends ([`media::audio_output::is_disabled`]):
//! here because every write goes through the guarded output, and there because the device's
//! output refuses its next command while it is on.

use std::sync::{Arc, Mutex};

use media::audio_output::{Factory, Observation, Output};
use tilo_action_line::LineClient;
use tilo_actions::remote::audio_output::{self, Request, Snapshot};

use tilo_clock::time::Duration;

/// How often a full device queue is asked again, until [`audio_output::DEADLINE`].
const RETRY: Duration = Duration::from_millis(5);

/// Install the device's output, on the connection the process's launcher handed it, as this
/// process's governed sound.
pub(crate) fn install(line: Arc<LineClient>) {
    media::audio_output::install(Arc::new(LineFactory {
        shared: Arc::new(Shared {
            line,
            observed: Mutex::new(None),
            failure: Mutex::new(None),
            backpressure: Mutex::new(0),
        }),
    }));
}

/// What every output this factory opens shares: the one connection, and the device's last word
/// about the output.
struct Shared {
    line: Arc<LineClient>,
    /// The newest state the device gave, which a caller that cannot wait reads.
    observed: Mutex<Option<Snapshot>>,
    failure: Mutex<Option<String>>,
    /// Writes answered past a full queue, as evidence without the PCM.
    backpressure: Mutex<u32>,
}

impl Shared {
    fn observe(&self, snapshot: &Snapshot) {
        if let Ok(mut observed) = self.observed.lock() {
            *observed = Some(snapshot.clone());
        }
    }

    /// Send `request` and wait for the device's answer.
    fn command(&self, request: Request) -> Result<Snapshot, String> {
        let result = self.line.audio_output(request).map_err(|error| {
            let why = error.to_string();
            self.fail(&why);
            why
        });
        if let Ok(snapshot) = &result {
            self.observe(snapshot);
        }
        result
    }

    fn fail(&self, why: &str) {
        if let Ok(mut failure) = self.failure.lock() {
            *failure = Some(why.to_owned());
        }
        tracing::warn!(
            target: "tilo::audio_output",
            backpressure = self.backpressure.lock().ok().map(|pressed| *pressed),
            %why,
            "governed audio command failed"
        );
    }

    fn cached_observation(&self, generation: u64) -> Observation {
        self.observed
            .lock()
            .ok()
            .and_then(|observed| {
                observed
                    .as_ref()
                    .filter(|observed| observed.generation == generation)
                    .map(observation)
            })
            .unwrap_or_default()
    }
}

struct LineFactory {
    shared: Arc<Shared>,
}

impl Factory for LineFactory {
    fn open(&self) -> Result<Box<dyn Output>, String> {
        let snapshot = self.shared.command(Request::Open {})?;
        if let Ok(mut failure) = self.shared.failure.lock() {
            // A new output starts without the old one's failure.
            *failure = None;
        }
        Ok(Box::new(LineOutput {
            shared: Arc::clone(&self.shared),
            generation: snapshot.generation,
            sequence: 0,
            closed: false,
        }))
    }
}

struct LineOutput {
    shared: Arc<Shared>,
    generation: u64,
    sequence: u64,
    closed: bool,
}

impl LineOutput {
    /// Pause, flush, volume and close are never waited for: a decoder holds nothing on them.
    fn control(&self, request: Request) -> Result<(), String> {
        self.shared
            .line
            .audio_output_send(request)
            .map_err(|error| error.to_string())
    }
}

fn observation(snapshot: &Snapshot) -> Observation {
    Observation {
        rendered_frames: snapshot.rendered_frames,
        queued_frames: snapshot.queued_frames,
        ready: snapshot.ready && !snapshot.closed,
    }
}

impl Output for LineOutput {
    fn cached_error(&self) -> Option<String> {
        self.shared
            .failure
            .lock()
            .ok()
            .and_then(|failure| failure.clone())
    }

    fn cached_observation(&self) -> Observation {
        self.shared.cached_observation(self.generation)
    }

    fn write(&mut self, samples: &[i16]) -> Result<Observation, String> {
        if self.closed
            || samples.is_empty()
            || samples.len() > audio_output::MAX_SAMPLES
            || !samples
                .len()
                .is_multiple_of(usize::from(audio_output::CHANNELS))
        {
            return Err("Invalid or closed governed audio output packet".into());
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Audio output sequence exhausted")?;
        let deadline = tilo_clock::now().checked_add(audio_output::DEADLINE);
        loop {
            let snapshot = self
                .shared
                .line
                .audio_output_write(self.generation, sequence, samples)
                .map_err(|error| {
                    let why = error.to_string();
                    self.shared.fail(&why);
                    why
                })?;
            self.shared.observe(&snapshot);
            if snapshot.closed {
                self.closed = true;
                return Err("Governed audio output closed".into());
            }
            if snapshot.accepted_sequence == sequence {
                self.sequence = sequence;
                return Ok(observation(&snapshot));
            }
            if snapshot.accepted_sequence > sequence {
                return Err("Governed audio output did not accept the packet".into());
            }
            if let Ok(mut pressed) = self.shared.backpressure.lock() {
                *pressed = pressed.saturating_add(1);
            }
            if deadline.is_none_or(|deadline| tilo_clock::now() >= deadline) {
                self.shared.fail("Governed audio output timed out");
                return Err("Governed audio output timed out".into());
            }
            std::thread::park_timeout(RETRY);
        }
    }

    fn observation(&mut self) -> Result<Observation, String> {
        let snapshot = self.shared.command(Request::Status {
            generation: self.generation,
        })?;
        Ok(observation(&snapshot))
    }

    fn pause(&mut self, paused: bool) -> Result<(), String> {
        self.control(Request::Pause {
            generation: self.generation,
            paused,
        })
    }

    fn flush(&mut self) -> Result<(), String> {
        self.control(Request::Flush {
            generation: self.generation,
        })
    }

    fn volume(&mut self, percent: u8) -> Result<(), String> {
        self.control(Request::Volume {
            generation: self.generation,
            percent,
        })
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.control(Request::Close {
            generation: self.generation,
        });
    }
}

impl Drop for LineOutput {
    fn drop(&mut self) {
        self.close();
    }
}
