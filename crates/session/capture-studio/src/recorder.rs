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

/// The file extension a recording is written with, from whether it carries
/// sound: MOV when it does, which MP4 cannot hold losslessly.
#[must_use]
pub fn extension(sound: bool) -> &'static str {
    Mp4Recording::extension(sound)
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
    /// Per audio track, the sound that came with the kept frames.
    audio: Vec<VecDeque<u8>>,
}

/// Bytes one track of the replay keeps: its own half minute, bounded so a
/// recorder left running never grows.
const AUDIO_KEPT_BYTES: usize = REPLAY_SECONDS as usize
    * crate::audio::FORMAT.rate as usize
    * crate::audio::FORMAT.frame_bytes();

/// What one video frame of a clip carries of each track: the muxer
/// interleaves its sound at the replay's own rate.
const AUDIO_BYTES_PER_VIDEO_FRAME: usize =
    (crate::audio::FORMAT.rate as usize / REPLAY_FPS as usize) * crate::audio::FORMAT.frame_bytes();

impl Replay {
    fn new(max_frames: usize, max_bytes: usize, audio_tracks: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            bytes: 0,
            max_frames: max_frames.max(1),
            max_bytes: max_bytes.max(1),
            audio: (0..audio_tracks).map(|_| VecDeque::new()).collect(),
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

    /// Keep the sound of `track` that came with the frames now kept.
    fn push_audio(&mut self, track: usize, pcm: &[u8]) {
        let Some(kept) = self.audio.get_mut(track) else {
            return;
        };
        kept.extend(pcm.iter().copied());
        if kept.len() > AUDIO_KEPT_BYTES {
            let excess = kept.len().saturating_sub(AUDIO_KEPT_BYTES) & !3;
            kept.drain(..excess);
        }
    }

    /// `track`'s sound for the `frames` kept, one video frame's worth each:
    /// the newest sound at the end, silence where the buffer is younger than
    /// the picture.
    fn audio_tail(&self, track: usize, frames: usize) -> Vec<u8> {
        let target_len = frames.saturating_mul(AUDIO_BYTES_PER_VIDEO_FRAME);
        let Some(kept) = self.audio.get(track) else {
            return vec![0; target_len];
        };
        let available = kept.len().min(target_len);
        let mut out = vec![0; target_len];
        let source_start = kept.len().saturating_sub(available);
        let target_start = target_len.saturating_sub(available);
        if let Some(targets) = out.get_mut(target_start..) {
            for (target, source) in targets.iter_mut().zip(kept.iter().skip(source_start)) {
                *target = *source;
            }
        }
        out
    }
}

/// What a recording left behind when its file could not be finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unfinished {
    /// The file it was writing, while what was written is still there to
    /// keep: a file without its trailer may still hold the frames it took.
    pub path: Option<PathBuf>,
    /// What went wrong, in the media stack's words.
    pub error: String,
}

