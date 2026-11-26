//! Export settings and the render pipeline.
//!
//! The encode runs through `GStreamer`, linked by the default `gst` feature. A
//! `--no-default-features` developer build keeps the export UI testable without it and
//! reports that encoding is unavailable. Headless screenshots stay hermetic because they
//! never start an encoder.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use media::encode::{AudioCodec, EncodeReport, HwEncPolicy, Selection, VideoCodec};

use video_edit::model::Project;

#[cfg(not(any(feature = "gst", feature = "decode")))]
mod stepped;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    P720,
    P1080,
    P2160,
}

impl Resolution {
    pub const ALL: [Resolution; 3] = [Resolution::P720, Resolution::P1080, Resolution::P2160];
    pub fn label(self) -> &'static str {
        match self {
            Resolution::P720 => "720p",
            Resolution::P1080 => "1080p",
            Resolution::P2160 => "2160p",
        }
    }
    pub fn dims(self) -> (u32, u32) {
        match self {
            Resolution::P720 => (1280, 720),
            Resolution::P1080 => (1920, 1080),
            Resolution::P2160 => (3840, 2160),
        }
    }
    pub fn target_mbps(self) -> f64 {
        match self {
            Resolution::P720 => 8.0,
            Resolution::P1080 => 16.0,
            Resolution::P2160 => 45.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    WebM,
    Mov,
}

impl Container {
    pub const ALL: [Container; 3] = [Container::Mp4, Container::WebM, Container::Mov];

    /// The containers this copy writes. A copy that encodes with the
    /// browser's own encoder ([`media::edit::ClipEncoder`]) writes `WebM`
    /// alone, with whichever royalty-free codec the browser has.
    pub fn offered() -> &'static [Container] {
        if media::edit::ClipEncoder::available() {
            &[Container::WebM]
        } else {
            &Self::ALL
        }
    }

    /// What the encoder actually produces — not what the container is
    /// capable of. `WebM` here is VP8 and MOV is H.264, because that is what
    /// [`encode_chain`] plugs; naming VP9 or `ProRes` would be telling the
    /// user they are getting a codec this pipeline never writes.
    pub fn label(self) -> &'static str {
        match self {
            // The browser picks the codec (VP9 where it can), so the label
            // names only what is certain.
            Container::WebM if media::edit::ClipEncoder::available() => "WebM",
            Container::Mp4 => "MP4 / H.264",
            Container::WebM => "WebM / VP8",
            Container::Mov => "MOV / H.264",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::WebM => "webm",
            Container::Mov => "mov",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRate {
    F24,
    F30,
    F60,
}

impl FrameRate {
    pub const ALL: [FrameRate; 3] = [FrameRate::F24, FrameRate::F30, FrameRate::F60];
    pub fn label(self) -> &'static str {
        match self {
            FrameRate::F24 => "24",
            FrameRate::F30 => "30",
            FrameRate::F60 => "60",
        }
    }
    pub fn fps(self) -> f64 {
        match self {
            FrameRate::F24 => 24.0,
            FrameRate::F30 => 30.0,
            FrameRate::F60 => 60.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportSettings {
    pub resolution: Resolution,
    pub container: Container,
    pub frame_rate: FrameRate,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            resolution: Resolution::P1080,
            container: Container::offered()
                .first()
                .copied()
                .unwrap_or(Container::Mp4),
            frame_rate: FrameRate::F30,
        }
    }
}

impl ExportSettings {
    /// The sum of what the two encoders are *asked* for: [`Resolution::target_mbps`] for the
    /// picture and [`AUDIO_BITRATE_BPS`] for the sound, the numbers
    /// [`pipeline_desc_with_audio`] writes into the graph.
    ///
    /// `with_audio` is whether this export muxes an audio track at all
    /// ([`video_edit::audio::has_audio`]); quoting the allowance for a video-only timeline bills for
    /// a track the file never carries.
    ///
    /// How closely the file lands on the total is the encoders' business.
    /// [`Self::estimate_label`] is what a dialog should show.
    pub fn estimated_mb(&self, duration: f64, with_audio: bool) -> f64 {
        let audio_mbps = if with_audio {
            AUDIO_BITRATE_BPS as f64 / 1_000_000.0
        } else {
            0.0
        };
        (self.resolution.target_mbps() + audio_mbps) * duration / 8.0
    }

    /// Either an approximate megabyte figure, or — where one cannot honestly be given — the rate
    /// requested and a plain statement that the size does not follow from it.
    ///
    /// The distinction is [`media::encode::Selection::bitrate_is_predictive`], false for an
    /// encoder measured to ignore its bitrate: `openh264enc` asked for 1 Mbit/s of noise writes
    /// 59 and `vp8enc` writes 14. A box with only those gets no figure, which there would be a
    /// guess dressed as a measurement.
    pub fn estimate_label(&self, project: &Project) -> String {
        let (video, _, _) = encode_chain(self.container);
        self.estimate_label_for(project, &video)
    }

    /// Split out so both branches can be exercised wherever the tests run: a box with no hardware
    /// encoder resolves every container to an encoder that ignores its bitrate.
    pub fn estimate_label_for(
        &self,
        project: &Project,
        video: &media::encode::Selection,
    ) -> String {
        let with_audio = video_edit::audio::has_audio(project);
        if video.bitrate_is_predictive() {
            // The range, not the timeline: quoting the whole project's size
            // for a marked ten-second render would bill for footage the file
            // never carries.
            let megabytes = self.estimated_mb(project.export_duration(), with_audio);
            return format!(
                "≈ {}",
                ui::format::size((megabytes.max(0.0) * 1_000_000.0).round() as u64)
            );
        }
        let audio = if with_audio {
            format!(" + {} kbit/s audio", AUDIO_BITRATE_BPS / 1000)
        } else {
            String::new()
        };
        format!("{:.0} Mbit/s{audio}", self.resolution.target_mbps())
    }

    pub fn suggested_filename(&self, project: &str) -> String {
        let slug: String = project
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        format!("{slug}.{}", self.container.extension())
    }
}

/// Where a render job is in its lifecycle. The dialog renders a progress
/// bar from this; a worker thread (when `gst` is enabled) advances it.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportStatus {
    Idle,
    Rendering { progress: f64 },
    Done { path: String },
    Failed { reason: String },
}

/// The video codec each container carries. The *codec* is fixed by the
/// container (and named in [`Container::label`]); which *encoder* writes it is
/// chosen per machine — see [`encode_chain`].
pub fn video_codec(container: Container) -> VideoCodec {
    match container {
        Container::Mp4 | Container::Mov => VideoCodec::H264,
        Container::WebM => VideoCodec::Vp8,
    }
}

pub fn audio_codec(container: Container) -> AudioCodec {
    match container {
        Container::Mp4 | Container::Mov => AudioCodec::Aac,
        Container::WebM => AudioCodec::Opus,
    }
}

pub fn mux_chain(container: Container) -> (Option<&'static str>, &'static str) {
    match container {
        Container::Mp4 => (Some("h264parse"), "mp4mux"),
        Container::Mov => (Some("h264parse"), "qtmux"),
        Container::WebM => (None, "webmmux"),
    }
}

/// Whether an element factory is installed — where this build can look.
///
/// `media`'s registry probe is only compiled where `GStreamer` is linked, which
/// for this crate means a `decode` or `gst` build. Without either, nothing can
/// be confirmed, and the selection says so ([`Selection::confirmed`]) rather
/// than assuming an encoder is present or absent.
#[cfg(any(feature = "decode", feature = "gst"))]
fn registry_available(name: &str) -> bool {
    media::encode::registry_has(name)
}

#[cfg(not(any(feature = "decode", feature = "gst")))]
fn registry_available(_name: &str) -> bool {
    false
}

/// The `GStreamer` chain per container: the (video encoder, optional parser, muxer) triple.
///
/// The encoder is chosen against the machine's real registry — VA-API, NVENC or QSV when
/// installed, then `x264enc`, then `openh264enc` — and the returned [`Selection`] reports
/// hardware only when the registry confirmed one. The muxer stays fixed by the container:
/// MP4 / MOV to `mp4mux` / `qtmux`, `WebM` to `webmmux`.
pub fn encode_chain(container: Container) -> (Selection, Option<&'static str>, &'static str) {
    let (parse, mux) = mux_chain(container);
    let sel = media::encode::select_video(
        video_codec(container),
        HwEncPolicy::from_env(),
        registry_available,
    );
    (sel, parse, mux)
}

/// The audio encoder each container takes, chosen the same way as the video
/// one. There is no hardware AAC or Opus encoder to prefer, so the ordering
/// here is by quality — `fdkaacenc` ahead of `avenc_aac`.
pub fn audio_encoder(container: Container) -> Selection {
    media::encode::select_audio(
        audio_codec(container),
        HwEncPolicy::from_env(),
        registry_available,
    )
}

/// Which encoders an export of `container` would actually use on this machine.
///
/// An observation, not a capability claim: [`EncodeReport::is_hardware`] is
/// false both for a software encoder and for a build that could not consult a
/// registry, so "not yet known" can never be read as "yes".
pub fn selected_encoders(container: Container) -> EncodeReport {
    let (video, _, _) = encode_chain(container);
    EncodeReport {
        video: Some(video),
        audio: Some(audio_encoder(container)),
    }
}

/// 128 kbit/s is transparent enough for stereo AAC and comfortable for Opus, and it is set
/// explicitly because the encoders' defaults disagree (`opusenc` starts at 64, `avenc_aac` at
/// 128, `fdkaacenc` at auto), so the same project would land at a different rate depending on
/// what the machine has. [`ExportSettings::estimated_mb`] charges for this same number.
pub const AUDIO_BITRATE_BPS: u64 = 128_000;

/// The audio half of the graph: a raw-PCM `appsrc` fed the mixed timeline,
/// converted and encoded into the same muxer the video goes to.
///
/// Kept next to [`pipeline_desc`] and built from the same pieces so the two
/// branches cannot drift, and so the whole graph is unit-testable without a
/// `GStreamer` stack.
pub fn audio_branch_desc(container: Container, muxer_name: &str) -> Option<String> {
    let enc = audio_encoder(container);
    Some(format!(
        "appsrc name=tilo_audio is-live=false format=time \
         caps=audio/x-raw,format=S16LE,channels={ch},rate={rate},layout=interleaved \
         ! audioconvert ! audioresample ! {enc} {bitrate} ! {muxer_name}.",
        ch = video_edit::audio::CHANNELS,
        rate = video_edit::audio::SAMPLE_RATE,
        enc = enc.spec.element,
        bitrate = enc.spec.bitrate_arg(AUDIO_BITRATE_BPS),
    ))
}

/// The flush after the last frame is real work — a long timeline has an index to write — and a
/// flat two-minute bound deleted renders that had legitimately taken longer. What distinguishes
/// a wedged pipeline from a slow one is whether anything is happening, so this watches for the
/// output growing.
#[cfg(any(feature = "gst", test))]
#[derive(Debug)]
struct FlushWatch {
    last_len: u64,
    last_progress: ui::time::Instant,
}

#[cfg(any(feature = "gst", test))]
impl FlushWatch {
    fn new(len: u64, now: ui::time::Instant) -> Self {
        Self {
            last_len: len,
            last_progress: now,
        }
    }

    fn observe_len(&mut self, len: u64, now: ui::time::Instant) -> bool {
        if len == self.last_len {
            return false;
        }
        self.last_len = len;
        self.last_progress = now;
        true
    }

    fn observe_activity(&mut self, now: ui::time::Instant) {
        self.last_progress = now;
    }

    fn stalled(&self, now: ui::time::Instant, limit: std::time::Duration) -> bool {
        now.duration_since(self.last_progress) >= limit
    }
}

/// Build the `gst-launch`-style pipeline description the export runs: a raw
/// RGBA `appsrc` (fed the composited frames) through `videoconvert`, the
/// container's encode chain, and a `filesink`. Pure, so the exact graph is
/// unit-tested on every build even where gstreamer can't link.
pub fn pipeline_desc(settings: &ExportSettings, w: u32, h: u32, out: &str) -> String {
    pipeline_desc_with_audio(settings, w, h, out, false)
}

/// [`pipeline_desc`], with a second branch that muxes the timeline's audio.
///
/// The muxer is named so the audio branch can link to it, which is the only
/// structural difference — a muxer with two pads instead of one. Export used
/// to have no audio branch at all, so every audio track in a project was
/// silently discarded.
pub fn pipeline_desc_with_audio(
    settings: &ExportSettings,
    w: u32,
    h: u32,
    out: &str,
    with_audio: bool,
) -> String {
    let (enc, parse, mux) = encode_chain(settings.container);
    let fps = settings.frame_rate.fps() as u32;
    let parse = parse.map(|p| format!("{p} ! ")).unwrap_or_default();
    let bitrate = (settings.resolution.target_mbps() * 1_000_000.0) as u64;
    // Each encoder names and scales its bitrate differently — `bitrate` in
    // bits for openh264, `target-bitrate` in bits for libvpx, `bitrate` in
    // *kilo*bits for every hardware encoder and x264. The spec converts, so
    // the number the size estimate quotes is the rate the encoder is asked
    // for whichever one was selected. The encoder's fixed settings follow
    // (no B-frames for x264, so the file's first picture is at zero).
    let encoder_args = enc.spec.launch_args(bitrate);
    let enc = enc.spec.element;
    let audio = with_audio
        .then(|| audio_branch_desc(settings.container, "tilo_mux"))
        .flatten()
        .map(|b| format!(" {b}"))
        .unwrap_or_default();
    // This is GStreamer's launch syntax, not a shell command. A path must
    // remain one quoted property even when it contains quotes or graph syntax.
    let out = out.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "appsrc name=tilo_src is-live=false format=time \
         caps=video/x-raw,format=RGBA,width={w},height={h},framerate={fps}/1 \
         ! videoconvert ! {enc} {encoder_args} ! {parse}{mux} name=tilo_mux \
         ! filesink location=\"{out}\"{audio}"
    )
}

