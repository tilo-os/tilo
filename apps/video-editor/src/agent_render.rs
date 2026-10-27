//! The renders an agent asks of a timeline: `export-video-content` (the
//! timeline as an MP4 kept as content), `export-video-status`,
//! `cancel-video-export`, `view-frame-content` (one still as a PNG), and
//! the editor's own `render-timeline` when an agent invokes it.
//!
//! An agent's record lives in a confined app worker with no GPU, no media
//! stack and no paths, so these are tagged `host_render`
//! ([`tilo_actions::HOST_RENDER_TAG`]): an agent's host answers them in its
//! render service, which runs the confined `tilo-video-render` helper (its
//! own crate: the process, its confinement and its channel to the host are
//! platform code) on a snapshot of the record. This module holds what both
//! sides agree on — the settings an agent may ask for, the answers it gets
//! back, which timelines it may render — what the helper renders with
//! ([`render_video`], [`render_still`]), and what a person's editor answers
//! when one of them reaches it directly (they are for agents; a person
//! renders with `render-timeline` or Export).
//!
//! An agent's render is deterministic: one software encoder
//! ([`AGENT_VIDEO_ENCODER`]) with fixed settings, and the container's clock
//! times zeroed ([`zero_mp4_times`]), so two renders of one timeline are the
//! same bytes and a repeated export is recognisably the same content.

use serde_json::{Map, Value, json};

use crate::model::Project;

#[cfg(all(feature = "gst", feature = "decode"))]
mod run;
#[cfg(all(feature = "gst", feature = "decode"))]
pub use run::{render_still, render_video, warm_up};

/// The longest timeline an agent's render encodes, in seconds: a render
/// holds the host's one render slot for its whole run, and two minutes of
/// 1080p is already most of what a content item may weigh.
// ratchet-allow(pixel_const): seconds of video, not a length on screen
pub const AGENT_MAX_SECONDS: f64 = 120.0;

/// How long an agent's export waits for its render when it names no `wait`,
/// and the most it may name, in seconds.
pub const AGENT_DEFAULT_WAIT: u64 = 60;
pub const AGENT_MAX_WAIT: u64 = 120;

/// The frame rates an agent may ask for: the editor's presets.
pub const AGENT_FRAME_RATES: [u32; 3] = [24, 30, 60];

/// The software encoder every agent render writes H.264 with.
///
/// `OpenH264` is BSD-licensed and Baseline-only, and it is the one encoder
/// that is both free to ship and deterministic single-threaded: two renders
/// of one timeline are byte-equal. A hardware encoder is neither.
pub const AGENT_VIDEO_ENCODER: &str = "openh264enc";

/// What an agent's render answers when this host's `GStreamer` lacks
/// [`AGENT_VIDEO_ENCODER`].
pub const AGENT_ENCODER_MISSING: &str = "this host cannot encode an agent's video: the \
    GStreamer OpenH264 encoder (openh264enc, from the openh264 plugin of gst-plugins-bad) is \
    not installed";

/// The message a cancelled render reports.
pub const CANCELLED: &str = crate::export::CANCELLED;

/// What a person's editor answers when an agent's render reaches it: these
/// are made by an agent host's render service, never in a window.
pub const HOST_ONLY: &str = "this render is made by an agent host's render service \
    (tilo-video-render), not in the editor: render with Render video, or export a file";

/// An agent's video export, as `export-video-content` names it: MP4 / H.264
/// (Baseline, through [`AGENT_VIDEO_ENCODER`]) at an even frame size within
/// 1920×1080 (either way round) and one of [`AGENT_FRAME_RATES`].
///
/// Explicit numbers from a caller that has no dialog, and the render's
/// identity: two exports of one timeline with equal settings are one render.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct AgentExportSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for AgentExportSettings {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
        }
    }
}

/// `aspect` when it is a usable width over height, else 16:9.
fn usable_aspect(aspect: f64) -> f64 {
    if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    }
}

