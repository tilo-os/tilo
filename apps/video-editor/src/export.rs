//! Export settings + the (gated) render pipeline.
//!
//! Real encode/decode runs through `GStreamer`, linked by the default `gst`
//! feature and by the shipped image build. A deliberately minimal
//! `--no-default-features` developer build keeps the export UI testable without
//! `GStreamer` and reports that encoding is unavailable; it is not the product
//! configuration. Headless screenshots remain hermetic because they never
//! start an encoder, not because the shipped app replaces export with a stub.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use media::encode::{AudioCodec, EncodeReport, HwEncPolicy, Selection, VideoCodec};

use crate::model::Project;

/// Output resolution presets. Stored as the long edge; the project's
/// aspect is preserved at encode time.
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
    /// (width, height) for a 16:9 frame at this preset.
    pub fn dims(self) -> (u32, u32) {
        match self {
            Resolution::P720 => (1280, 720),
            Resolution::P1080 => (1920, 1080),
            Resolution::P2160 => (3840, 2160),
        }
    }
    /// Rough target video bitrate in megabits/sec — drives the size
    /// estimate shown in the dialog.
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
    /// What the encoder actually produces — not what the container is
    /// capable of. `WebM` here is VP8 and MOV is H.264, because that is what
    /// [`encode_chain`] plugs; naming VP9 or `ProRes` would be telling the
    /// user they are getting a codec this pipeline never writes.
    pub fn label(self) -> &'static str {
        match self {
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

/// The full export job description, as chosen in the dialog.
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
            container: Container::Mp4,
            frame_rate: FrameRate::F30,
        }
    }
}

impl ExportSettings {
    /// Estimated output size in megabytes for a `duration`-second project,
    /// muxing audio or not.
    ///
    /// This is the sum of what the two encoders are *asked* for:
    /// [`Resolution::target_mbps`] for the picture and [`AUDIO_BITRATE_BPS`]
    /// for the sound, both being the numbers [`pipeline_desc_with_audio`]
    /// writes into the graph — so the figure describes the encode that
    /// actually runs rather than a guess about it.
    ///
    /// `with_audio` is not a preference; it is whether this export will mux an
    /// audio track at all ([`crate::audio::has_audio`]). Quoting the audio
    /// allowance for a video-only timeline would bill for a track the file
    /// never carries — which is what this estimate used to do, charging
    /// 0.256 Mbit/s for stereo back when the pipeline had no audio branch to
    /// encode it with.
    ///
    /// How closely the file lands on the total is the encoders' business, and
    /// they differ. [`Self::estimate_label`] is what a dialog should show: it
    /// declines to quote a size at all where the selected encoder is known not
    /// to track the rate it is given.
    pub fn estimated_mb(&self, duration: f64, with_audio: bool) -> f64 {
        let audio_mbps = if with_audio {
            AUDIO_BITRATE_BPS as f64 / 1_000_000.0
        } else {
            0.0
        };
        (self.resolution.target_mbps() + audio_mbps) * duration / 8.0
    }

    /// The size line for the export dialog: either an approximate megabyte
    /// figure, or — where one cannot honestly be given — the rate that was
    /// requested and a plain statement that the size does not follow from it.
    ///
    /// The distinction is [`media::encode::Selection::bitrate_is_predictive`],
    /// false for an encoder measured to ignore its bitrate and for a build
    /// that could not confirm any encoder at all. Which of those a machine
    /// lands in depends on what it has installed: `x264enc` and the hardware
    /// encoders carry no such measurement against them, while `openh264enc`
    /// asked for 1 Mbit/s of noise writes 59 and `vp8enc` writes 14, and
    /// eight times the request moves either by nothing worth counting. A box
    /// with only those two — which is the fallback case, and this one — gets
    /// no megabyte figure, because there it would be a guess dressed as a
    /// measurement.
    pub fn estimate_label(&self, project: &Project) -> String {
        let (video, _, _) = encode_chain(self.container);
        self.estimate_label_for(project, &video)
    }

    /// [`Self::estimate_label`] against a given encoder choice rather than
    /// this machine's.
    ///
    /// Split out so both branches can be exercised wherever the tests run: a
    /// box with no hardware encoder resolves every container to an encoder
    /// that ignores its bitrate, so the megabyte branch would otherwise go
    /// unexecuted exactly where it most needs checking.
    pub fn estimate_label_for(
        &self,
        project: &Project,
        video: &media::encode::Selection,
    ) -> String {
        let with_audio = crate::audio::has_audio(project);
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

/// The audio codec each container carries. MP4 and MOV take AAC; `WebM` takes
/// Opus, which is its companion codec the way VP8 is its video one.
pub fn audio_codec(container: Container) -> AudioCodec {
    match container {
        Container::Mp4 | Container::Mov => AudioCodec::Aac,
        Container::WebM => AudioCodec::Opus,
    }
}

/// The parser + muxer for a container. Fixed by the container format, so
/// unlike the encoder these do not vary with the machine.
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

/// The `GStreamer` element chain that encodes the export, per container: the
/// (video encoder, optional parser, muxer) triple.
///
/// The encoder is **chosen against the machine's real `GStreamer` registry**,
/// not hard-coded. For H.264 that means a VA-API, NVENC or QSV encoder when
/// one is installed — covering AMD, Intel and NVIDIA, integrated and discrete
/// — then `x264enc`, then `openh264enc`, which is the baseline this workspace
/// can always count on but also the weakest (Baseline profile only). The
/// returned [`Selection`] carries which of those it is, and reports hardware
/// only when the registry actually confirmed one.
///
/// The muxer half stays fixed by the container:
///   * MP4 / MOV — `… ! h264parse ! (mp4mux|qtmux)`
///   * `WebM` — `… ! webmmux`
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

/// The rate the export asks its audio encoder for, in bits per second.
///
/// 128 kbit/s is the ordinary transparent-enough setting for stereo AAC and
/// comfortably so for Opus, and it is set explicitly rather than left to each
/// encoder's default — those disagree (`opusenc` starts at 64 kbit/s,
/// `avenc_aac` at 128, `fdkaacenc` at "auto"), so the same project would land
/// at a different rate depending on which encoder the machine happened to
/// have.
///
/// It is also what [`ExportSettings::estimated_mb`] charges for the audio
/// track, which is only meaningful because it is the number written into the
/// pipeline rather than an assumption about it.
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
        ch = crate::audio::CHANNELS,
        rate = crate::audio::SAMPLE_RATE,
        enc = enc.spec.element,
        bitrate = enc.spec.bitrate_arg(AUDIO_BITRATE_BPS),
    ))
}

/// Whether the muxer is still writing the file out.
///
/// The flush after the last frame is real work — on a long timeline the
/// muxer has an index to write — and it used to be bounded by a flat two
/// minutes from when the flush began. That is a duration a big export can
/// legitimately exceed, and exceeding it deleted the file, throwing away a
/// render that had taken minutes. What actually distinguishes a wedged
/// pipeline from a slow one is whether anything is *happening*, so this
/// watches for the output growing and gives up only on silence.
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

    /// Record the output's current length. Returns whether it had grown,
    /// which is also what resets the clock.
    fn observe_len(&mut self, len: u64, now: ui::time::Instant) -> bool {
        if len == self.last_len {
            return false;
        }
        self.last_len = len;
        self.last_progress = now;
        true
    }

    /// Anything on the pipeline's bus is the pipeline being alive.
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

/// How many frames a render writes, and where on the timeline it starts.
///
/// Split out of the encode loop so what gets rendered can be asserted without
/// an encoder: the loop reads its frame count and every frame's timeline time
/// from here and decides neither itself. Before the timeline had in / out
/// marks the loop ran `0..total` unconditionally, so checking a ten-second
/// fix meant re-rendering the whole project.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderPlan {
    /// Timeline seconds of the render's first frame.
    pub start: f64,
    /// Frames the render writes.
    pub total: u64,
    fps: f64,
}

impl RenderPlan {
    /// The plan for `project` at `fps`, honouring its in / out marks.
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

/// A render running on a worker thread.
///
/// Export used to run inline on the UI thread — the whole encode plus a
/// two-minute bus wait — so the window was frozen for the duration and
/// `ExportStatus::Rendering` was constructed nowhere, leaving the dialog's
/// progress bar as dead code. A job owns the thread, publishes progress as
/// it encodes, and can be told to stop.
pub struct ExportJob {
    shared: Arc<JobState>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// What the worker publishes and the UI reads. Small enough to poll every
/// frame without ceremony.
struct JobState {
    /// Frames written so far, and how many there are in total.
    done: AtomicU64,
    total: AtomicU64,
    cancel: Arc<AtomicBool>,
    outcome: Mutex<Option<ExportStatus>>,
    /// Told each frame written, and how many there are.
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
    /// Where the render has got to. `Rendering { progress }` while the
    /// worker is encoding, then whatever it finished as.
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

    /// Whether the worker is still going.
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