/// Output dimensions for the export: the resolution preset's height, with the
/// width scaled to the project's aspect ratio and rounded to an even number
/// (H.264 requires even dimensions).
pub fn output_dims(settings: &ExportSettings, project: &Project) -> (u32, u32) {
    let (_pw, ph) = settings.resolution.dims();
    // The project's own aspect, through its one accessor — so a vertical or
    // square project exports vertical or square instead of being letterboxed
    // into the 16:9 the frame used to be fixed at.
    let aspect = project.aspect();
    let h = ph.max(2);
    let mut w = (f64::from(h) * aspect).round() as u32;
    w += w & 1; // make even
    (w.max(2), h)
}

/// Split out of the encode loop so what gets rendered can be asserted without an encoder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderPlan {
    pub start: f64,
    pub total: u64,
    fps: f64,
}

impl RenderPlan {
    #[must_use]
    pub fn new(project: &Project, fps: f64) -> Self {
        let fps = if fps.is_finite() && fps > 0.0 {
            fps
        } else {
            1.0
        };
        let (start, end) = project.export_range();
        let duration = (end - start).max(1.0 / fps);
        // Subtracting two frame-aligned edit points can land a few ULPs
        // above an integer (e.g. (0.8 - 0.5) * 30). Ceil would encode an
        // extra frame outside the cut. Only absorb arithmetic roundoff;
        // genuinely fractional frames still round up.
        let frames = duration * fps;
        let nearest = frames.round();
        // Include the original edit points' magnitude: subtracting two
        // hour-long timestamps loses more precision than subtracting two
        // small ones, even when both describe the same short cut.
        let precision_scale = project
            .clips
            .iter()
            .fold(start.abs().max(end.abs()).max(1.0), |scale, c| {
                scale.max(c.in_point.abs().max(c.out_point.abs()) / c.speed.rate())
            });
        let roundoff = 8.0 * f64::EPSILON * precision_scale * fps;
        let frames = if (frames - nearest).abs() <= roundoff {
            nearest
        } else {
            frames
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a frame count is a positive whole number; the ceil is taken first"
        )]
        let total = frames.ceil().max(1.0) as u64;
        Self { start, total, fps }
    }

    /// The timeline time frame `i` is composited at. The file's own timestamps
    /// stay zero-based, so a range that begins a minute in still writes a file
    /// that starts at zero.
    #[must_use]
    pub fn time_of(self, i: u64) -> f64 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a frame index within a render is far inside f64's exact range"
        )]
        let i = i as f64;
        self.start + i / self.fps
    }
}