/// `value` rounded to the nearest even whole number, at least 2.
fn even(value: f64) -> u64 {
    ((value / 2.0).round() * 2.0).clamp(2.0, 1e6) as u64
}

impl AgentExportSettings {
    /// The settings `args` name, for a project of `aspect` (width over
    /// height): `format` (mp4, the only one), `width`, `height`, `fps`.
    /// Neither dimension named is 1280×720; one named gives the other from
    /// the project's aspect, rounded to even.
    ///
    /// # Errors
    ///
    /// Why the arguments name no render an agent may ask for, in words the
    /// caller can correct.
    pub fn from_args(args: &Map<String, Value>, aspect: f64) -> Result<Self, String> {
        let args = AgentArgs::read(args)?;
        args.mp4_only()?;
        let aspect = usable_aspect(aspect);
        let width = whole("width", args.width)?;
        let height = whole("height", args.height)?;
        let (width, height) = match (width, height) {
            (None, None) => (1280, 720),
            (Some(width), None) => (width, even(width as f64 / aspect)),
            (None, Some(height)) => (even(height as f64 * aspect), height),
            (Some(width), Some(height)) => (width, height),
        };
        let fps = whole("fps", args.fps)?.unwrap_or(30);
        Self::checked(width, height, fps)
    }

    /// The settings the editor's `render-timeline` names for an agent: `format` (mp4
    /// only, as every agent render) and `size` (`720p` or `1080p`, the
    /// height; the width follows the project's aspect), at 30 frames a
    /// second.
    ///
    /// # Errors
    ///
    /// A format other than mp4, or a size an agent may not render.
    pub fn from_render_args(args: &Map<String, Value>, aspect: f64) -> Result<Self, String> {
        let args = AgentArgs::read(args)?;
        args.mp4_only()?;
        let height = match args.size.as_deref().map(str::trim) {
            None | Some("1080p") => 1080,
            Some("720p") => 720,
            Some(other) => {
                return Err(format!("an agent's render is 720p or 1080p, not {other}"));
            }
        };
        let width = even(height as f64 * usable_aspect(aspect));
        Self::checked(width, height, 30)
    }

    fn checked(width: u64, height: u64, fps: u64) -> Result<Self, String> {
        for (name, value) in [("width", width), ("height", height)] {
            if value % 2 != 0 {
                return Err(format!("the {name} must be even (H.264), not {value}"));
            }
            if value < 16 {
                return Err(format!(
                    "the {name} must be at least 16 pixels, not {value}"
                ));
            }
        }
        if width.max(height) > 1920 || width.min(height) > 1080 {
            return Err(format!(
                // ratchet-allow(glyph_icon): the multiplication sign of a size, text rather than an icon
                "{width}×{height} is larger than an agent's render may be: at most 1920×1080, \
                 either way round"
            ));
        }
        if !AGENT_FRAME_RATES.iter().any(|rate| u64::from(*rate) == fps) {
            return Err(format!("the frame rate is 24, 30 or 60, not {fps}"));
        }
        Ok(Self {
            width: u32::try_from(width).map_err(|_| "the width is too large".to_owned())?,
            height: u32::try_from(height).map_err(|_| "the height is too large".to_owned())?,
            fps: u32::try_from(fps).map_err(|_| "the frame rate is too large".to_owned())?,
        })
    }

    /// The video bitrate the encoder is asked for, in bits per second: 2.5
    /// megabits at 720p30, scaled by pixels per second and capped at 3.5, so
    /// two minutes stay under the 60 MiB a render may keep when the encoder
    /// holds to its rate (a very busy picture can come out larger, and such
    /// a render is refused by size).
    #[must_use]
    pub fn video_bitrate(&self) -> u64 {
        let pixels = u64::from(self.width) * u64::from(self.height) * u64::from(self.fps);
        let base = 1280 * 720 * 30;
        (2_500_000 * pixels / base).clamp(500_000, 3_500_000)
    }
}

