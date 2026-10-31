//! The recorder: one encoder, an optional file, and the replay buffer.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use media::StreamCodec;
use media::realtime::EncodedUnit;
use tilo_callstream::VideoEncoder;
use tilo_callstream::mp4_recording::{Mp4Recording, recording_encoder};

/// Frames the replay buffer keeps per second.
pub const REPLAY_FPS: u32 = 30;
/// How much of the past the replay buffer keeps.
pub const REPLAY_SECONDS: u32 = 30;
/// Frames the replay buffer keeps at most.
pub const REPLAY_FRAMES: usize = 900;
/// Bytes the replay buffer keeps at most.
pub const REPLAY_BYTES: usize = 128 * 1024 * 1024;

/// One frame of the desktop as the compositor drew it: rows of R, G, B, A,
/// top first, `width * 4` bytes each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Frame {
    /// A frame of `width` x `height` from `rgba`.
    ///
    /// # Errors
    ///
    /// When `rgba` is not `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, String> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("the frame's size overflows")?;
        if rgba.len() != expected {
            return Err(format!(
                "a {width}x{height} frame is {expected} bytes, not {}",
                rgba.len()
            ));
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }
}

/// What a recording is encoded as, and how much of the past is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// The frame's width in pixels; even, as every encoder here wants.
    pub width: u32,
    /// The frame's height in pixels; even.
    pub height: u32,
    /// Frames per second the recording claims.
    pub fps: u32,
    /// Bits per second the encoder aims at.
    pub bitrate_bps: u64,
    /// A keyframe at least this often, so a clip can start near any moment.
    pub keyframe_interval_frames: u32,
    /// Frames the replay buffer keeps at most.
    pub replay_frames: usize,
    /// Bytes the replay buffer keeps at most.
    pub replay_bytes: usize,
}

impl Settings {
    /// Recording a screen of `width` x `height` at the replay buffer's
    /// frame rate, sized for text and motion on a desktop.
    #[must_use]
    pub fn for_size(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            fps: REPLAY_FPS,
            bitrate_bps: 8_000_000,
            keyframe_interval_frames: REPLAY_FPS * 2,
            replay_frames: REPLAY_FRAMES,
            replay_bytes: REPLAY_BYTES,
        }
    }
}

/// What the recorder is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Not writing a file; the replay buffer still fills.
    Idle,
    /// Writing every frame into the file.
    Recording,
    /// A file is open, but the frames between the pause and the resume are
    /// not in it.
    Paused,
}

/// An encoded unit the replay buffer keeps.
#[derive(Debug)]
struct Buffered {
    data: Arc<[u8]>,
    keyframe: bool,
}

/// The last half minute, encoded: bounded in frames and bytes, starting at
/// a keyframe.
#[derive(Debug)]
pub struct Replay {
    frames: VecDeque<Buffered>,
    bytes: usize,
    max_frames: usize,
    max_bytes: usize,
}

impl Replay {
    fn new(max_frames: usize, max_bytes: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            bytes: 0,
            max_frames: max_frames.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    fn push(&mut self, unit: &EncodedUnit) {
        self.bytes = self.bytes.saturating_add(unit.data.len());
        self.frames.push_back(Buffered {
            data: Arc::from(unit.data.as_slice()),
            keyframe: unit.keyframe,
        });
        while self.frames.len() > self.max_frames || self.bytes > self.max_bytes {
            self.pop();
        }
        // A clip starts at the oldest keyframe the buffer holds: what came
        // before it could not be decoded on its own.
        while self.frames.len() > 1 && !self.frames.front().is_some_and(|front| front.keyframe) {
            self.pop();
        }
    }

    fn pop(&mut self) {
        if let Some(gone) = self.frames.pop_front() {
            self.bytes = self.bytes.saturating_sub(gone.data.len());
        }
    }

    /// Whether the buffer starts at a keyframe, so it is a clip.
    fn is_clip(&self) -> bool {
        self.frames.front().is_some_and(|front| front.keyframe)
    }

    /// The frames kept, oldest first.
    fn units(&self) -> Vec<EncodedUnit> {
        self.frames
            .iter()
            .map(|buffered| EncodedUnit {
                data: buffered.data.to_vec(),
                keyframe: buffered.keyframe,
                pts: std::time::Duration::ZERO,
            })
            .collect()
    }
}

/// Records the desktop: one encoder, the file being written if any, and the
/// replay buffer that always fills.
pub struct Recorder {
    settings: Settings,
    encoder: VideoEncoder,
    codec: StreamCodec,
    recording: Option<(PathBuf, Mp4Recording)>,
    state: State,
    replay: Replay,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("codec", &self.codec)
            .field("encoder", &self.encoder)
            .field("settings", &self.settings)
            .field("state", &self.state)
            .field("recording", &self.recording.as_ref().map(|(path, _)| path))
            .field("replay_frames", &self.replay.frames.len())
            .finish()
    }
}

impl Recorder {
    /// A recorder for `settings`, in the most compatible codec this machine
    /// can encode.
    ///
    /// # Errors
    ///
    /// When no encoder here can encode that size.
    pub fn new(settings: Settings) -> Result<Self, String> {
        let encoder = recording_encoder(
            settings.width,
            settings.height,
            settings.fps,
            settings.bitrate_bps,
            settings.keyframe_interval_frames,
            true,
        )?;
        let codec = encoder.codec();
        Ok(Self {
            settings,
            encoder,
            codec,
            recording: None,
            state: State::Idle,
            replay: Replay::new(settings.replay_frames, settings.replay_bytes),
        })
    }