/// The absolute path the export writes to. Honours `$TILO_VE_EXPORT_OUT` (a full
/// path, used by tests) or `$TILO_VE_EXPORT_DIR` (a directory), else the system
/// temp dir, always with the settings-derived filename.
pub fn output_path(settings: &ExportSettings, project: &Project) -> std::path::PathBuf {
    if let Some(full) = std::env::var_os("TILO_VE_EXPORT_OUT") {
        return std::path::PathBuf::from(full);
    }
    let dir = std::env::var_os("TILO_VE_EXPORT_DIR")
        .map(std::path::PathBuf::from)
        .or_else(tilo_paths::temp_dir)
        .unwrap_or_default();
    dir.join(settings.suggested_filename(&project.name))
}

/// The file a render run a step at a time made, for its job to write.
#[cfg(not(any(feature = "gst", feature = "decode")))]
pub struct Rendered {
    pub bytes: Vec<u8>,
    /// The project has sound this browser could not encode.
    pub without_sound: bool,
}

pub struct ExportJob {
    shared: Arc<JobState>,
    handle: Option<std::thread::JoinHandle<()>>,
    #[cfg(not(any(feature = "gst", feature = "decode")))]
    stepped: Option<stepped::SteppedRender>,
}

struct JobState {
    /// Frames written so far, and how many there are in total.
    done: AtomicU64,
    total: AtomicU64,
    cancel: Arc<AtomicBool>,
    outcome: Mutex<Option<ExportStatus>>,
    report: Option<Box<dyn Fn(u64, u64) + Send + Sync>>,
    /// How long one frame may go without every source's real picture before
    /// the export fails rather than write a stand-in.
    #[cfg_attr(
        not(feature = "gst"),
        allow(dead_code, reason = "only the GStreamer render composites frames")
    )]
    frame_patience: std::time::Duration,
}