/// How long an agent's call waits for its render, from its `wait` (seconds,
/// at most [`AGENT_MAX_WAIT`]; [`AGENT_DEFAULT_WAIT`] when absent).
///
/// # Errors
///
/// A `wait` that is not a whole number of seconds from 0.
pub fn wait_from_args(args: &Map<String, Value>) -> Result<std::time::Duration, String> {
    let wait = whole("wait", AgentArgs::read(args)?.wait)?.unwrap_or(AGENT_DEFAULT_WAIT);
    Ok(std::time::Duration::from_secs(wait.min(AGENT_MAX_WAIT)))
}

/// One still of the timeline an agent asks for (`view-frame-content`): the
/// time, and the size it is drawn at (the height follows the project's
/// aspect).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentFrameSettings {
    pub at: f64,
    pub width: u32,
    pub height: u32,
}

impl AgentFrameSettings {
    /// The still `args` name: `at` seconds (required, within the timeline of
    /// `duration` seconds), `width` (default 640, even, 16 to 1920).
    ///
    /// # Errors
    ///
    /// Why the arguments name no still of this timeline.
    pub fn from_args(
        args: &Map<String, Value>,
        aspect: f64,
        duration: f64,
    ) -> Result<Self, String> {
        let args = AgentArgs::read(args)?;
        let at = args.at.ok_or("name the time to view, in seconds: `at`")?;
        if !at.is_finite() || at < 0.0 || at > duration.max(0.0) {
            return Err(format!(
                "{at} s is not on the timeline, which runs from 0 to {duration} s"
            ));
        }
        let width = whole("width", args.width)?.unwrap_or(640);
        if width % 2 != 0 || !(16..=1920).contains(&width) {
            return Err(format!(
                "the width is an even number from 16 to 1920, not {width}"
            ));
        }
        let height = even(width as f64 / usable_aspect(aspect)).min(1920);
        Ok(Self {
            at,
            width: u32::try_from(width).map_err(|_| "the width is too large".to_owned())?,
            height: u32::try_from(height).map_err(|_| "the height is too large".to_owned())?,
        })
    }
}

/// What a finished agent render made, as its caller is told.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentRendered {
    pub frames: u64,
    pub duration: f64,
}

/// The answer of a finished render: its content address and what it holds.
#[must_use]
pub fn done_answer(
    content: &str,
    rendered: AgentRendered,
    settings: AgentExportSettings,
    bytes: u64,
) -> Value {
    json!({
        "state": "done",
        "content": content,
        "format": "mp4",
        "duration": rendered.duration,
        "frames": rendered.frames,
        "width": settings.width,
        "height": settings.height,
        "fps": settings.fps,
        "bytes": bytes,
    })
}

/// The answer of a render still running after the caller's wait: the job to
/// ask about again (`export-video-status`) and how far it has come.
#[must_use]
pub fn rendering_answer(job: &str, done: u64, total: u64) -> Value {
    json!({ "state": "rendering", "job": job, "done": done, "total": total })
}

/// The answer of a render that failed or was cancelled.
#[must_use]
pub fn failed_answer(job: &str, error: &str) -> Value {
    json!({ "state": "failed", "job": job, "error": error })
}

/// The sources `project` places on its timeline: imported media (not
/// titles) that some clip uses.
fn placed(project: &Project) -> impl Iterator<Item = &crate::model::MediaItem> {
    project.media.iter().filter(|item| {
        !item.kind.is_generated() && project.clips.iter().any(|clip| clip.media == item.id)
    })
}

/// Why `project` is no timeline an agent's render takes: empty, longer than
/// [`AGENT_MAX_SECONDS`], or placing a source that is not room content.
///
/// # Errors
///
/// The refusal, naming the offending source.
pub fn renderable(project: &Project) -> Result<(), String> {
    if project.clips.is_empty() {
        return Err("the timeline is empty: there is nothing to render".into());
    }
    let duration = project.export_duration();
    if duration > AGENT_MAX_SECONDS {
        return Err(format!(
            "the timeline runs {duration:.1} s; an agent's render takes at most \
             {AGENT_MAX_SECONDS:.0} s (mark a range, or shorten the cut)"
        ));
    }
    if let Some(item) =
        placed(project).find(|item| tilo_actions::content_hash(&item.path).is_none())
    {
        return Err(format!(
            "{} is not room content: an agent's render reads only sources imported with \
             import-content",
            item.name
        ));
    }
    Ok(())
}