    /// What the recorder is doing.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// The bitstream it writes.
    #[must_use]
    pub fn codec(&self) -> StreamCodec {
        self.codec
    }

    /// How many frames the replay buffer holds.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.replay.frames.len()
    }

    /// How many bytes the replay buffer holds.
    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.replay.bytes
    }

    /// Encode one frame: into the file while recording, into the replay
    /// buffer always.
    ///
    /// # Errors
    ///
    /// When the frame is not the size the recorder was built for, the
    /// encoder fails, or the file cannot be written.
    pub fn frame(&mut self, frame: &Frame) -> Result<(), String> {
        if frame.width != self.settings.width || frame.height != self.settings.height {
            return Err(format!(
                "a {}x{} frame in a {}x{} recording",
                frame.width, frame.height, self.settings.width, self.settings.height
            ));
        }
        let units = self
            .encoder
            .encode(&frame.rgba)
            .map_err(|error| error.to_string())?;
        for unit in &units {
            self.unit(unit)?;
        }
        Ok(())
    }

    fn unit(&mut self, unit: &EncodedUnit) -> Result<(), String> {
        self.replay.push(unit);
        if self.state == State::Recording
            && let Some((_, recording)) = &mut self.recording
        {
            recording.write_access_unit(&unit.data)?;
        }
        Ok(())
    }

    /// Start writing a recording to `path`; it begins at the next keyframe.
    ///
    /// # Errors
    ///
    /// When a recording is already running.
    pub fn start(&mut self, path: PathBuf) -> Result<(), String> {
        if self.recording.is_some() {
            return Err("a recording is already running".to_owned());
        }
        self.encoder.force_keyframe();
        let recording = Mp4Recording::new(
            path.clone(),
            self.codec,
            self.settings.width,
            self.settings.height,
            self.settings.fps,
            Vec::new(),
        );
        self.recording = Some((path, recording));
        self.state = State::Recording;
        Ok(())
    }

    /// Stop writing frames into the file, keeping it open.
    pub fn pause(&mut self) {
        if self.state == State::Recording {
            self.state = State::Paused;
        }
    }

    /// Write frames into the file again.
    pub fn resume(&mut self) {
        if self.state == State::Paused {
            self.encoder.force_keyframe();
            self.state = State::Recording;
        }
    }

    /// Stop recording and finish the file: where it is, if one was running.
    ///
    /// # Errors
    ///
    /// When the file cannot be finished.
    pub fn stop(&mut self) -> Result<Option<PathBuf>, String> {
        let (State::Recording | State::Paused) = self.state else {
            return Ok(None);
        };
        self.state = State::Idle;
        let Some((path, recording)) = self.recording.take() else {
            return Ok(None);
        };
        recording.finish()?;
        Ok(Some(path))
    }

    /// Write what the replay buffer holds to `path` as its own clip, and
    /// return how many frames it holds.
    ///
    /// # Errors
    ///
    /// When the buffer does not start at a keyframe yet or the file cannot
    /// be written.
    pub fn save_replay(&mut self, path: PathBuf) -> Result<usize, String> {
        if !self.replay.is_clip() {
            return Err("the replay buffer has no complete picture yet".to_owned());
        }
        let units = self.replay.units();
        let mut recording = Mp4Recording::new(
            path,
            self.codec,
            self.settings.width,
            self.settings.height,
            self.settings.fps,
            Vec::new(),
        );
        for unit in &units {
            recording.write_access_unit(&unit.data)?;
        }
        recording.finish()?;
        Ok(units.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(keyframe: bool, len: usize) -> EncodedUnit {
        EncodedUnit {
            data: vec![0; len],
            keyframe,
            pts: std::time::Duration::ZERO,
        }
    }

    #[test]
    fn the_replay_buffer_keeps_its_bounds_and_starts_at_a_keyframe() {
        let mut replay = Replay::new(4, usize::MAX);
        replay.push(&unit(true, 10));
        replay.push(&unit(false, 10));
        replay.push(&unit(false, 10));
        replay.push(&unit(true, 10));
        replay.push(&unit(false, 10));
        // Bounded, and what it keeps starts where a decoder could: the
        // oldest keyframe still inside the bounds.
        assert_eq!(
            replay.frames.len(),
            2,
            "only the last keyframe and after it"
        );
        assert!(replay.frames.front().is_some_and(|front| front.keyframe));
        assert!(replay.is_clip());

        // Bytes are a bound of their own.
        let mut replay = Replay::new(100, 25);
        for keyframe in [true, false, false, false, false] {
            replay.push(&unit(keyframe, 10));
        }
        assert!(replay.bytes <= 25, "{} bytes kept", replay.bytes);
        assert_eq!(
            replay.bytes,
            replay
                .frames
                .iter()
                .map(|frame| frame.data.len())
                .sum::<usize>()
        );
        assert!(
            !replay.is_clip(),
            "the keyframe it started at went with the older frames: no clip until the next one"
        );
        replay.push(&unit(true, 10));
        assert!(replay.is_clip());

        // A buffer that has seen only half a picture is no clip yet.
        let mut replay = Replay::new(4, usize::MAX);
        replay.push(&unit(false, 10));
        assert!(!replay.is_clip());
    }
}