impl ExportJob {
    /// Start a render that runs a step at a time on the calling thread
    /// (each [`Self::pump`]) and hands its file to the receiver it returns,
    /// which closes without one when the render stops: how a browser page
    /// exports, with the encoder the browser has.
    #[cfg(not(any(feature = "gst", feature = "decode")))]
    pub fn stepped(
        project: &Project,
        settings: ExportSettings,
    ) -> (Self, std::sync::mpsc::Receiver<Rendered>) {
        let (rendered, file) = std::sync::mpsc::channel();
        let shared = Arc::new(JobState {
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            cancel: Arc::default(),
            outcome: Mutex::new(None),
            frame_patience: EXPORT_FRAME_PATIENCE,
            report: None,
        });
        let stepped = match stepped::SteppedRender::start(project, settings, rendered, &shared) {
            Ok(render) => Some(render),
            Err(reason) => {
                if let Ok(mut slot) = shared.outcome.lock() {
                    *slot = Some(ExportStatus::Failed { reason });
                }
                None
            }
        };
        let job = Self {
            shared,
            handle: None,
            stepped,
        };
        (job, file)
    }

    pub fn pump(&mut self) {
        #[cfg(not(any(feature = "gst", feature = "decode")))]
        if let Some(render) = self.stepped.as_mut()
            && self.shared.outcome.lock().is_ok_and(|o| o.is_none())
        {
            render.step(&self.shared);
        }
    }

