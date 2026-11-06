//! The machine's own sound and the microphone, as a screen recording takes
//! them.
//!
//! Recording follows the session's `PipeWire` boundary: resolve the current
//! default sink or source with `wpctl`, then capture it through the one
//! `pw-record` capture ([`media::audio::record`]) at the muxer's lossless
//! stereo 48 kHz. The frame loop never blocks on the audio process; chunks
//! that arrive while encoding stalls are dropped rather than waited for, and
//! a pause drains instead of replaying sound captured while paused.

use media::audio::record::{self, Format, Overflow};
use ui::widget::AudioCaptureMode;

/// What every recording track is captured at before the muxer takes it: the
/// file's lossless stereo 48 kHz.
pub const FORMAT: Format = Format::STEREO_48K;

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
    /// The default input.
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
    if !record::available() {
        return Availability::default();
    }
    Availability {
        system: default_target(Source::System).is_ok(),
        microphone: default_target(Source::Microphone).is_ok(),
    }
}

/// One recording track: a source and its open capture.
pub struct Track {
    source: Source,
    capture: record::Capture,
    stream: record::Stream,
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
                let target = default_target(source)?;
                let spec = record::Spec {
                    target: &target,
                    properties: Some(source.properties()),
                    format: FORMAT,
                    overflow: Overflow::Drop,
                    label: "screen recording",
                };
                let (capture, stream) = record::Capture::start(&spec)?;
                Ok(Self {
                    source,
                    capture,
                    stream,
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
    /// When the recorder ended or its sound stopped coming.
    pub fn drain(&mut self, keep: bool) -> Result<Vec<u8>, String> {
        if !self.capture.alive() {
            return Err(format!(
                "PipeWire capture of {} ended early",
                self.source.track_name()
            ));
        }
        if keep {
            self.stream.take()
        } else {
            self.stream.discard();
            Ok(Vec::new())
        }
    }
}

fn default_target(source: Source) -> Result<String, String> {
    media::audio::default_node::object_serial(source.default_node())
        .map_err(|error| format!("default {}: {error}", source.track_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            FORMAT,
            Format {
                rate: 48_000,
                channels: 2
            },
            "the muxer's tracks are 48 kHz stereo S16LE"
        );
        assert_eq!(FORMAT.frame_bytes(), 4);
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
}