/// Records the desktop: one encoder, the file being written if any, and the
/// replay buffer that always fills.
pub struct Recorder {
    settings: Settings,
    encoder: VideoEncoder,
    codec: StreamCodec,
    /// The audio tracks the file carries, in muxing order.
    audio_tracks: Vec<String>,
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
            .field("audio_tracks", &self.audio_tracks)
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
        Self::with_audio(settings, Vec::new())
    }

    /// A recorder for `settings` whose file also carries `audio_tracks`,
    /// named in muxing order: the machine's own sound, a microphone, or both.
    ///
    /// # Errors
    ///
    /// When no encoder here can encode that size.
    pub fn with_audio(settings: Settings, audio_tracks: Vec<String>) -> Result<Self, String> {
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
            replay: Replay::new(
                settings.replay_frames,
                settings.replay_bytes,
                audio_tracks.len(),
            ),
            audio_tracks,
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

    /// The file extension a recording is written with: MOV when it carries
    /// sound, which MP4 cannot hold losslessly.
    #[must_use]
    pub fn extension(&self) -> &'static str {
        extension(!self.audio_tracks.is_empty())
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

    /// Take one track's sound, interleaved stereo S16LE at 48 kHz: into the
    /// file while recording, into the replay buffer always.
    ///
    /// # Errors
    ///
    /// When there is no such track, the sound is not whole sample frames, or
    /// the file cannot be written.
    pub fn audio(&mut self, track: usize, pcm: &[u8]) -> Result<(), String> {
        if track >= self.audio_tracks.len() {
            return Err(format!("a recording has no audio track {track}"));
        }
        if !pcm.len().is_multiple_of(4) {
            return Err("stereo S16 audio is not whole sample frames".to_owned());
        }
        self.replay.push_audio(track, pcm);
        if self.state == State::Recording
            && let Some((_, recording)) = &mut self.recording
        {
            recording.write_pcm_audio(track, pcm)?;
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
            self.audio_tracks.clone(),
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
    /// When the file cannot be finished: what went wrong, and the file it
    /// left when it is still there.
    pub fn stop(&mut self) -> Result<Option<PathBuf>, Unfinished> {
        let (State::Recording | State::Paused) = self.state else {
            return Ok(None);
        };
        self.state = State::Idle;
        let Some((path, recording)) = self.recording.take() else {
            return Ok(None);
        };
        match recording.finish() {
            Ok(()) => Ok(Some(path)),
            Err(error) => Err(Unfinished {
                path: path.exists().then_some(path),
                error,
            }),
        }
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
        let per_frame = AUDIO_BYTES_PER_VIDEO_FRAME;
        let audio: Vec<Vec<u8>> = (0..self.audio_tracks.len())
            .map(|track| self.replay.audio_tail(track, units.len()))
            .collect();
        let mut recording = Mp4Recording::new(
            path,
            self.codec,
            self.settings.width,
            self.settings.height,
            self.settings.fps,
            self.audio_tracks.clone(),
        );
        for (index, unit) in units.iter().enumerate() {
            recording.write_access_unit(&unit.data)?;
            // Each frame carries its own 1/fps of sound: the muxer interleaves
            // the tracks by sample count, so the clip shows what it heard.
            let start = index.saturating_mul(per_frame);
            let end = start.saturating_add(per_frame);
            for (track, pcm) in audio.iter().enumerate() {
                let Some(samples) = pcm.get(start..end) else {
                    return Err("a clip's audio is shorter than its frames".to_owned());
                };
                recording.write_pcm_audio(track, samples)?;
            }
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
        let mut replay = Replay::new(4, usize::MAX, 0);
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
        let mut replay = Replay::new(100, 25, 0);
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
        let mut replay = Replay::new(4, usize::MAX, 0);
        replay.push(&unit(false, 10));
        assert!(!replay.is_clip());
    }

    /// The sound of the replay buffer follows the frames kept: the newest
    /// sound ends with the newest frame, and what the buffer never heard is
    /// silence rather than a shifted track.
    #[test]
    fn a_clips_sound_is_aligned_with_its_frames() {
        let mut replay = Replay::new(4, usize::MAX, 1);
        let per_frame = AUDIO_BYTES_PER_VIDEO_FRAME;
        replay.push(&unit(true, 10));
        replay.push(&unit(false, 10));
        replay.push_audio(0, &vec![1; per_frame / 2]);
        replay.push_audio(0, &vec![2; per_frame / 2]);
        let tail = replay.audio_tail(0, 2);
        assert_eq!(tail.len(), per_frame.saturating_mul(2));
        let (young, heard) = tail.split_at(per_frame);
        assert!(
            young.iter().all(|sample| *sample == 0),
            "sound the buffer never heard is silence, not a shifted track"
        );
        assert_eq!(&heard[..per_frame / 2], &vec![1; per_frame / 2][..]);
        assert_eq!(&heard[per_frame / 2..], &vec![2; per_frame / 2][..]);

        // The newest half minute at most: older sound goes with older frames.
        let mut replay = Replay::new(4, usize::MAX, 1);
        replay.push_audio(0, &vec![9; AUDIO_KEPT_BYTES.saturating_add(per_frame)]);
        assert_eq!(replay.audio[0].len(), AUDIO_KEPT_BYTES);
        assert!(replay.audio[0].iter().all(|sample| *sample == 9));
        // A track the recorder has none of is silence, never a shifted one.
        assert_eq!(replay.audio_tail(1, 1), vec![0; per_frame]);
    }

    /// A 64x48 picture of one shade.
    fn picture(shade: u8) -> Frame {
        let mut rgba = vec![0u8; 64 * 48 * 4];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[
                shade.saturating_mul(35),
                110,
                210u8.saturating_sub(shade),
                255,
            ]);
        }
        Frame {
            width: 64,
            height: 48,
            rgba,
        }
    }

    /// What a demuxer sees in `path`: (video frames, audio tracks).
    fn inspect(path: &std::path::Path) -> (u64, usize) {
        use media::gst;
        use media::gst::prelude::*;

        gst::init().unwrap();
        let pipeline = gst::parse::launch(&format!(
            "filesrc location=\"{}\" ! qtdemux name=d d.video_0 ! queue ! fakesink name=v sync=false",
            path.display()
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let frames = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sink = pipeline.by_name("v").unwrap();
        let pad = sink.static_pad("sink").unwrap();
        let counter = Arc::clone(&frames);
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
        let audio = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tracks = Arc::clone(&audio);
        let demux = pipeline.by_name("d").unwrap();
        demux.connect_pad_added(move |_, pad| {
            if pad.name().starts_with("audio") {
                tracks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
        pipeline.set_state(gst::State::Playing).unwrap();
        let bus = pipeline.bus().unwrap();
        bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(20),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        let frames = frames.load(std::sync::atomic::Ordering::Relaxed);
        let audio = audio.load(std::sync::atomic::Ordering::Relaxed);
        let _ = pipeline.set_state(gst::State::Null);
        (frames, audio)
    }

    #[test]
    fn a_recording_with_sound_carries_its_tracks_through_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = Recorder::with_audio(
            Settings::for_size(64, 48),
            vec!["Desktop sound".into(), "Microphone".into()],
        )
        .unwrap();
        assert_eq!(recorder.extension(), "mov", "MP4 cannot hold sound");
        assert_eq!(
            Recorder::new(Settings::for_size(64, 48))
                .unwrap()
                .extension(),
            "mp4"
        );
        let path = dir
            .path()
            .join(format!("recording.{}", recorder.extension()));
        recorder.start(path.clone()).unwrap();
        let per_frame = AUDIO_BYTES_PER_VIDEO_FRAME;
        for shade in 0..3u8 {
            recorder.frame(&picture(shade)).unwrap();
            recorder
                .audio(0, &vec![shade.saturating_add(1); per_frame])
                .unwrap();
            recorder
                .audio(1, &vec![shade.saturating_add(4); per_frame])
                .unwrap();
        }
        assert_eq!(recorder.stop().unwrap(), Some(path.clone()));
        let (frames, tracks) = inspect(&path);
        assert_eq!(frames, 3, "every frame is in the file");
        assert_eq!(tracks, 2, "both sounds are in the file");
        assert!(
            recorder.audio(0, &[0; 3]).is_err(),
            "half a sample frame is not muxed"
        );
    }

    #[test]
    fn a_clip_of_what_just_happened_carries_the_sound_it_held() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder =
            Recorder::with_audio(Settings::for_size(64, 48), vec!["Microphone".into()]).unwrap();
        let per_frame = AUDIO_BYTES_PER_VIDEO_FRAME;
        recorder.frame(&picture(1)).unwrap();
        recorder.audio(0, &vec![7; per_frame]).unwrap();
        recorder.frame(&picture(2)).unwrap();
        recorder.audio(0, &vec![8; per_frame]).unwrap();
        let path = dir.path().join(format!("replay.{}", recorder.extension()));
        assert_eq!(recorder.save_replay(path.clone()).unwrap(), 2);
        let (frames, tracks) = inspect(&path);
        assert_eq!(
            (frames, tracks),
            (2, 1),
            "the clip carries its frames and its sound"
        );
    }
}