    pub fn status(&self) -> ExportStatus {
        if let Some(done) = self.shared.outcome.lock().ok().and_then(|o| o.clone()) {
            return done;
        }
        let total = self.shared.total.load(Ordering::Relaxed).max(1);
        let done = self.shared.done.load(Ordering::Relaxed);
        ExportStatus::Rendering {
            progress: (done as f64 / total as f64).clamp(0.0, 1.0),
        }
    }

    pub fn is_running(&self) -> bool {
        self.shared.outcome.lock().is_ok_and(|o| o.is_none())
    }

    /// Ask the render to stop. The worker notices between frames, tears the
    /// pipeline down and finishes as [`ExportStatus::Failed`] with a
    /// cancelled reason — a half-written file is removed rather than left
    /// looking like a finished render.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }

    pub fn join(mut self) -> ExportStatus {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.shared
            .outcome
            .lock()
            .ok()
            .and_then(|o| o.clone())
            .unwrap_or_else(|| ExportStatus::Failed {
                reason: "export worker vanished".into(),
            })
    }
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }
}

impl JobState {
    #[cfg_attr(
        not(feature = "gst"),
        allow(dead_code, reason = "only the GStreamer render writes frames")
    )]
    fn wrote(&self, done: u64) {
        self.done.store(done, Ordering::Relaxed);
        if let Some(report) = &self.report {
            report(done, self.total.load(Ordering::Relaxed));
        }
    }
}

/// `report` is told each frame written and how many there are, and setting `cancel` stops the
/// render as [`ExportJob::cancel`] does.
///
/// # Errors
/// Why no file was written, as [`ExportStatus::Failed`] says it.
pub fn render_to(
    project: &Project,
    settings: ExportSettings,
    destination: &std::path::Path,
    cancel: Arc<AtomicBool>,
    report: impl Fn(u64, u64) + Send + Sync + 'static,
) -> Result<String, String> {
    let state = JobState {
        done: AtomicU64::new(0),
        total: AtomicU64::new(0),
        cancel,
        outcome: Mutex::new(None),
        frame_patience: EXPORT_FRAME_PATIENCE,
        report: Some(Box::new(report)),
    };
    render(project, settings, destination, &state)
}

pub const CANCELLED: &str = "Export cancelled";

/// How long one request for a source's frame waits before the export checks
/// for Cancel and asks again. An export never writes a frame without every
/// source's real picture; this only bounds how late a Cancel is noticed.
#[cfg(feature = "gst")]
const EXPORT_FRAME_ATTEMPT: std::time::Duration = std::time::Duration::from_secs(2);

/// How long an export keeps asking for one frame's pictures before it fails,
/// naming the source. A slow machine opening a source and landing its first
/// picture takes seconds; a source that gives nothing for this long cannot
/// produce the frame, and a stand-in in its place is not the person's work.
const EXPORT_FRAME_PATIENCE: std::time::Duration = std::time::Duration::from_secs(120);

/// Start a render on a worker thread, writing to `destination`.
///
/// Returns immediately. Poll [`ExportJob::status`] for progress.
pub fn start_export(
    project: &Project,
    settings: &ExportSettings,
    destination: std::path::PathBuf,
) -> ExportJob {
    start_export_with_patience(project, *settings, destination, EXPORT_FRAME_PATIENCE)
}

/// [`start_export`], giving up on a frame whose pictures have not all
/// arrived after `frame_patience`.
fn start_export_with_patience(
    project: &Project,
    settings: ExportSettings,
    destination: std::path::PathBuf,
    frame_patience: std::time::Duration,
) -> ExportJob {
    let shared = Arc::new(JobState {
        done: AtomicU64::new(0),
        total: AtomicU64::new(0),
        cancel: Arc::default(),
        outcome: Mutex::new(None),
        frame_patience,
        report: None,
    });
    // The worker owns its own copy: the UI thread keeps editing the project
    // while the render runs, and a render must encode the timeline as it was
    // when the user pressed Export.
    let project = project.clone();
    let state = shared.clone();
    let handle = ui::rt::spawn_thread("video-editor-export", move || {
        let result = render(&project, settings, &destination, &state);
        let status = match result {
            Ok(path) => ExportStatus::Done { path },
            Err(reason) => ExportStatus::Failed { reason },
        };
        if let Ok(mut slot) = state.outcome.lock() {
            *slot = Some(status);
        }
    })
    .ok();
    if handle.is_none()
        && let Ok(mut slot) = shared.outcome.lock()
    {
        *slot = Some(ExportStatus::Failed {
            reason: "could not start the export worker".into(),
        });
    }
    ExportJob {
        shared,
        handle,
        #[cfg(not(any(feature = "gst", feature = "decode")))]
        stepped: None,
    }
}