    /// Block until the worker finishes, returning its outcome. Mostly for
    /// tests; the UI polls [`Self::status`] instead.
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
        // A dropped handle must not leave an encoder running against a file
        // nobody is waiting for.
        self.shared.cancel.store(true, Ordering::Relaxed);
    }
}

impl JobState {
    /// `done` frames are written.
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

/// Render `project` to `destination` on the calling thread, for a caller
/// that runs its own job: `report` is told each frame written and how many
/// there are, and setting `cancel` stops the render as [`ExportJob::cancel`]
/// does.
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

/// The message a cancelled render reports.
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
    ExportJob { shared, handle }
}

/// Run the encode. Split out so [`start_export`] reads as the threading it is.
/// Media a clip places on the timeline whose bytes cannot be read.
///
/// The compositor treats an unresolved source as normal and draws the
/// layer's representative fill — right for a preview of a reference whose
/// bytes are still arriving, and wrong for an export, which would write a
/// file of stand-ins and call it finished. So the render asks this first
/// and refuses by name.
///
/// Titles are excluded: a title's picture is drawn from the document, so it
/// has no bytes to be missing. Media sitting in the bin but not placed on
/// any clip is excluded too — it is not in the file either way.
pub fn unreadable_sources(project: &Project) -> Vec<(String, crate::media::Hydration)> {
    let mut out: Vec<(String, crate::media::Hydration)> = Vec::new();
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
        if !src.is_decodable() && !out.iter().any(|(n, _)| *n == item.name) {
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
fn unreadable_reason(missing: &[(String, crate::media::Hydration)]) -> String {
    let names: Vec<&str> = missing.iter().map(|(n, _)| n.as_str()).collect();
    let all_projected = missing
        .iter()
        .all(|(_, h)| *h == crate::media::Hydration::Projected);
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
    media: crate::model::MediaId,
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
        Some(UNAVAILABLE)
    }
}

/// What a person reads when this copy has no video encoder, in their terms:
/// where they are and what they cannot do, never how the app was built.
#[cfg(any(not(feature = "gst"), test))]
const UNAVAILABLE: &str = "Exporting video isn't available here yet";

/// Kick off an export. Returns the status the dialog should show next.
///
/// With `--features gst` this runs a real `GStreamer` pipeline — a raw-RGBA
/// `appsrc` fed the composited timeline frames (the same grade/fade math the
/// preview shows), encoded and muxed to a real file on disk. The default build
/// has no encoder linked, so it is honest — an "unavailable" status, never a
/// faked progress bar or a success for a file that was never written.
pub fn begin_export(project: &Project, settings: &ExportSettings) -> ExportStatus {
    start_export(project, settings, output_path(settings, project)).join()
}

/// A file a render was handed open, which its graph's sink writes: the
/// agent host's render helper's (`tilo-video-render`), which may open
/// nothing once confined.
pub trait HandedOutput {
    /// How many bytes have been written so far, when that can be told.
    fn written(&self) -> Option<u64>;

    /// Throw away what was written.
    fn discard(&self);
}

/// Where a render's file goes: a path it creates, or a file it was handed
/// open.
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
            Self::Path(path) => crate::files::metadata(path).ok().map(|m| m.len),
            Self::Handed(file) => file.written(),
        }
    }

    /// Throw away what a failed render wrote, so nothing half-written looks
    /// like a render.
    fn discard(&self) {
        match self {
            Self::Path(path) => crate::files::remove(path),
            Self::Handed(file) => file.discard(),
        }
    }
}

/// One encode: the frame size and rate the timeline is composited at, and
/// the graph (its sink writing [`Output`]) the frames are pushed into.
#[cfg(feature = "gst")]
pub(crate) struct Encode<'a> {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub desc: String,
    pub output: Output<'a>,
}