/// The room content every source `project` places is read from, once each,
/// lowercase.
#[must_use]
pub fn placed_content(project: &Project) -> Vec<String> {
    let mut hashes: Vec<String> = Vec::new();
    for item in placed(project) {
        if let Some(hash) = tilo_actions::content_hash(&item.path) {
            let hash = hash.to_ascii_lowercase();
            if !hashes.contains(&hash) {
                hashes.push(hash);
            }
        }
    }
    hashes
}

/// `project` rendering only `start..end` (each defaulting to its render
/// range): what the editor's `render-timeline` names.
///
/// # Errors
///
/// A range that does not lie on the timeline.
pub fn ranged(project: &Project, start: Option<f64>, end: Option<f64>) -> Result<Project, String> {
    if start.is_none() && end.is_none() {
        return Ok(project.clone());
    }
    let (from, to) = project.export_range();
    let (start, end) = (start.unwrap_or(from), end.unwrap_or(to));
    if !(start.is_finite() && end.is_finite() && start >= 0.0 && end > start) {
        return Err("a render begins at or after the start and ends after it begins".into());
    }
    let mut whole = project.clone();
    whole.clear_marks();
    let (_, last) = whole.export_range();
    if start >= last {
        return Err(format!(
            "the timeline ends at {last:.2} s, before the render begins"
        ));
    }
    whole.set_mark_out(end.min(last));
    whole.set_mark_in(start);
    Ok(whole)
}

/// The `GStreamer` graph an agent's render runs: the composited RGBA frames
/// through [`AGENT_VIDEO_ENCODER`] with fixed settings — one thread, one
/// slice, no frame skipping, a keyframe every second — into `mp4mux` and
/// `sink`, with the timeline's mix beside it when there is any. The muxer
/// writes in place (no faststart: that needs a scratch file a confined
/// helper may not make).
#[must_use]
pub fn pipeline_desc(settings: &AgentExportSettings, sink: &str, with_audio: bool) -> String {
    let AgentExportSettings { width, height, fps } = *settings;
    let bitrate = settings.video_bitrate();
    let audio = with_audio
        .then(|| crate::export::audio_branch_desc(crate::export::Container::Mp4, "tilo_mux"))
        .flatten()
        .map(|branch| format!(" {branch}"))
        .unwrap_or_default();
    format!(
        "appsrc name=tilo_src is-live=false format=time \
         caps=video/x-raw,format=RGBA,width={width},height={height},framerate={fps}/1 \
         ! videoconvert ! video/x-raw,format=I420 \
         ! {AGENT_VIDEO_ENCODER} gop-size={fps} multi-thread=1 slice-mode=n-slices num-slices=1 \
         enable-frame-skip=false rate-control=bitrate bitrate={bitrate} \
         ! h264parse ! mp4mux name=tilo_mux faststart=false ! {sink}{audio}"
    )
}

/// The timeline a record's saved document (`AutoCommit::save`) holds, as a
/// render reads it from the snapshot its host hands over.
///
/// # Errors
///
/// The bytes are no document, or the document no timeline.
pub fn read_snapshot(bytes: &[u8]) -> Result<Project, String> {
    ui::tilo_resource::Replica::<Project>::load(bytes)
        .and_then(|replica| replica.state())
        .map_err(|error| format!("the timeline does not read: {error}"))
}