/// Split out so [`start_export`] reads as the threading it is.
/// Media a clip places whose bytes cannot be read.
///
/// The compositor draws a representative fill for an unresolved source — right for a preview of
/// a reference still arriving, and wrong for an export, which would write a file of stand-ins
/// and call it finished. So the render asks first and refuses by name.
///
/// Titles are excluded, having no bytes to miss, and so is media sitting in the bin unplaced.
pub fn unreadable_sources(project: &Project) -> Vec<(String, video_edit::media::Hydration)> {
    let mut out: Vec<(String, video_edit::media::Hydration)> = Vec::new();
    let (start, end) = project.export_range();
    for item in &project.media {
        if item.kind.is_generated()
            || !project
                .clips
                .iter()
                .any(|c| c.media == item.id && c.start < end && c.end() > start)
        {
            continue;
        }
        let src = item.source();
        let kept = renders_by_steps()
            && (video_edit::files::kept_here(&item.path)
                || video_edit::files::kept_source(&item.path).is_some());
        if !src.is_decodable() && !kept && !out.iter().any(|(n, _)| *n == item.name) {
            out.push((item.name.clone(), src.hydration));
        }
    }
    out
}

/// The refusal an export gives when a source it needs cannot be read.
///
/// It separates the two halves the app already distinguishes elsewhere: a
/// reference whose bytes have not arrived is a different problem from a
/// path that resolves to nothing, and a message that blurred them would
/// send the user looking in the wrong place.
fn unreadable_reason(missing: &[(String, video_edit::media::Hydration)]) -> String {
    let names: Vec<&str> = missing.iter().map(|(n, _)| n.as_str()).collect();
    let all_projected = missing
        .iter()
        .all(|(_, h)| *h == video_edit::media::Hydration::Projected);
    let what = if all_projected {
        "is not on this device yet"
    } else {
        "cannot be read"
    };
    format!(
        "Cannot export: {} {what}. Exporting now would write stand-in frames \
         rather than the footage.",
        names.join(", "),
    )
}

fn render(
    project: &Project,
    settings: ExportSettings,
    destination: &std::path::Path,
    state: &JobState,
) -> Result<String, String> {
    // filesink truncates its destination as soon as it starts. Refuse an
    // input path (including a symlink to one) before opening the graph, or
    // exporting over imported footage would destroy the source before the
    // decoder could read it.
    if let Ok(output) = tilo_files::canonicalize(destination)
        && let Some(item) = project.media.iter().find(|item| {
            !item.kind.is_generated()
                && tilo_files::canonicalize(&item.source().abs_path)
                    .is_ok_and(|source| source == output)
        })
    {
        return Err(format!(
            "Cannot export over source media {}. Choose a different output file.",
            item.name,
        ));
    }
    let missing = unreadable_sources(project);
    if !missing.is_empty() {
        return Err(unreadable_reason(&missing));
    }
    #[cfg(feature = "gst")]
    {
        gst_pipeline::run(project, settings, destination, state)
    }
    #[cfg(not(feature = "gst"))]
    {
        let _ = (project, settings, destination, state);
        Err(unavailable_reason()
            .unwrap_or("Export is unavailable")
            .to_string())
    }
}

/// Why an export stopped at a frame `media` gave no picture for.
#[cfg_attr(
    not(feature = "gst"),
    allow(dead_code, reason = "only the GStreamer render composites frames")
)]
fn no_picture_reason(
    project: &Project,
    media: video_edit::model::MediaId,
    source_time: f64,
    waited: std::time::Duration,
) -> String {
    let name = project.media(media).map_or_else(
        || "a source".to_string(),
        |item| format!("\"{}\"", item.name),
    );
    format!(
        "Cannot export: {name} gave no picture at {source_time:.2}s within {}s. The file \
         would have held a stand-in frame there instead of the footage.",
        waited.as_secs()
    )
}

/// Why export cannot run in this build, or `None` when it can.
///
/// The dialog must ask this rather than carry its own copy: a `gst` build
/// exports for real, and telling the user encoding is "coming soon" beside
/// a working Export button is simply false.
pub fn unavailable_reason() -> Option<&'static str> {
    #[cfg(feature = "gst")]
    {
        None
    }
    #[cfg(not(feature = "gst"))]
    {
        // A browser page encodes with the browser's own encoder.
        (!renders_by_steps()).then_some(UNAVAILABLE)
    }
}