/// Run `encode` over `project` on the calling thread, for a caller with a
/// graph of its own (an agent's render, [`crate::agent_render::render_video`]): `report` is told
/// each frame written and how many there are, and `cancel` stops it.
/// Returns how many frames were written.
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
        reason = "the pipeline module is the gated half of this file and shares its scope"
    )]
    use super::*;
    use std::sync::atomic::Ordering;

    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;

    /// All exits must stop the encoder and remove an incomplete container,
    /// including allocation, GPU setup and push failures.
    struct OutputGuard<'a> {
        pipeline: gst::Pipeline,
        output: &'a Output<'a>,
        keep: bool,
    }

    impl Drop for OutputGuard<'_> {
        fn drop(&mut self) {
            let _ = self.pipeline.set_state(gst::State::Null);
            if !self.keep {
                self.output.discard();
            }
        }
    }

    /// Cumulative boundaries avoid truncating 1/30 or 1/60 second once per
    /// frame, which otherwise shortens the file and drifts against the PCM.
    pub(super) fn frame_time(i: u64, fps: f64) -> gst::ClockTime {
        gst::ClockTime::from_nseconds((i as f64 * 1_000_000_000.0 / fps).round() as u64)
    }

    /// How many audio frames belong to video frame `i`.
    ///
    /// The sample rate is not a whole multiple of most frame rates — 48000 at
    /// 30fps is 1600 exactly, but at 24fps it is 2000 and at 29.97 it is not
    /// an integer at all. Deriving each frame's count from the difference
    /// between two cumulative totals means the rounding error never
    /// accumulates, so an hour of audio still ends where the video does
    /// instead of drifting seconds out of sync.
    fn audio_frames_for(i: u64, fps: f64) -> usize {
        let at = |n: u64| (n as f64 / fps * f64::from(crate::audio::SAMPLE_RATE)).round() as u64;
        (at(i + 1) - at(i)) as usize
    }

    /// How long the flush may go without progress before the export gives up.
    ///
    /// An **inactivity** timeout, not a total one. This used to be a flat
    /// two minutes from the start of the flush, which is a duration a long
    /// 4K mux can legitimately exceed — and exceeding it deleted the file,
    /// throwing away a render that had taken minutes. "Nothing has
    /// happened for two minutes" is a wedged pipeline; "this has taken two
    /// minutes" is just a big timeline.
    ///
    /// Progress is the muxer's own output growing, which is the direct
    /// evidence it is still working, plus any bus traffic. Both reset the
    /// clock.
    const QUIET_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

    /// Build + run the real export graph for a file a person chose: the
    /// preset's encoders, writing `destination`. Returns the written path.
    pub fn run(
        project: &Project,
        settings: ExportSettings,
        destination: &std::path::Path,
        state: &super::JobState,
    ) -> Result<String, String> {
        gst::init().map_err(|e| format!("GStreamer init failed: {e}"))?;

        tilo_files::file_at(destination)
            .map_err(|e| format!("Cannot write to this output path: {e}"))?;
        let (w, h) = output_dims(&settings, project);
        let fps = settings.frame_rate.fps();
        let out = destination.to_path_buf();
        if let Some(parent) = out.parent() {
            crate::files::create_dir_all(parent);
        }
        let out_str = out.to_string_lossy().into_owned();

        // Only ask for an audio branch when there is something to put in it:
        // a muxer left holding an empty pad never finishes the file.
        let with_audio = crate::audio::has_audio(project);
        // State which encoders this machine actually resolved to, so a slow
        // export is diagnosable — "it fell back to openh264enc because no
        // vah264enc is installed" rather than a guess. `gst::init` above has
        // populated the registry, so this is read, not assumed.
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

    /// Push every composited frame of the timeline into the `appsrc` of
    /// `encode`'s graph, encode and mux, and finish its output. Blocks until
    /// the pipeline reaches EOS (or errors), then returns how many frames
    /// were written. Any failure is surfaced verbatim and discards the
    /// output — nothing claims success unless a real file was produced.
    #[allow(
        clippy::too_many_lines,
        reason = "one export pipeline: probe, encode, mux and finish in order, sharing cleanup on every exit"
    )]
    pub fn encode_with(
        project: &Project,
        encode: &Encode<'_>,
        state: &super::JobState,
    ) -> Result<u64, String> {
        gst::init().map_err(|e| format!("GStreamer init failed: {e}"))?;
        let (w, h, fps) = (encode.width, encode.height, encode.fps);
        let with_audio = crate::audio::has_audio(project);
        let out = &encode.output;
        let pipeline = gst::parse::launch(&encode.desc)
            .map_err(|e| format!("build export pipeline: {e}"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "export pipeline is not a Pipeline".to_string())?;
        let mut output = OutputGuard {
            pipeline: pipeline.clone(),
            output: out,
            keep: false,
        };

        let appsrc = pipeline
            .by_name("tilo_src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .ok_or_else(|| "export appsrc not found".to_string())?;
        appsrc.set_format(gst::Format::Time);

        let audio_src = if with_audio {
            match pipeline
                .by_name("tilo_audio")
                .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            {
                Some(a) => {
                    a.set_format(gst::Format::Time);
                    Some(a)
                }
                None => return Err("export audio appsrc not found".to_string()),
            }
        } else {
            None
        };

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("start export pipeline: {e}"))?;

        // The frames come off the very same GPU compositor the preview
        // canvas draws through — layer stack, grade, effect chain,
        // envelope and transitions — fed by the same media sessions, so
        // the file cannot disagree with what was on screen.
        //
        // `Exact` because an export has no interactive deadline to miss
        // and every frame must be the right one; the preview asks the
        // same sources for `Responsive` instead. `strict` because the
        // preview's stand-ins (the representative fill, the last picture a
        // source produced) are never written into the file: a frame is
        // composited again until every source has given its real picture,
        // and the export fails, naming the source, if one has not within
        // the frame's patience.
        let mut compositor =
            crate::gpu::HeadlessCompositor::new().map_err(|e| format!("export compositor: {e}"))?;
        let mut sources = crate::gpu::MediaFrameSource::new(project, crate::gpu::Fetch::Exact)
            .with_timeout(super::EXPORT_FRAME_ATTEMPT)
            .strict();
        let mut pcm = crate::audio::DecodedPcm::new(project);

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
                    let _ = pipeline.set_state(gst::State::Null);
                    // A half-written file must not be left looking like a render.
                    out.discard();
                    return Err(super::CANCELLED.to_string());
                }
                let rgba = compositor.frame(project, t, w, h, &mut sources);
                let Some(&(media, source_time)) = sources.missing_pictures().first() else {
                    break rgba;
                };
                if ui::time::now().saturating_duration_since(asked) >= state.frame_patience {
                    let _ = pipeline.set_state(gst::State::Null);
                    out.discard();
                    return Err(super::no_picture_reason(
                        project,
                        media,
                        source_time,
                        state.frame_patience,
                    ));
                }
            };
            let pts = frame_time(i, fps);
            let frame_dur = frame_time(i + 1, fps) - pts;
            debug_assert_eq!(rgba.len(), frame_bytes);
            let mut buffer = gst::Buffer::with_size(frame_bytes)
                .map_err(|_| "allocate export frame buffer".to_string())?;
            {
                #[expect(
                    clippy::expect_used,
                    reason = "a buffer allocated on the line above has no other owner"
                )]
                let bref = buffer.get_mut().expect("fresh buffer is uniquely owned");
                bref.set_pts(pts);
                bref.set_duration(frame_dur);
                let mut map = bref
                    .map_writable()
                    .map_err(|_| "map export frame buffer".to_string())?;
                map.copy_from_slice(&rgba);
            }
            if let Err(e) = appsrc.push_buffer(buffer) {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(format!("push export frame {i}: {e:?}"));
            }
            // One frame's worth of mixed audio, pushed alongside the picture
            // so the two streams stay in step without a separate clock.
            if let Some(audio) = &audio_src {
                let frames = audio_frames_for(i, fps);
                let block = crate::audio::mix_block(project, t, frames, &mut pcm);
                let bytes = crate::audio::to_s16le(&block);
                let mut buffer = gst::Buffer::with_size(bytes.len())
                    .map_err(|_| "allocate export audio buffer".to_string())?;
                {
                    #[expect(
                        clippy::expect_used,
                        reason = "a buffer allocated on the line above has no other owner"
                    )]
                    let bref = buffer.get_mut().expect("fresh buffer is uniquely owned");
                    bref.set_pts(pts);
                    bref.set_duration(frame_dur);
                    let mut map = bref
                        .map_writable()
                        .map_err(|_| "map export audio buffer".to_string())?;
                    map.copy_from_slice(&bytes);
                }
                if let Err(e) = audio.push_buffer(buffer) {
                    let _ = pipeline.set_state(gst::State::Null);
                    return Err(format!("push export audio {i}: {e:?}"));
                }
            }
            state.wrote(i + 1);
        }
        appsrc
            .end_of_stream()
            .map_err(|e| format!("end export stream: {e:?}"))?;
        if let Some(audio) = &audio_src {
            audio
                .end_of_stream()
                .map_err(|e| format!("end export audio stream: {e:?}"))?;
        }

        // Wait for EOS (or an error) so the muxer flushes a complete file.
        let bus = pipeline
            .bus()
            .ok_or_else(|| "export pipeline bus".to_string())?;
        let mut result = Ok(());
        let mut watch = FlushWatch::new(out.len().unwrap_or(0), ui::time::now());
        loop {
            use gst::MessageView;
            if state.cancel.load(Ordering::Relaxed) {
                result = Err(super::CANCELLED.to_string());
                break;
            }
            if let Some(len) = out.len() {
                watch.observe_len(len, ui::time::now());
            }
            if watch.stalled(ui::time::now(), QUIET_LIMIT) {
                result = Err(format!(
                    "the muxer stopped writing {}s ago; giving up on the export",
                    QUIET_LIMIT.as_secs(),
                ));
                break;
            }
            // Short waits rather than one long one, so a cancel arriving
            // while the muxer flushes is still noticed promptly.
            let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(200)) else {
                continue;
            };
            watch.observe_activity(ui::time::now());
            match msg.view() {
                MessageView::Eos(_) => break,
                MessageView::Error(e) => {
                    result = Err(format!(
                        "export pipeline error: {} ({})",
                        e.error(),
                        e.debug().unwrap_or_default()
                    ));
                    break;
                }
                _ => {}
            }
        }
        let _ = pipeline.set_state(gst::State::Null);
        if result.is_err() {
            out.discard();
        }
        result?;

        // Never report success unless a real, non-empty file exists.
        match out.len() {
            Some(len) if len > 0 => {
                output.keep = true;
                Ok(total)
            }
            Some(_) => Err("export produced an empty file".to_string()),
            None => Err("export wrote no file".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "gst")]
    mod generated_clip {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/generated_clip.rs"
        ));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn render_jobs_report_progress_or_failure_without_hanging() {
        // Case `a_muxer_still_writing_is_not_given_up_on`:
        // A muxer that is still writing is never given up on, however long it
        // takes; one that has gone quiet is.
        //
        // The old rule was a flat two minutes from the start of the flush, and
        // it deleted the file on expiry — so a long export whose mux genuinely
        // took longer lost a render that had already succeeded. What separates a
        // wedged pipeline from a slow one is whether anything is happening.
        {
            use ui::time::Duration;

            let limit = Duration::from_secs(120);
            let t0 = ui::time::now();
            let mut w = FlushWatch::new(0, t0);

            // Four minutes of steady writing — twice the old cap — never stalls.
            let mut now = t0;
            for step in 1..=4u64 {
                now += Duration::from_secs(60);
                assert!(
                    w.observe_len(step * 4_000_000, now),
                    "a growing file is progress",
                );
                assert!(
                    !w.stalled(now, limit),
                    "a muxer that wrote {}s in is not stalled",
                    step * 60,
                );
            }

            // Then it stops. Short of the limit it is still given the benefit of
            // the doubt; past it, it is not.
            let quiet = now;
            assert!(!w.observe_len(16_000_000, quiet + Duration::from_secs(60)));
            assert!(!w.stalled(quiet + Duration::from_secs(119), limit));
            assert!(w.stalled(quiet + Duration::from_secs(121), limit));

            // Bus traffic counts as being alive even with the file unchanged —
            // a muxer can work for a while before its next write lands.
            w.observe_activity(quiet + Duration::from_secs(121));
            assert!(!w.stalled(quiet + Duration::from_secs(122), limit));
        }
        {
            // With no encoder linked the job still resolves, promptly, with the
            // same reason the dialog shows.
            let p = Project::new("t");
            let job = start_export(
                &p,
                &ExportSettings::default(),
                std::env::temp_dir().join("tilo-ve-never.mp4"),
            );
            let status = job.join();
            match (unavailable_reason(), &status) {
                (Some(reason), ExportStatus::Failed { reason: got }) => assert_eq!(reason, got),
                (None, s) => assert!(
                    !matches!(s, ExportStatus::Failed { reason } if reason == UNAVAILABLE),
                    "a gst build must not claim export is unavailable, got {s:?}",
                ),
                (Some(_), other) => panic!("expected the unavailable status, got {other:?}"),
            }
        }
        {
            // The bug: a relative media path resolved to nothing, every layer
            // fell back to its representative fill, and the export wrote a
            // valid MP4 of a flat gradient and reported Done. A stand-in frame
            // is right for a preview and never right for a file.
            let p = one_clip_over("no/such/directory/definitely-not-here-42.mp4");
            let missing = unreadable_sources(&p);
            assert_eq!(
                missing.len(),
                1,
                "the one unreadable source is named: {missing:?}"
            );
            assert_eq!(missing[0].0, "clip");

            let out =
                std::env::temp_dir().join(format!("tilo-ve-missing-{}.mp4", std::process::id()));
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_VE_EXPORT_OUT", &out);
            let status = begin_export(&p, &ExportSettings::default());
            drop(env);

            match status {
                ExportStatus::Failed { reason } => {
                    assert!(
                        reason.contains("clip"),
                        "the refusal names the source: {reason:?}"
                    );
                    assert!(
                        reason.contains("stand-in"),
                        "and says what it would otherwise have written: {reason:?}",
                    );
                }
                other => panic!("an unreadable source must refuse, got {other:?}"),
            }
            assert!(
                !out.exists(),
                "and no file is left behind looking like a render"
            );
        }
        {
            // The refusal must not catch a project that legitimately has no
            // bytes to read: a title's picture comes from the document.
            use crate::model::{MediaItem, MediaKind, TextSource, Track, TrackKind};
            use uuid::Uuid;
            let mut p = Project::new("title only");
            let mid = Uuid::new_v4();
            p.media.push(MediaItem {
                id: mid,
                name: "Title".into(),
                kind: MediaKind::Text,
                duration: 2.0,
                text: Some(TextSource::new("HELLO")),
                ..Default::default()
            });
            let tid = Uuid::new_v4();
            p.tracks.push(Track {
                id: tid,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                ..Default::default()
            });
            p.add_clip(tid, mid, 0.0).expect("media exists");
            assert!(
                unreadable_sources(&p).is_empty(),
                "a title has no bytes to be missing"
            );
        }
        {
            // Importing a file and then not using it must not make the whole
            // project unexportable — it is not in the render either way.
            use crate::model::{MediaItem, MediaKind};
            use uuid::Uuid;
            let mut p = one_clip_over("no/such/definitely-not-here-42.mp4");
            p.clips.clear();
            p.media.push(MediaItem {
                id: Uuid::new_v4(),
                name: "unused".into(),
                kind: MediaKind::Video,
                path: "also/not/here.mp4".into(),
                ..Default::default()
            });
            assert!(
                unreadable_sources(&p).is_empty(),
                "nothing placed on the timeline, so nothing is missing from the render",
            );
        }
    }

    use super::*;

    /// A source whose bytes resolve but never decode makes the export fail,
    /// naming it, and leaves no file: an export never writes a stand-in in
    /// place of a frame it could not get.
    #[test]
    #[cfg(feature = "gst")]
    fn a_source_that_never_delivers_fails_the_export_instead_of_writing_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("never-decodes.mp4");
        std::fs::write(&clip, vec![0x5a_u8; 64 * 1024]).unwrap();
        let p = one_clip_over(&clip.to_string_lossy());
        assert!(
            unreadable_sources(&p).is_empty(),
            "its bytes are there, so only decoding can tell it gives nothing",
        );
        let out = dir.path().join("render.mp4");
        let status = start_export_with_patience(
            &p,
            ExportSettings::default(),
            out.clone(),
            std::time::Duration::from_secs(5),
        )
        .join();
        match status {
            ExportStatus::Failed { reason } => {
                assert!(reason.contains("\"clip\""), "names the source: {reason:?}");
                assert!(
                    reason.contains("gave no picture"),
                    "and says why: {reason:?}"
                );
            }
            other => panic!("a source that never delivers must fail the export, got {other:?}"),
        }
        assert!(!out.exists(), "and no file is left behind");
    }

    #[test]
    fn exporting_over_source_media_is_refused_without_changing_its_bytes() {
        let dir = tempfile::tempdir().expect("source fixture directory");
        let source = dir.path().join("source.mp4");
        let original = b"source bytes must survive even a failed export";
        std::fs::write(&source, original).expect("write source fixture");
        let project = one_clip_over(source.to_str().expect("UTF-8 fixture path"));
        let status = start_export(&project, &ExportSettings::default(), source.clone()).join();
        assert!(
            matches!(status, ExportStatus::Failed { reason } if reason.contains("source media"))
        );
        assert_eq!(std::fs::read(&source).expect("source survives"), original);
    }

    #[test]
    fn cut_render_plans_do_not_pad_short_clips_or_add_roundoff_frames() {
        let mut project = Project::new("short cut");
        project.clips.push(crate::model::Clip {
            in_point: 0.5,
            out_point: 0.8,
            ..Default::default()
        });
        assert!(
            (project.duration() - 1.0).abs() < f64::EPSILON,
            "the ruler keeps its UI minimum"
        );
        let plan = RenderPlan::new(&project, 30.0);
        assert_eq!(plan.total, 9, "a nine-frame cut stays nine frames");
        assert!(plan.time_of(8) < project.clips[0].end());

        project.clips[0].out_point += 0.001;
        assert_eq!(RenderPlan::new(&project, 30.0).total, 10);
        project.set_mark_in(0.1);
        project.set_mark_out(0.2);
        let plan = RenderPlan::new(&project, 30.0);
        assert_eq!(plan.total, 3);
        assert!((plan.time_of(0) - 0.1).abs() < f64::EPSILON);

        project.clear_marks();
        project.clips[0].in_point = 3600.1;
        project.clips[0].out_point = 3600.4;
        assert_eq!(RenderPlan::new(&project, 30.0).total, 9);
        project.clips[0].start = 3600.0;
        project.set_mark_in(3600.1);
        project.set_mark_out(3600.2);
        assert_eq!(RenderPlan::new(&project, 30.0).total, 3);
    }

    #[test]
    #[cfg(feature = "gst")]
    fn export_frame_boundaries_do_not_accumulate_timestamp_roundoff() {
        for fps in [24, 30, 60] {
            assert_eq!(
                gst_pipeline::frame_time(3600 * fps, fps as f64),
                gstreamer::ClockTime::from_seconds(3600),
            );
        }
    }

    #[test]
    #[cfg(feature = "gst")]
    #[serial_test::serial(video_editor_gst)]
    fn real_renders_report_progress_cancel_and_mux_audio() {
        // Case `a_render_reports_progress_and_finishes_where_it_was_told`:
        // The progress bar used to be dead code: nothing ever constructed
        // `Rendering`, so the dialog showed a bar that could not move.
        'case: {
            if !generated_clip::media_stack() {
                break 'case;
            }
            let clip = demo_clip();
            let project = project_over(&clip, 2.0, 1.0);
            let out = std::env::temp_dir().join(format!("tilo-ve-job-{}.mp4", std::process::id()));
            std::fs::remove_file(&out).ok();

            let job = start_export(&project, &ExportSettings::default(), out.clone());
            // The job is a handle, not a blocking call: this thread is free while
            // the encode runs, which is the whole point of moving it off the UI.
            let mut seen_rendering = false;
            let mut last = 0.0;
            let deadline = tilo_test_env::hang_deadline();
            while job.is_running() && ui::time::now() < deadline {
                if let ExportStatus::Rendering { progress } = job.status() {
                    seen_rendering = true;
                    assert!(
                        (0.0..=1.0).contains(&progress),
                        "progress stays in range, got {progress}",
                    );
                    assert!(progress >= last - 1e-9, "progress never goes backwards");
                    last = progress;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(
                seen_rendering,
                "a running render reports Rendering, not a frozen dialog"
            );

            match job.join() {
                ExportStatus::Done { path } => {
                    assert_eq!(path, out.to_string_lossy(), "it wrote where it was told to");
                    assert!(std::fs::metadata(&out).expect("the file exists").len() > 512);
                }
                other => panic!("the render should have finished, got {other:?}"),
            }
            std::fs::remove_file(&out).ok();
        }
        // Case `cancelling_a_render_stops_it_and_leaves_no_file`:
        // Cancel has to actually stop the encode and not leave a stub file
        // sitting there looking like a finished render.
        'case: {
            if !generated_clip::media_stack() {
                break 'case;
            }
            let clip = demo_clip();
            // Long enough that the cancel lands mid-encode rather than after it:
            // twenty seconds of timeline, the whole source played five times.
            let mut project = project_over(&clip, 0.0, generated_clip::DURATION_SECS);
            let (track, media) = (project.tracks[0].id, project.media[0].id);
            for n in 1..5 {
                let start = f64::from(n) * generated_clip::DURATION_SECS;
                project.add_clip(track, media, start).unwrap();
            }
            let out =
                std::env::temp_dir().join(format!("tilo-ve-cancel-{}.mp4", std::process::id()));
            std::fs::remove_file(&out).ok();

            let job = start_export(
                &project,
                &ExportSettings {
                    resolution: Resolution::P720,
                    ..Default::default()
                },
                out.clone(),
            );
            tilo_test_env::wait_until("the render writes its first frame", || {
                !job.is_running()
                    || matches!(job.status(), ExportStatus::Rendering { progress } if progress > 0.0)
            });
            job.cancel();
            let state = Arc::clone(&job.shared);

            match job.join() {
                ExportStatus::Failed { reason } => {
                    assert_eq!(reason, CANCELLED, "a cancelled render says so plainly");
                }
                other => panic!("a cancelled render must not report {other:?}"),
            }
            assert!(
                !out.exists(),
                "a cancelled render leaves no half-written file behind",
            );
            // And it stopped between frames rather than encoding the whole
            // 20 seconds of timeline first.
            let (done, total) = (
                state.done.load(Ordering::Relaxed),
                state.total.load(Ordering::Relaxed),
            );
            assert!(
                done < total,
                "cancel was noticed only after all {total} frames: it is not checked between frames",
            );
        }
        // Case `the_export_muxes_the_timelines_audio`:
        // Audio was discarded entirely: the graph had one `appsrc` and no audio
        // pad, so every audio track in a project went missing without a word.
        // Prove a real stream now reaches the file, and that what is in it is
        // the mixed timeline rather than silence.
        'case: {
            if !generated_clip::media_stack() {
                break 'case;
            }
            let clip = demo_clip();
            let pid = std::process::id();
            let tone = std::env::temp_dir().join(format!("tilo-ve-tone-{pid}.wav"));
            write_tone(&tone, 1.5);
            assert!(
                audio_peak(&tone) > 0.5,
                "the tone itself is loud, so silence means a bug"
            );

            let p = project_with_tone(&clip, &tone, 1.5, false);
            assert!(crate::audio::has_audio(&p), "the project has an audio lane");

            let out = std::env::temp_dir().join(format!("tilo-ve-audio-{pid}.mp4"));
            std::fs::remove_file(&out).ok();
            match start_export(&p, &ExportSettings::default(), out.clone()).join() {
                ExportStatus::Done { .. } => {}
                other => panic!("the export must write a file, got {other:?}"),
            }

            // Decode the audio back out of what was written. A container check
            // cannot tell a real stream from a missing one.
            let pcm = decoded_audio(&out);
            assert!(
                pcm.len() > crate::audio::SAMPLE_RATE as usize,
                "at least half a second of audio was muxed, got {} samples",
                pcm.len(),
            );
            let peak = crate::audio::peak(&pcm);
            assert!(
                peak > 0.05,
                "the muxed audio is the timeline's, not silence (peak {peak})",
            );
            std::fs::remove_file(&out).ok();
            std::fs::remove_file(&tone).ok();
        }
        // Case `a_file_with_no_audio_track_cannot_be_asked_for_its_level`:
        // The peak helper has to be able to say "I could not read this". While it
        // answered `0.0` for an unreadable file, every level assertion below was
        // one decode hiccup away from reporting a working export as a silent one
        // -- which is precisely how this file's audio tests flaked under load,
        // and the failure then named the wrong thing as broken.
        //
        // A deliberately video-only encoded fixture has no level to read, and
        // asking for one has to fail rather than come back with a number.
        'case: {
            if !generated_clip::media_stack() {
                break 'case;
            }
            let clip = generated_clip::encoded_video_only();
            let refused = std::panic::catch_unwind(|| audio_peak(&clip))
                .expect_err("a video-only file has no level to report");
            let message = refused
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| refused.downcast_ref::<&str>().copied())
                .unwrap_or_default();
            assert!(message.contains("no readable audio stream"), "{message}");
        }
        // Case `volume_and_mute_reach_the_encoded_audio`:
        // Per-clip volume and track mute exist in the CRDT and the inspector but
        // reached no audio path at all. Prove both now change the encoded file.
        'case: {
            if !generated_clip::media_stack() {
                break 'case;
            }
            let clip = demo_clip();
            let pid = std::process::id();
            let tone = std::env::temp_dir().join(format!("tilo-ve-tone2-{pid}.wav"));
            write_tone(&tone, 1.0);

            let dir = std::env::temp_dir();
            let render = |p: &Project, name: &str| {
                let out = dir.join(format!("tilo-ve-{name}-{pid}.mp4"));
                std::fs::remove_file(&out).ok();
                match start_export(p, &ExportSettings::default(), out.clone()).join() {
                    ExportStatus::Done { .. } => {}
                    other => panic!("export {name} failed: {other:?}"),
                }
                out
            };

            let full = render(&project_with_tone(&clip, &tone, 1.0, false), "full");

            let mut half_project = project_with_tone(&clip, &tone, 1.0, false);
            let audio_clip = half_project
                .clips
                .iter()
                .find(|c| {
                    half_project
                        .track(c.track)
                        .is_some_and(|t| t.kind == crate::model::TrackKind::Audio)
                })
                .map(|c| c.id)
                .expect("the audio clip");
            half_project.clip_mut(audio_clip).unwrap().volume = 0.25;
            let quiet = render(&half_project, "quiet");

            let muted = render(&project_with_tone(&clip, &tone, 1.0, true), "muted");

            let loud_peak = audio_peak(&full);
            let quiet_peak = audio_peak(&quiet);
            let muted_peak = audio_peak(&muted);
            println!(
                "encoded peaks: full={loud_peak:.3} quarter={quiet_peak:.3} muted={muted_peak:.3}"
            );

            assert!(
                loud_peak > 0.05,
                "the unmuted export carries audio ({loud_peak})"
            );
            assert!(
                quiet_peak < loud_peak * 0.5,
                "quartering clip volume is audible in the ENCODED file: {loud_peak} → {quiet_peak}",
            );
            assert!(
                muted_peak < loud_peak * 0.1,
                "muting the track silences the ENCODED audio: {loud_peak} → {muted_peak}",
            );

            for f in [&full, &quiet, &muted, &tone] {
                std::fs::remove_file(f).ok();
            }
        }
    }

    /// Write a `secs`-long tone to `path` and hand back its duration.
    ///
    /// A generated tone has a known, plainly non-zero level, so the mixer
    /// assertions do not depend on the source clip's independently tested AAC
    /// track.
    #[cfg(feature = "gst")]
    fn write_tone(path: &std::path::Path, secs: f64) {
        use gstreamer as gst;
        use gstreamer::prelude::*;
        gst::init().expect("gstreamer init");
        let desc = format!(
            "audiotestsrc num-buffers={buffers} wave=sine freq=440 volume=0.8 \
             ! audioconvert ! audioresample \
             ! audio/x-raw,format=S16LE,channels=2,rate=48000 \
             ! wavenc ! filesink location={}",
            path.display(),
            // audiotestsrc emits 1024-sample buffers by default at 48kHz.
            buffers = (secs * 48_000.0 / 1024.0).ceil() as u64,
        );
        let pipeline = gst::parse::launch(&desc)
            .expect("build tone pipeline")
            .downcast::<gst::Pipeline>()
            .expect("pipeline");
        pipeline
            .set_state(gst::State::Playing)
            .expect("play tone pipeline");
        let bus = pipeline.bus().expect("bus");
        for msg in bus.iter_timed(gst::ClockTime::from_seconds(30)) {
            use gst::MessageView;
            match msg.view() {
                MessageView::Eos(_) => break,
                MessageView::Error(e) => panic!("tone encode error: {}", e.error()),
                _ => {}
            }
        }
        let _ = pipeline.set_state(gst::State::Null);
        assert!(
            std::fs::metadata(path).is_ok_and(|m| m.len() > 1024),
            "a real tone file was written to {}",
            path.display(),
        );
    }

    /// `project_over` plus an audio lane carrying `tone`.
    #[cfg(feature = "gst")]
    fn project_with_tone(
        clip: &std::path::Path,
        tone: &std::path::Path,
        secs: f64,
        muted: bool,
    ) -> Project {
        use crate::model::{MediaItem, MediaKind, Track, TrackKind};
        use uuid::Uuid;
        let mut p = project_over(clip, 3.0, secs);
        let aid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: aid,
            name: "tone".into(),
            kind: MediaKind::Audio,
            duration: secs.max(1.0),
            width: 0,
            height: 0,
            fps: 0.0,
            palette: 0,
            path: tone.to_string_lossy().into_owned(),
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "A1".into(),
            kind: TrackKind::Audio,
            index: 1,
            muted,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        });
        let ac = p.add_clip(tid, aid, 0.0).unwrap();
        {
            let c = p.clip_mut(ac).unwrap();
            c.in_point = 0.0;
            c.out_point = secs;
        }
        p
    }

    /// The decoded audio of a media file.
    ///
    /// Fails rather than handing back silence when the file cannot be read. A
    /// helper that answers `0.0` both for "this is quiet" and for "I could not
    /// look" turns a failure to observe into a claim about the subject: the
    /// assertions below would then report an export that had carried audio
    /// perfectly well as one that had produced none, naming the wrong thing as
    /// broken. That is exactly how this test flaked under a full suite run --
    /// the decode came back empty, and "empty" read as "silent".
    #[cfg(feature = "gst")]
    fn decoded_audio(path: &std::path::Path) -> Vec<f32> {
        let uri = crate::media::engine_source(path)
            .to_uri()
            .expect("fixture file URL");
        let pcm = crate::audio::decode_pcm_for_test(&uri).unwrap_or_else(|| {
            panic!(
                "no readable audio stream in {} -- the level cannot be judged",
                path.display()
            )
        });
        assert!(
            pcm.len() > crate::audio::SAMPLE_RATE as usize / 2,
            "only {} samples decoded from {}: too little to say anything about its level",
            pcm.len(),
            path.display(),
        );
        pcm
    }

    /// The peak level of the audio inside a media file. See [`decoded_audio`]:
    /// a file that cannot be read fails here rather than reading as silence.
    #[cfg(feature = "gst")]
    fn audio_peak(path: &std::path::Path) -> f32 {
        let pcm = decoded_audio(path);
        crate::audio::peak(&pcm)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn encoder_selection_labels_and_bitrates_match_the_pipeline() {
        {
            for container in Container::ALL {
                let s = ExportSettings {
                    container,
                    ..Default::default()
                };
                let silent = pipeline_desc_with_audio(&s, 64, 64, "/tmp/o", false);
                assert!(
                    !silent.contains("tilo_audio"),
                    "a project with no audio gets no audio branch: {silent}",
                );

                let voiced = pipeline_desc_with_audio(&s, 64, 64, "/tmp/o", true);
                let selected = audio_encoder(container);
                let enc = selected.spec.element;
                assert!(voiced.contains("appsrc name=tilo_audio"), "{voiced}");
                assert!(
                    voiced.contains(enc),
                    "{container:?} encodes audio with {enc}: {voiced}"
                );
                // The branch has to end at the same muxer the video goes to, or
                // it would be a second file's worth of graph going nowhere — and
                // it carries the rate the size estimate quotes on the way there.
                let arg = selected.spec.bitrate_arg(AUDIO_BITRATE_BPS);
                assert!(
                    voiced.contains("name=tilo_mux")
                        && voiced.contains(&format!("{enc} {arg} ! tilo_mux.")),
                    "the audio branch encodes at {arg} and links into the video's muxer: {voiced}",
                );
                assert!(
                    voiced.contains(&format!("rate={}", crate::audio::SAMPLE_RATE)),
                    "the caps state the rate the mixer produces: {voiced}",
                );
            }
        }
        {
            // Every choice in the dialog has to name what the user will get. The
            // *encoder* now varies with the machine, so the label is checked
            // against the codec the container carries — and the encoder actually
            // selected is checked to be one that writes that codec, which is what
            // keeps the two from drifting apart.
            for container in Container::ALL {
                let (sel, _, _) = encode_chain(container);
                let codec = video_codec(container);
                let label = container.label().to_lowercase();
                let named = match codec {
                    VideoCodec::H264 => "h.264",
                    VideoCodec::Vp8 => "vp8",
                    VideoCodec::Vp9 => "vp9",
                    VideoCodec::Av1 => "av1",
                };
                assert!(
                    label.contains(named),
                    "{container:?} is labelled {:?} but carries {codec:?}",
                    container.label(),
                );
                assert!(
                    codec
                        .candidates()
                        .iter()
                        .any(|c| c.element == sel.spec.element),
                    "{container:?} is labelled {named} but selected {}, which does not encode it",
                    sel.spec.element,
                );
            }
        }
        {
            // The whole point of carrying units per encoder: the number in the
            // pipeline has to *mean* the bitrate the dialog's size estimate quotes,
            // whichever encoder this machine chose. Reconstructing it from the
            // selection is not a tautology — it is checking that the description
            // built for the runner used the converted value rather than the raw
            // bits, which is the difference between 16 Mbit/s and 16 Gbit/s.
            for container in Container::ALL {
                for resolution in Resolution::ALL {
                    let s = ExportSettings {
                        resolution,
                        container,
                        ..Default::default()
                    };
                    let desc = pipeline_desc(&s, 640, 360, "/tmp/o");
                    let (sel, _, _) = encode_chain(container);
                    let bps = (resolution.target_mbps() * 1_000_000.0) as u64;
                    let want = format!("{} {}", sel.spec.element, sel.spec.bitrate_arg(bps));
                    assert!(
                        desc.contains(&want),
                        "expected {want:?} in the pipeline for {container:?}/{resolution:?}: {desc}",
                    );
                    // And the value is a real rate, never the encoders' "auto".
                    assert!(
                        !desc.contains(&format!("{}=0 ", sel.spec.bitrate_property)),
                        "the encoder must not be left on its own default: {desc}",
                    );
                }
            }
        }
        {
            for container in Container::ALL {
                let report = selected_encoders(container);
                let video = report
                    .video
                    .as_ref()
                    .expect("a video encoder is always named");
                assert!(
                    !video.spec.element.is_empty(),
                    "the report names a real element: {}",
                    report.describe(),
                );
                // The honesty rule, stated as an assertion: a selection that no
                // registry confirmed can never come back as hardware.
                if !video.confirmed {
                    assert!(
                        !report.is_hardware(),
                        "an unconfirmed selection reported hardware: {}",
                        report.describe(),
                    );
                }
                // Whatever ran, the description says which family it was, so a
                // caller states it rather than assuming it.
                let d = report.describe();
                assert!(d.contains("video: "), "{d}");
                assert!(d.contains("audio: "), "{d}");
            }
        }
        {
            let p = Project::new("t");
            let s = ExportSettings::default();
            match (unavailable_reason(), begin_export(&p, &s)) {
                // No encoder linked: the reason given to the dialog is the same
                // one the attempt fails with.
                (Some(reason), ExportStatus::Failed { reason: got }) => {
                    assert_eq!(
                        reason, got,
                        "the dialog and the attempt must say the same thing"
                    );
                }
                // An encoder is linked, so nothing may claim otherwise. The
                // empty project has no clips but still renders a black frame.
                (None, status) => assert!(
                    !matches!(&status, ExportStatus::Failed { reason } if reason == UNAVAILABLE),
                    "a gst build must not report export unavailable, got {status:?}",
                ),
                (Some(reason), other) => {
                    panic!("export claims to be unavailable ({reason}) yet returned {other:?}")
                }
            }
        }
        {
            let mp4 = ExportSettings {
                container: Container::Mp4,
                ..Default::default()
            };
            let d = pipeline_desc(&mp4, 1920, 1080, "/tmp/out.mp4");
            assert!(
                d.contains("appsrc name=tilo_src"),
                "an appsrc feeds real frames: {d}"
            );
            assert!(
                d.contains("format=RGBA,width=1920,height=1080"),
                "caps match the frames: {d}"
            );
            // Which H.264 encoder this is depends on the machine — a VA-API,
            // NVENC or QSV one where the registry has it, openh264enc where it
            // does not — so the assertion is against the encoder that was actually
            // selected, at the bitrate the size estimate quotes, in that encoder's
            // own units. (`media::encode` proves the fallback lands on openh264enc
            // when nothing else is installed, against a registry it fabricates,
            // which is a check no real machine can weaken.)
            let (sel, _, _) = encode_chain(Container::Mp4);
            assert!(
                !sel.spec.kind.is_hardware() || sel.confirmed,
                "a hardware element is only ever named once the registry confirmed it: {}",
                sel.describe(),
            );
            assert!(
                d.contains(sel.spec.element),
                "the selected H.264 encoder: {d}"
            );
            assert!(
                d.contains(&format!(
                    "{} {}",
                    sel.spec.element,
                    sel.spec.bitrate_arg(16_000_000)
                )),
                "the encoder is told to hit the bitrate the size estimate quotes: {d}",
            );
            assert!(d.contains("h264parse ! mp4mux"), "parsed + MP4-muxed: {d}");
            assert!(
                d.contains("filesink location=\"/tmp/out.mp4\""),
                "writes a real file: {d}"
            );

            let webm = ExportSettings {
                container: Container::WebM,
                ..Default::default()
            };
            let dw = pipeline_desc(&webm, 640, 360, "/tmp/o.webm");
            let (vp8, _, _) = encode_chain(Container::WebM);
            assert!(
                dw.contains(vp8.spec.element) && dw.contains("webmmux"),
                "VP8/WebM chain: {dw}"
            );
            assert!(
                dw.contains(&format!(
                    "{} {}=",
                    vp8.spec.element, vp8.spec.bitrate_property
                )),
                "the VP8 encoder gets its bitrate under whichever property it names: {dw}",
            );
            assert!(!dw.contains("h264parse"), "no h264parse for WebM: {dw}");

            let mov = ExportSettings {
                container: Container::Mov,
                ..Default::default()
            };
            assert!(
                pipeline_desc(&mov, 2, 2, "/tmp/o.mov").contains("qtmux"),
                "MOV muxes with qtmux"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn export_sizes_filenames_and_dimensions_are_predictable() {
        // Case `the_size_line_quotes_a_size_only_where_one_can_be_predicted`:
        // The dialog's size line must not quote a megabyte figure the encoder
        // cannot deliver.
        //
        // The estimate has been wrong in both directions: it once charged
        // 0.256 Mbit/s for a stereo track the pipeline had no branch to encode,
        // and once — after that allowance was removed — went on quoting a
        // video-only figure while the pipeline muxed real audio. Both were
        // confident numbers about something else. So this asserts on the two
        // things that make the figure mean anything: it accounts for audio when
        // and only when audio will be muxed, and it is withheld entirely where
        // the selected encoder is [not
        // rate-controlling](media::encode::Selection::bitrate_is_predictive).
        //
        // Which branch each container takes is a fact about this machine, so the
        // test asks the selection rather than assuming — and covers both branches
        // wherever the machine has one predictive encoder and one that is not.
        {
            use crate::model::TrackKind;
            let silent = project_of("silent", TrackKind::Video);
            let voiced = project_of("voiced", TrackKind::Audio);
            assert!(!crate::audio::has_audio(&silent) && crate::audio::has_audio(&voiced));
            assert!(
                (silent.duration() - voiced.duration()).abs() < 1e-9,
                "the two projects must be the same length, or the comparison below \
             measures duration rather than the audio allowance: {} vs {}",
                silent.duration(),
                voiced.duration(),
            );

            // Both encoder choices, whatever this machine happens to have: one
            // that rate-controls and one measured not to. A box with no hardware
            // encoder resolves every container to the second kind, so driving
            // only the machine's own choice would leave the megabyte branch
            // unexecuted.
            let confirmed = |element: &str| {
                let spec = media::encode::VideoCodec::H264
                    .candidates()
                    .iter()
                    .chain(media::encode::VideoCodec::Vp8.candidates())
                    .find(|s| s.element == element)
                    .unwrap_or_else(|| panic!("{element} is in the candidate tables"));
                media::encode::Selection {
                    spec,
                    confirmed: true,
                    passed_over: Vec::new(),
                }
            };
            let choices = [confirmed("x264enc"), confirmed("openh264enc")];

            let mut saw_predictive = false;
            let mut saw_unpredictable = false;
            for container in Container::ALL {
                let s = ExportSettings {
                    container,
                    ..Default::default()
                };
                // The machine's own choice must agree with the same call made
                // explicitly, or these two branches are testing something the
                // dialog does not use.
                let (machine, _, _) = encode_chain(container);
                assert_eq!(
                    s.estimate_label(&voiced),
                    s.estimate_label_for(&voiced, &machine),
                    "the dialog's label is the one these branches exercise",
                );

                for video in &choices {
                    let (a, b) = (
                        s.estimate_label_for(&silent, video),
                        s.estimate_label_for(&voiced, video),
                    );
                    println!(
                        "{container:?} via {}: silent {a:?}, with audio {b:?}",
                        video.describe()
                    );

                    // The arithmetic first, at full precision and independent of how
                    // the label rounds: an audio track costs exactly the rate the
                    // audio branch is given.
                    let d = voiced.duration();
                    let charged = s.estimated_mb(d, true) - s.estimated_mb(d, false);
                    let want = AUDIO_BITRATE_BPS as f64 / 1_000_000.0 * d / 8.0;
                    assert!(
                        (charged - want).abs() < 1e-9,
                        "muxing audio costs the {AUDIO_BITRATE_BPS} bps the branch encodes at: \
                     charged {charged} MB, expected {want} MB",
                    );

                    if video.bitrate_is_predictive() {
                        saw_predictive = true;
                        assert!(
                            a.contains("MB") && b.contains("MB"),
                            "a size is quoted: {a:?} / {b:?}"
                        );
                        // The audio track has to cost something, or the figure is
                        // still describing a video-only file.
                        let mb = |label: &str| {
                            label
                                .trim_start_matches(['≈', ' '])
                                .trim_end_matches(" MB")
                                .parse::<f64>()
                                .unwrap_or_else(|e| panic!("a megabyte figure in {label:?}: {e}"))
                        };
                        assert!(
                            mb(&b) > mb(&a),
                            "muxing audio must cost more than not muxing it: {a:?} vs {b:?}",
                        );
                    } else {
                        saw_unpredictable = true;
                        assert!(
                            !a.contains("MB") && !b.contains("MB"),
                            "{} does not track its bitrate, so no size may be quoted: {a:?} / {b:?}",
                            video.spec.element,
                        );
                        // What *is* known — the rate that was asked for — is still
                        // said, so the line is informative rather than merely blank.
                        let mbps = format!("{:.0} Mbit/s", s.resolution.target_mbps());
                        assert!(a.contains(&mbps) && b.contains(&mbps), "{a:?} / {b:?}");
                        assert!(
                            b.contains(&format!("{} kbit/s audio", AUDIO_BITRATE_BPS / 1000)),
                            "and the audio rate, when there is audio: {b:?}",
                        );
                        assert!(!a.contains("kbit/s"), "but not when there is none: {a:?}");
                    }
                }
            }
            println!(
                "size line: predictive branch seen={saw_predictive}, \
                  unpredictable branch seen={saw_unpredictable}"
            );
            assert!(
                saw_predictive && saw_unpredictable,
                "both branches must be exercised, saw predictive={saw_predictive} \
             unpredictable={saw_unpredictable} — a branch that never ran is a branch \
             this test says nothing about",
            );
        }
        {
            let d = 60.0;
            let p720 = ExportSettings {
                resolution: Resolution::P720,
                ..Default::default()
            }
            .estimated_mb(d, true);
            let p2160 = ExportSettings {
                resolution: Resolution::P2160,
                ..Default::default()
            }
            .estimated_mb(d, true);
            assert!(p2160 > p720 * 4.0, "4K should be much heavier than 720p");
        }
        {
            let s = ExportSettings {
                container: Container::WebM,
                ..Default::default()
            };
            assert_eq!(s.suggested_filename("My Doc v3"), "My_Doc_v3.webm");
        }
        {
            let mut p = Project::new("t");
            p.width = 1440;
            p.height = 1080; // 4:3
            let s = ExportSettings {
                resolution: Resolution::P720,
                ..Default::default()
            };
            let (w, h) = output_dims(&s, &p);
            assert_eq!(h, 720);
            assert_eq!(w, 960, "4:3 at 720p is 960×720");
            assert_eq!(w % 2, 0);
            assert_eq!(h % 2, 0);
        }
    }

    #[test]
    fn the_size_estimate_matches_the_bitrate_the_encoder_is_given() {
        // The estimate is only honest if it describes the encode that
        // actually runs.
        for container in Container::ALL {
            for resolution in Resolution::ALL {
                let s = ExportSettings {
                    resolution,
                    container,
                    ..Default::default()
                };
                let d = pipeline_desc(&s, 1920, 1080, "/tmp/o");
                let bps = (resolution.target_mbps() * 1_000_000.0) as u64;
                // The rate is written in the selected encoder's own units —
                // bits for openh264 and libvpx, kilobits for x264 and every
                // hardware encoder — so which number appears depends on what
                // this machine has installed. Ask the selection for it rather
                // than assuming the bits-per-second baseline.
                let (sel, _, _) = encode_chain(container);
                let arg = sel.spec.launch_args(bps);
                assert!(
                    d.contains(&format!("{} {arg} !", sel.spec.element)),
                    "{container:?} at {} must encode at the quoted {bps} bps ({arg}): {d}",
                    resolution.label(),
                );
                // 10s at that bitrate, in megabytes, video only.
                let want = resolution.target_mbps() * 10.0 / 8.0;
                assert!(
                    (s.estimated_mb(10.0, false) - want).abs() < 1e-9,
                    "a silent export quotes exactly the encoded video bitrate, nothing more",
                );
                // And with audio, exactly that plus the rate the audio branch
                // is given — which is written into the same graph.
                let audio_branch = audio_branch_desc(container, "tilo_mux").expect("audio branch");
                assert!(
                    audio_branch.contains(&format!("={AUDIO_BITRATE_BPS} !")),
                    "{container:?} must encode audio at the quoted {AUDIO_BITRATE_BPS} bps: \
                     {audio_branch}",
                );
                let want_av = want + AUDIO_BITRATE_BPS as f64 / 1_000_000.0 * 10.0 / 8.0;
                assert!(
                    (s.estimated_mb(10.0, true) - want_av).abs() < 1e-9,
                    "an export that muxes audio quotes the video rate plus the audio one",
                );
            }
        }
    }

    /// One ten-second clip on a track of `kind`. Nothing here decodes — the
    /// estimate reads the document, not the media.
    ///
    /// The two kinds give a silent and a voiced project of **identical
    /// duration**, which is what lets the two size labels be compared: with
    /// different durations the audio allowance would be lost inside the
    /// difference in length.
    fn project_of(name: &str, kind: crate::model::TrackKind) -> Project {
        use crate::model::{MediaItem, MediaKind, Track};
        use uuid::Uuid;
        let mut p = Project::new(name);
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "clip".into(),
            kind: if kind == crate::model::TrackKind::Audio {
                MediaKind::Audio
            } else {
                MediaKind::Video
            },
            duration: CLIP_SECS,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "T1".into(),
            kind,
            index: 1,
            ..Default::default()
        });
        let cid = p.add_clip(tid, mid, 0.0).expect("the media exists");
        p.clip_mut(cid).expect("clip").out_point = CLIP_SECS;
        p
    }

    /// Long enough that the audio allowance survives the label's rounding to
    /// whole megabytes: 128 kbit/s over a minute is 0.96 MB, so the two
    /// figures differ by one. At ten seconds it is 0.16 MB and both labels
    /// read the same — true of the display, but then the label can say
    /// nothing about whether audio was accounted for.
    const CLIP_SECS: f64 = 60.0;

    #[cfg(not(feature = "gst"))]
    #[test]
    fn default_build_reports_export_unavailable() {
        let p = Project::new("t");
        let s = ExportSettings::default();
        // Without the gst feature no encoder is wired in, so begin_export must
        // be honest — a clear "can't export" status, never a faked progress
        // bar and never a `Done` for a file that was not written.
        match begin_export(&p, &s) {
            ExportStatus::Failed { reason } => {
                assert_eq!(reason, UNAVAILABLE);
                // Build details are for developers; a person only learns
                // what they cannot do here.
                for internal in ["gstreamer", "--features", "build", "`"] {
                    assert!(
                        !reason.to_lowercase().contains(internal),
                        "the failure must not mention {internal:?}, got {reason:?}",
                    );
                }
            }
            other => panic!("expected an honest unavailable status, got {other:?}"),
        }
    }

    /// A project holding one clip of `path`, which is exactly what the
    /// export probe and a hand-built project look like.
    fn one_clip_over(path: &str) -> Project {
        use crate::model::{MediaItem, MediaKind, Track, TrackKind};
        use uuid::Uuid;
        let mut p = Project::new("relative");
        p.width = 320;
        p.height = 180;
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "clip".into(),
            kind: MediaKind::Video,
            duration: 2.0,
            width: 320,
            height: 180,
            fps: 30.0,
            palette: 3,
            path: path.to_string(),
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            ..Default::default()
        });
        let cid = p.add_clip(tid, mid, 0.0).expect("media exists");
        p.clip_mut(cid).unwrap().out_point = 0.2;
        p
    }

    #[test]
    #[cfg(feature = "gst")]
    #[serial_test::serial(video_editor_gst)]
    fn real_export_encodes_footage_grades_and_transitions() {
        // Case `real_export_writes_a_playable_mp4`:
        // Real export through GStreamer: encodes a short two-frame project to a
        // real file and asserts a valid, non-empty container was written. Only
        // compiled under `--features gst`; the encoder/mux plugins must be present.
        //
        // The clip is a **title**, whose picture is drawn from the document. It
        // used to be a video reference whose bytes were not on this machine —
        // which the export now refuses, correctly: that project could only ever
        // have encoded stand-in fills. A title needs no decoder, so the encode
        // and mux path this test is about still runs end to end, over a picture
        // that is genuinely the one the timeline holds.
        'case: {
            use crate::model::{MediaItem, MediaKind, TextSource, Track, TrackKind};
            use uuid::Uuid;
            if !generated_clip::media_stack() {
                break 'case;
            }
            // A tiny real project: one 0.2s clip so the export loop runs quickly.
            let mut p = Project::new("Export Test");
            let mid = Uuid::new_v4();
            p.media.push(MediaItem {
                id: mid,
                name: "c".into(),
                kind: MediaKind::Text,
                duration: 0.2,
                width: 320,
                height: 180,
                fps: 30.0,
                palette: 3,
                path: String::new(),
                text: Some(TextSource::new("EXPORT")),
                ..Default::default()
            });
            let tid = Uuid::new_v4();
            p.tracks.push(Track {
                id: tid,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                muted: false,
                locked: false,
                solo: false,
                gain_db: 0.0,
                pan: 0.0,
            });
            let cid = p.add_clip(tid, mid, 0.0).unwrap();
            p.clip_mut(cid).unwrap().out_point = 0.2;

            let out =
                std::env::temp_dir().join(format!("tilo-ve-export-{}.mp4", std::process::id()));
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_VE_EXPORT_OUT", &out);
            let s = ExportSettings {
                resolution: Resolution::P720,
                container: Container::Mp4,
                frame_rate: FrameRate::F24,
            };
            let status = begin_export(&p, &s);
            drop(env);

            match status {
                ExportStatus::Done { path } => {
                    assert_eq!(path, out.to_string_lossy());
                    let bytes = std::fs::read(&out).expect("export file exists");
                    assert!(
                        bytes.len() > 512,
                        "a real encoded file, got {} bytes",
                        bytes.len()
                    );
                    // A valid ISO-BMFF file has an `ftyp` box type at offset 4.
                    assert_eq!(&bytes[4..8], b"ftyp", "the output is a real MP4 container");
                }
                other => panic!("real export must write a file, got {other:?}"),
            }
            std::fs::remove_file(&out).ok();
        }
        // Case `the_export_encodes_the_source_footage`:
        // The bug this whole path exists to kill: the export used to encode a
        // synthesized gradient built from a palette table, under *every*
        // feature combination. Prove the encoded pictures are the source
        // footage — structurally, frame by frame — and that they are nothing
        // like the representative fill the palette would have produced.
        'case: {
            if !generated_clip::media_stack() {
                break 'case;
            }
            let clip = demo_clip();
            // A second and a half of real video, from a second in — far enough
            // past the head that the picture has content.
            let project = project_over(&clip, 1.0, 1.5);
            let out =
                std::env::temp_dir().join(format!("tilo-ve-footage-{}.mp4", std::process::id()));
            match export_to(&project, &out) {
                ExportStatus::Done { .. } => {}
                other => panic!("the export must write a file, got {other:?}"),
            }

            // Three instants across the exported clip, each compared with the
            // source picture it was cut from.
            let mut matched = 0;
            for (timeline_t, source_t) in [(0.2, 1.2), (0.7, 1.7), (1.2, 2.2)] {
                let exported = luma_grid(&out, timeline_t, 12);
                let source = luma_grid(&clip, source_t, 12);
                let same = correlation(&exported, &source);

                // The picture the *old* code would have encoded: the clip's
                // palette colour through the representative fill. It has no
                // horizontal structure at all, so it cannot correlate with real
                // footage the way the real frame does.
                let against_gradient = {
                    let gradient = crate::preview::render_rgba(&project, timeline_t, 12, 12);
                    let grid: Vec<f64> = gradient
                        .chunks(4)
                        .map(|p| {
                            (0.2126 * f64::from(p[0])
                                + 0.7152 * f64::from(p[1])
                                + 0.0722 * f64::from(p[2]))
                                / 255.0
                        })
                        .collect();
                    correlation(&exported, &grid)
                };

                assert!(
                    same > 0.9,
                    "the exported frame at {timeline_t}s must be the source picture at \
                 {source_t}s (correlation {same:.3})",
                );
                assert!(
                    same > against_gradient + 0.3,
                    "the exported frame at {timeline_t}s is the footage ({same:.3}), not the \
                 synthesized gradient ({against_gradient:.3})",
                );
                matched += 1;
            }
            assert_eq!(matched, 3);

            // Distinct instants of the timeline must be distinct pictures —
            // an export stuck on one frame would still correlate above.
            let early = luma_grid(&out, 0.2, 12);
            let late = luma_grid(&out, 1.2, 12);
            assert!(
                correlation(&early, &late) < 0.995,
                "the export advances through the footage rather than holding one frame",
            );
            std::fs::remove_file(&out).ok();
        }
        // Case `the_grade_changes_the_encoded_footage`:
        // The grade used to be computed on the palette colour, so it never
        // touched a decoded pixel. Prove it reaches the encoded footage.
        'case: {
            if !generated_clip::media_stack() {
                break 'case;
            }
            let clip = demo_clip();
            let neutral = project_over(&clip, 1.0, 0.6);
            let mut bright = neutral.clone();
            let cid = bright.clips[0].id;
            bright.clip_mut(cid).unwrap().filters = crate::model::Filters {
                brightness: 0.85,
                contrast: 0.5,
                saturation: 0.5,
            };
            let mut grey = neutral.clone();
            grey.clip_mut(cid).unwrap().effects = vec![crate::model::ClipEffect {
                id: uuid::Uuid::new_v4(),
                kind: crate::model::EffectKind::Grayscale,
                amount: 1.0,
            }]
            .into();

            let dir = std::env::temp_dir();
            let pid = std::process::id();
            let a = dir.join(format!("tilo-ve-grade-neutral-{pid}.mp4"));
            let b = dir.join(format!("tilo-ve-grade-bright-{pid}.mp4"));
            let c = dir.join(format!("tilo-ve-grade-grey-{pid}.mp4"));
            for (p, out) in [(&neutral, &a), (&bright, &b), (&grey, &c)] {
                match export_to(p, out) {
                    ExportStatus::Done { .. } => {}
                    other => panic!("export failed: {other:?}"),
                }
            }

            let mean = |g: &[f64]| g.iter().sum::<f64>() / g.len() as f64;
            let neutral_luma = mean(&luma_grid(&a, 0.3, 8));
            let bright_luma = mean(&luma_grid(&b, 0.3, 8));
            assert!(
                bright_luma > neutral_luma + 0.05,
                "raising brightness must brighten the ENCODED footage: \
             {neutral_luma:.3} → {bright_luma:.3}",
            );

            // ...and the structure survives, so it is the same footage graded,
            // not a different picture entirely.
            assert!(
                correlation(&luma_grid(&a, 0.3, 12), &luma_grid(&b, 0.3, 12)) > 0.9,
                "the brightened export is the same footage, graded",
            );

            // Grayscale must actually desaturate the real pixels.
            let colourfulness = |path: &std::path::Path| {
                let f = decode_one(path, 0.3);
                let mut spread = 0.0f64;
                let mut n = 0.0f64;
                for px in f.rgba.chunks(4) {
                    let (r, g, bl) = (f64::from(px[0]), f64::from(px[1]), f64::from(px[2]));
                    spread += (r - g).abs() + (g - bl).abs();
                    n += 1.0;
                }
                spread / n.max(1.0)
            };
            let colour_before = colourfulness(&a);
            let colour_after = colourfulness(&c);
            assert!(
                colour_after < colour_before * 0.5,
                "a grayscale effect must desaturate the ENCODED footage: \
             {colour_before:.2} → {colour_after:.2}",
            );

            for f in [&a, &b, &c] {
                std::fs::remove_file(f).ok();
            }
        }
        // Case `transitions_reach_the_encoded_file_as_distinct_pictures`:
        // Every transition must reach the encoded file as its own picture.
        'case: {
            use crate::model::Transition;
            if !generated_clip::media_stack() {
                break 'case;
            }
            let clip = demo_clip();
            // Two clips over one source, the incoming one inverted so the two
            // pictures are unmistakably different whatever the footage does.
            let build = |kind: Transition| {
                let mut p = project_over(&clip, 2.0, 1.0);
                let track = p.tracks[0].id;
                let media = p.media[0].id;
                let second = p.add_clip(track, media, 1.0).unwrap();
                {
                    let c = p.clip_mut(second).unwrap();
                    c.in_point = 3.0;
                    c.out_point = 4.0;
                    c.transition = kind;
                    c.effects = vec![crate::model::ClipEffect {
                        id: uuid::Uuid::new_v4(),
                        kind: crate::model::EffectKind::Invert,
                        amount: 1.0,
                    }]
                    .into();
                }
                p
            };

            let dir = std::env::temp_dir();
            let pid = std::process::id();
            let mut grids = Vec::new();
            for kind in [
                Transition::CrossDissolve,
                Transition::FadeIn,
                Transition::DipToBlack,
                Transition::Wipe,
            ] {
                let out = dir.join(format!("tilo-ve-tr-{}-{pid}.mp4", kind.as_str()));
                match export_to(&build(kind), &out) {
                    ExportStatus::Done { .. } => {}
                    other => panic!("{kind:?} export failed: {other:?}"),
                }
                // Mid-transition: the incoming clip starts at 1.0s and the
                // transition runs for TRANSITION_SECS.
                let at = 1.0 + crate::preview::TRANSITION_SECS * 0.5;
                grids.push((kind, luma_grid(&out, at, 12)));
                std::fs::remove_file(&out).ok();
            }

            // A wipe is geometry: halfway across, one side of the ENCODED frame
            // is the incoming clip and the other is still the outgoing one. A
            // dissolve mixes the two evenly everywhere, so it must NOT show
            // that split. This is what tells the two apart even when their
            // whole-frame averages agree.
            let half_split = |g: &[f64]| {
                let n = 12;
                let mean_of = |cols: std::ops::Range<usize>| {
                    let mut sum = 0.0;
                    let mut count = 0.0;
                    for row in 0..n {
                        for col in cols.clone() {
                            sum += g[row * n + col];
                            count += 1.0;
                        }
                    }
                    sum / count
                };
                (mean_of(0..5) - mean_of(7..12)).abs()
            };
            let wipe_split = half_split(
                &grids
                    .iter()
                    .find(|(k, _)| *k == Transition::Wipe)
                    .unwrap()
                    .1,
            );
            let dissolve_split = half_split(
                &grids
                    .iter()
                    .find(|(k, _)| *k == Transition::CrossDissolve)
                    .unwrap()
                    .1,
            );
            assert!(
                wipe_split > dissolve_split + 0.1,
                "the encoded wipe must split the frame left/right ({wipe_split:.3}) far more \
             than an even dissolve does ({dissolve_split:.3})",
            );

            for i in 0..grids.len() {
                for j in (i + 1)..grids.len() {
                    let r = correlation(&grids[i].1, &grids[j].1);
                    let mean_i = grids[i].1.iter().sum::<f64>() / grids[i].1.len() as f64;
                    let mean_j = grids[j].1.iter().sum::<f64>() / grids[j].1.len() as f64;
                    assert!(
                        r < 0.98 || (mean_i - mean_j).abs() > 0.05,
                        "{:?} and {:?} encoded the same picture (correlation {r:.3}, \
                     mean {mean_i:.3} vs {mean_j:.3}) — transitions must differ",
                        grids[i].0,
                        grids[j].0,
                    );
                }
            }
        }
    }

    /// Deterministic moving footage encoded by the same `GStreamer` stack the
    /// export tests exercise. Generation remains fail-loud.
    #[cfg(feature = "gst")]
    fn demo_clip() -> std::path::PathBuf {
        generated_clip::encoded_video()
    }

    /// A one-clip project sourcing `path` from `in_point` for `len` seconds.
    #[cfg(feature = "gst")]
    fn project_over(path: &std::path::Path, in_point: f64, len: f64) -> Project {
        use crate::model::{MediaItem, MediaKind, Track, TrackKind};
        use uuid::Uuid;
        let mut p = Project::new("Footage");
        p.width = 1920;
        p.height = 1080;
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "demo".into(),
            kind: MediaKind::Video,
            duration: generated_clip::DURATION_SECS,
            width: generated_clip::WIDTH,
            height: generated_clip::HEIGHT,
            fps: f64::from(generated_clip::FPS),
            palette: 3,
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        });
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        {
            let c = p.clip_mut(cid).unwrap();
            c.in_point = in_point;
            c.out_point = in_point + len;
        }
        p
    }

    /// Decode `path` and return the picture at `at_secs`, downscaled to a
    /// `n × n` luminance grid — small enough that encoder noise and scaling
    /// wash out, detailed enough that two different pictures never agree.
    #[cfg(feature = "gst")]
    fn decode_one(path: &std::path::Path, at_secs: f64) -> ::media::VideoFrame {
        let source = crate::media::engine_source(path);
        let mut pool = ::media::SessionPool::for_editor();
        let session = pool.session(&source);
        let mut s = session.lock().expect("session");
        s.set_fps(30.0);
        let frame = s
            .frame_at(
                (at_secs.max(0.0) * 1000.0) as u64,
                std::time::Duration::from_secs(20),
            )
            .unwrap_or_else(|| panic!("decode {} at {at_secs}s", path.display()));
        drop(s);
        frame
            .to_video_frame()
            .unwrap_or_else(|| panic!("{} produced a GPU-resident frame", path.display()))
    }

    #[cfg(feature = "gst")]
    fn luma_grid(path: &std::path::Path, at_secs: f64, n: usize) -> Vec<f64> {
        let frame = decode_one(path, at_secs);
        let (w, h) = (frame.width as usize, frame.height as usize);
        let mut grid = vec![0.0; n * n];
        for gy in 0..n {
            for gx in 0..n {
                let (x0, x1) = (gx * w / n, ((gx + 1) * w / n).max(gx * w / n + 1));
                let (y0, y1) = (gy * h / n, ((gy + 1) * h / n).max(gy * h / n + 1));
                let mut sum = 0.0;
                let mut count = 0.0;
                for y in y0..y1.min(h) {
                    for x in x0..x1.min(w) {
                        let i = (y * w + x) * 4;
                        sum += 0.2126 * f64::from(frame.rgba[i])
                            + 0.7152 * f64::from(frame.rgba[i + 1])
                            + 0.0722 * f64::from(frame.rgba[i + 2]);
                        count += 1.0;
                    }
                }
                grid[gy * n + gx] = if count > 0.0 {
                    sum / count / 255.0
                } else {
                    0.0
                };
            }
        }
        grid
    }

    /// Pearson correlation between two luminance grids. Correlation, not
    /// absolute difference, because the export is scaled and re-encoded —
    /// what must survive is the *structure* of the picture.
    #[cfg(feature = "gst")]
    fn correlation(a: &[f64], b: &[f64]) -> f64 {
        assert_eq!(a.len(), b.len());
        let n = a.len() as f64;
        let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
        let mut num = 0.0;
        let (mut da, mut db) = (0.0, 0.0);
        for (x, y) in a.iter().zip(b) {
            num += (x - ma) * (y - mb);
            da += (x - ma) * (x - ma);
            db += (y - mb) * (y - mb);
        }
        if da <= 1e-12 || db <= 1e-12 {
            return 0.0;
        }
        num / (da * db).sqrt()
    }

    #[cfg(feature = "gst")]
    fn export_to(project: &Project, out: &std::path::Path) -> ExportStatus {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_VE_EXPORT_OUT", out);
        let settings = ExportSettings {
            resolution: Resolution::P720,
            container: Container::Mp4,
            frame_rate: FrameRate::F30,
        };
        begin_export(project, &settings)
    }
}