/// Zero the creation and modification times an MP4's movie, track and
/// media headers (`mvhd`, `tkhd`, `mdhd`) carry: the muxer's wall clock is
/// the one thing that would make two renders of one timeline differ.
/// Returns how many headers were changed.
pub fn zero_mp4_times(data: &mut [u8]) -> usize {
    let mut fields: Vec<std::ops::Range<usize>> = Vec::new();
    for moov in boxes(data, 0..data.len()).filter(|b| &b.kind == b"moov") {
        for child in boxes(data, moov.body.clone()) {
            match &child.kind {
                b"mvhd" => fields.extend(header_times(data, &child.body)),
                b"trak" => {
                    for part in boxes(data, child.body.clone()) {
                        match &part.kind {
                            b"tkhd" => fields.extend(header_times(data, &part.body)),
                            b"mdia" => {
                                for media in boxes(data, part.body.clone()) {
                                    if &media.kind == b"mdhd" {
                                        fields.extend(header_times(data, &media.body));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut changed = 0;
    for field in fields {
        if let Some(times) = data.get_mut(field) {
            times.fill(0);
            changed += 1;
        }
    }
    changed
}

/// One ISO-BMFF box: its four-letter kind and where its body lies.
struct BoxAt {
    kind: [u8; 4],
    body: std::ops::Range<usize>,
}

/// The boxes laid one after another in `data[within]`; a malformed size
/// ends the walk.
fn boxes(data: &[u8], within: std::ops::Range<usize>) -> impl Iterator<Item = BoxAt> + '_ {
    let mut at = within.start;
    let end = within.end.min(data.len());
    std::iter::from_fn(move || {
        let head = data.get(at..at.checked_add(8)?.min(end))?;
        let size32 = u32::from_be_bytes(head.get(..4)?.try_into().ok()?);
        let kind: [u8; 4] = head.get(4..8)?.try_into().ok()?;
        let (size, header) = match size32 {
            0 => (end.checked_sub(at)?, 8),
            1 => {
                let large = data.get(at.checked_add(8)?..at.checked_add(16)?)?;
                (
                    usize::try_from(u64::from_be_bytes(large.try_into().ok()?)).ok()?,
                    16,
                )
            }
            n => (usize::try_from(n).ok()?, 8),
        };
        let next = at.checked_add(size)?;
        if size < header || next > end {
            return None;
        }
        let body = at.checked_add(header)?..next;
        at = next;
        Some(BoxAt { kind, body })
    })
}

/// Where a full box header's two times lie in `body`: after version and
/// flags, 32-bit each in version 0, 64-bit in version 1.
fn header_times(data: &[u8], body: &std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
    let width = match data.get(body.start)? {
        0 => 4,
        1 => 8,
        _ => return None,
    };
    let start = body.start.checked_add(4)?;
    let end = start.checked_add(2 * width)?;
    (end <= body.end).then_some(start..end)
}

/// The arguments an agent's render verbs read, from the arguments bound to
/// the call. A verb's other arguments (an export's range) are its own.
#[derive(serde::Deserialize)]
struct AgentArgs {
    format: Option<String>,
    size: Option<String>,
    width: Option<f64>,
    height: Option<f64>,
    fps: Option<f64>,
    wait: Option<f64>,
    at: Option<f64>,
}

impl AgentArgs {
    fn read(args: &Map<String, Value>) -> Result<Self, String> {
        serde_json::from_value(Value::Object(args.clone()))
            .map_err(|error| format!("the arguments do not read: {error}"))
    }

    fn mp4_only(&self) -> Result<(), String> {
        match self.format.as_deref().map(str::trim) {
            None => Ok(()),
            Some(format) if format.eq_ignore_ascii_case("mp4") => Ok(()),
            Some(format) => Err(format!("an agent's video renders as mp4, not {format}")),
        }
    }
}

/// A whole, non-negative number argument; `30.0` reads as `30`.
fn whole(name: &str, value: Option<f64>) -> Result<Option<u64>, String> {
    value
        .map(|n| {
            if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n < 1e9 {
                Ok(n as u64)
            } else {
                Err(format!("`{name}` is a whole number from 0, not {n}"))
            }
        })
        .transpose()
}

#[cfg(test)]
mod tests;