/// Whether this copy exports a step at a time with the browser's encoder
/// ([`ExportJob::stepped`]) rather than on a `GStreamer` worker thread.
pub fn renders_by_steps() -> bool {
    cfg!(not(any(feature = "gst", feature = "decode"))) && media::edit::ClipEncoder::available()
}

#[cfg(any(feature = "gst", not(feature = "decode")))]
fn audio_frame_at(i: u64, fps: f64) -> u64 {
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "frame and sample counts of a render are far inside f64's exact range, and positive"
    )]
    let at = (i as f64 / fps * f64::from(video_edit::audio::SAMPLE_RATE)).round() as u64;
    at
}

/// The sample rate is not a whole multiple of most frame rates, and deriving each frame's count
/// from the difference between two cumulative totals means the rounding error never
/// accumulates: an hour of audio still ends where the video does.
#[cfg(any(feature = "gst", not(feature = "decode")))]
fn audio_frames_for(i: u64, fps: f64) -> usize {
    let frames = audio_frame_at(i + 1, fps).saturating_sub(audio_frame_at(i, fps));
    usize::try_from(frames).unwrap_or(usize::MAX)
}

/// What a person reads when this copy has no video encoder, in their terms:
#[cfg(not(feature = "gst"))]
#[cfg(any(not(feature = "gst"), test))]
const UNAVAILABLE: &str = "Exporting video isn't available here yet";

/// With `--features gst` this runs a real pipeline — a raw-RGBA `appsrc` fed the composited
/// frames the preview shows — encoded and muxed to a real file. With no encoder linked it is
/// honest: an "unavailable" status, never a faked progress bar.
pub fn begin_export(project: &Project, settings: &ExportSettings) -> ExportStatus {
    start_export(project, settings, output_path(settings, project)).join()
}

/// A file a render was handed open, which its graph's sink writes: what a
/// device hands a render it already opened.
pub trait HandedOutput {
    /// How many bytes have been written so far, when that can be told.
    fn written(&self) -> Option<u64>;

    fn discard(&self);
}

#[cfg(feature = "gst")]
pub(crate) enum Output<'a> {
    Path(&'a std::path::Path),
    Handed(&'a dyn HandedOutput),
}

#[cfg(feature = "gst")]
impl Output<'_> {
    /// How much has been written so far.
    fn len(&self) -> Option<u64> {
        match self {
            Self::Path(path) => video_edit::files::metadata(path).ok().map(|m| m.len),
            Self::Handed(file) => file.written(),
        }
    }

    fn discard(&self) {
        match self {
            Self::Path(path) => video_edit::files::remove(path),
            Self::Handed(file) => file.discard(),
        }
    }
}

#[cfg(feature = "gst")]
pub(crate) struct Encode<'a> {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub desc: String,
    pub output: Output<'a>,
}

/// `report` is told each frame written and how many there are, and `cancel` stops it. Returns
/// how many frames were written.
///
/// # Errors
/// Why no file was written; the output is discarded then.
#[cfg(feature = "gst")]
pub(crate) fn encode_to(
    project: &Project,
    encode: &Encode<'_>,
    cancel: Arc<AtomicBool>,
    report: impl Fn(u64, u64) + Send + Sync + 'static,
) -> Result<u64, String> {
    let missing = unreadable_sources(project);
    if !missing.is_empty() {
        return Err(unreadable_reason(&missing));
    }
    let state = JobState {
        done: AtomicU64::new(0),
        total: AtomicU64::new(0),
        cancel,
        outcome: Mutex::new(None),
        frame_patience: EXPORT_FRAME_PATIENCE,
        report: Some(Box::new(report)),
    };
    gst_pipeline::encode_with(project, encode, &state)
}

#[cfg(feature = "gst")]
mod gst_pipeline {
    #[allow(
        clippy::wildcard_imports,
        reason = "the pipeline module is the guarded half of this file and shares its scope"
    )]
    use super::*;
    use std::sync::atomic::Ordering;

    use media::encode::graph::Graph;

    /// An **inactivity** timeout, not a total one: a long 4K mux takes minutes, and a flat bound
    /// deleted a render that had already succeeded. Progress is the muxer's own output growing
    /// plus any bus traffic.
    const QUIET_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

    /// Build + run the real export graph for a file a person chose: the
    /// preset's encoders, writing `destination`. Returns the written path.
    pub fn run(
        project: &Project,
        settings: ExportSettings,
        destination: &std::path::Path,
        state: &super::JobState,
    ) -> Result<String, String> {
        tilo_files::file_at(destination)
            .map_err(|e| format!("Cannot write to this output path: {e}"))?;
        let (w, h) = output_dims(&settings, project);
        let fps = settings.frame_rate.fps();
        let out = destination.to_path_buf();
        if let Some(parent) = out.parent() {
            video_edit::files::create_dir_all(parent);
        }
        let out_str = out.to_string_lossy().into_owned();

        let with_audio = video_edit::audio::has_audio(project);
        // State which encoders this machine actually resolved to, so a slow
        // export is diagnosable — "it fell back to openh264enc because no
        // vah264enc is installed" rather than a guess. The registry is read,
        // not assumed.
        tracing::info!(
            "export: {}",
            selected_encoders(settings.container).describe()
        );
        let desc = pipeline_desc_with_audio(&settings, w, h, &out_str, with_audio);
        let encode = Encode {
            width: w,
            height: h,
            fps,
            desc,
            output: Output::Path(&out),
        };
        encode_with(project, &encode, state)?;
        Ok(out_str)
    }

    /// Push every composited frame of the timeline into `encode`'s graph,
    /// encode and mux, and finish its output. Blocks until the graph has
    /// finished the file (or failed), then returns how many frames were
    /// written. Any failure is surfaced verbatim and discards the output —
    /// nothing claims success unless a real file was produced.
    pub fn encode_with(
        project: &Project,
        encode: &Encode<'_>,
        state: &super::JobState,
    ) -> Result<u64, String> {
        let out = &encode.output;
        let result = encode_frames(project, encode, state).and_then(|total| match out.len() {
            Some(len) if len > 0 => Ok(total),
            Some(_) => Err("export produced an empty file".to_string()),
            None => Err("export wrote no file".to_string()),
        });
        if result.is_err() {
            out.discard();
        }
        result
    }

    fn encode_frames(
        project: &Project,
        encode: &Encode<'_>,
        state: &super::JobState,
    ) -> Result<u64, String> {
        let (w, h, fps) = (encode.width, encode.height, encode.fps);
        let with_audio = video_edit::audio::has_audio(project);
        let out = &encode.output;
        let graph = Graph::start(&encode.desc, with_audio, fps)?;

        // The very same GPU compositor the preview draws through, fed by the same media
        // sessions, so the file cannot disagree with what was on screen.
        //
        // `Exact` because an export has no deadline to miss and every frame must be the right
        // one, and `strict` because a stand-in is never written into the file: a frame is
        // composited again until every source has given its real picture.
        let mut compositor =
            crate::gpu::HeadlessCompositor::new().map_err(|e| format!("export compositor: {e}"))?;
        let mut sources = crate::gpu::MediaFrameSource::new(project, crate::gpu::Fetch::Exact)
            .with_timeout(super::EXPORT_FRAME_ATTEMPT)
            .strict();
        let mut pcm = video_edit::audio::DecodedPcm::new(project);

        // What this render covers: the marked range, or the whole timeline
        // when nothing is marked. One place decides it (`RenderPlan`), so the
        // encode, the progress figure and the size estimate cannot disagree.
        let plan = super::RenderPlan::new(project, fps);
        let total = plan.total;
        state.total.store(total, Ordering::Relaxed);
        let frame_bytes = (w * h * 4) as usize;
        for i in 0..total {
            let t = plan.time_of(i);
            let asked = ui::time::now();
            let rgba = loop {
                // Cancellation is checked between attempts: the encode of one
                // frame is short, and stopping mid-buffer would leave the
                // muxer holding a partial sample.
                if state.cancel.load(Ordering::Relaxed) {
                    return Err(super::CANCELLED.to_string());
                }
                let rgba = compositor.frame(project, t, w, h, &mut sources);
                let Some(&(media, source_time)) = sources.missing_pictures().first() else {
                    break rgba;
                };
                if ui::time::now().saturating_duration_since(asked) >= state.frame_patience {
                    return Err(super::no_picture_reason(
                        project,
                        media,
                        source_time,
                        state.frame_patience,
                    ));
                }
            };
            debug_assert_eq!(rgba.len(), frame_bytes);
            let sound = with_audio.then(|| {
                let block =
                    video_edit::audio::mix_block(project, t, audio_frames_for(i, fps), &mut pcm);
                video_edit::audio::to_s16le(&block)
            });
            graph.push(i, &rgba, sound.as_deref())?;
            state.wrote(i + 1);
        }

        // Wait for the muxer to flush a complete file.
        let mut watch = FlushWatch::new(out.len().unwrap_or(0), ui::time::now());
        graph.finish(|activity| {
            let now = ui::time::now();
            if state.cancel.load(Ordering::Relaxed) {
                return Err(super::CANCELLED.to_string());
            }
            if activity {
                watch.observe_activity(now);
            }
            if let Some(len) = out.len() {
                watch.observe_len(len, now);
            }
            if watch.stalled(now, QUIET_LIMIT) {
                return Err(format!(
                    "the muxer stopped writing {}s ago; giving up on the export",
                    QUIET_LIMIT.as_secs(),
                ));
            }
            Ok(())
        })?;
        Ok(total)
    }
}
