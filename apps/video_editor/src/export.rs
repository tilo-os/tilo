//! Export settings + the (gated) render pipeline.
//!
//! Real encode/decode runs through GStreamer, which is only linked when
//! the crate is built with `--features gst`. The default build — the one
//! the fleet's `qos-build` invokes — keeps the pipeline a stub so the app
//! compiles and screenshots without the gstreamer-1.0 dev headers being
//! present. The UI (the export dialog) is fully live either way; only the
//! actual muxing is gated.

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
    /// capable of. WebM here is VP8 and MOV is H.264, because that is what
    /// [`encode_chain`] plugs; naming VP9 or ProRes would be telling the
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
    /// Estimated output size in megabytes for a `duration`-second project.
    ///
    /// Video only, because video is all the pipeline encodes — there is no
    /// audio track in the output, so charging the estimate for one would
    /// quote the user a size for something they are not getting. The
    /// figure is honest for the same reason the bitrate is real: the
    /// encoder is told to hit [`Resolution::target_mbps`].
    pub fn estimated_mb(&self, duration: f64) -> f64 {
        self.resolution.target_mbps() * duration / 8.0
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

/// The GStreamer element chain that encodes the export, per container: the
/// (encoder, optional parser, muxer) triple. Kept pure + shared so the pipeline
/// **construction** is verifiable without a gstreamer stack (see tests).
///
/// The encoders/muxers are the real elements the runner plugs:
///   * MP4 / MOV — `openh264enc ! h264parse ! (mp4mux|qtmux)` (H.264).
///   * WebM — `vp8enc ! webmmux` (VP8, WebM's baseline codec).
pub fn encode_chain(container: Container) -> (&'static str, Option<&'static str>, &'static str) {
    match container {
        Container::Mp4 => ("openh264enc", Some("h264parse"), "mp4mux"),
        Container::Mov => ("openh264enc", Some("h264parse"), "qtmux"),
        Container::WebM => ("vp8enc", None, "webmmux"),
    }
}

/// The encoder property that carries a target bitrate in bits per second.
/// Both encoders default far below any of the presets — openh264enc sits at
/// 128 kbps — so without setting this the size estimate in the dialog would
/// be describing a bitrate nothing was ever asked to hit.
fn bitrate_property(container: Container) -> &'static str {
    match container {
        Container::Mp4 | Container::Mov => "bitrate",
        Container::WebM => "target-bitrate",
    }
}

/// Build the `gst-launch`-style pipeline description the export runs: a raw
/// RGBA `appsrc` (fed the composited frames) through `videoconvert`, the
/// container's encode chain, and a `filesink`. Pure, so the exact graph is
/// unit-tested on every build even where gstreamer can't link.
pub fn pipeline_desc(settings: &ExportSettings, w: u32, h: u32, out: &str) -> String {
    let (enc, parse, mux) = encode_chain(settings.container);
    let fps = settings.frame_rate.fps() as u32;
    let parse = parse.map(|p| format!("{p} ! ")).unwrap_or_default();
    let bitrate = (settings.resolution.target_mbps() * 1_000_000.0) as u64;
    let bitrate_prop = bitrate_property(settings.container);
    format!(
        "appsrc name=qos_src is-live=false format=time \
         caps=video/x-raw,format=RGBA,width={w},height={h},framerate={fps}/1 \
         ! videoconvert ! {enc} {bitrate_prop}={bitrate} ! {parse}{mux} ! filesink location={out}"
    )
}

/// Output dimensions for the export: the resolution preset's height, with the
/// width scaled to the project's aspect ratio and rounded to an even number
/// (H.264 requires even dimensions).
pub fn output_dims(settings: &ExportSettings, project: &Project) -> (u32, u32) {
    let (_pw, ph) = settings.resolution.dims();
    let aspect = if project.height > 0 {
        project.width as f64 / project.height as f64
    } else {
        16.0 / 9.0
    };
    let h = ph.max(2);
    let mut w = (h as f64 * aspect).round() as u32;
    w += w & 1; // make even
    (w.max(2), h)
}

/// The absolute path the export writes to. Honours `$QOS_VE_EXPORT_OUT` (a full
/// path, used by tests) or `$QOS_VE_EXPORT_DIR` (a directory), else the system
/// temp dir, always with the settings-derived filename.
pub fn output_path(settings: &ExportSettings, project: &Project) -> std::path::PathBuf {
    if let Some(full) = std::env::var_os("QOS_VE_EXPORT_OUT") {
        return std::path::PathBuf::from(full);
    }
    let dir = std::env::var_os("QOS_VE_EXPORT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(settings.suggested_filename(&project.name))
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
        Some("Export needs the GStreamer build (`--features gst`)")
    }
}

/// Kick off an export. Returns the status the dialog should show next.
///
/// With `--features gst` this runs a real GStreamer pipeline — a raw-RGBA
/// `appsrc` fed the composited timeline frames (the same grade/fade math the
/// preview shows), encoded and muxed to a real file on disk. The default build
/// has no encoder linked, so it is honest — an "unavailable" status, never a
/// faked progress bar or a success for a file that was never written.
pub fn begin_export(project: &Project, settings: &ExportSettings) -> ExportStatus {
    #[cfg(feature = "gst")]
    {
        match gst_pipeline::run(project, settings) {
            Ok(path) => ExportStatus::Done { path },
            Err(e) => ExportStatus::Failed { reason: e },
        }
    }
    #[cfg(not(feature = "gst"))]
    {
        let _ = (project, settings);
        ExportStatus::Failed {
            reason: unavailable_reason()
                .unwrap_or("Export is unavailable")
                .to_string(),
        }
    }
}

#[cfg(feature = "gst")]
mod gst_pipeline {
    use super::*;
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;

    /// Build + run the real export graph: push every composited frame of the
    /// timeline into an `appsrc`, encode + mux to the container, and write the
    /// file. Blocks until the pipeline reaches EOS (or errors), then returns the
    /// written path. Any failure is surfaced verbatim — nothing claims success
    /// unless a real file was produced.
    pub fn run(project: &Project, settings: &ExportSettings) -> Result<String, String> {
        gst::init().map_err(|e| format!("GStreamer init failed: {e}"))?;

        let (w, h) = output_dims(settings, project);
        let fps = settings.frame_rate.fps();
        let out = output_path(settings, project);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let out_str = out.to_string_lossy().into_owned();

        let desc = pipeline_desc(settings, w, h, &out_str);
        let pipeline = gst::parse::launch(&desc)
            .map_err(|e| format!("build export pipeline: {e}"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "export pipeline is not a Pipeline".to_string())?;

        let appsrc = pipeline
            .by_name("qos_src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .ok_or_else(|| "export appsrc not found".to_string())?;
        appsrc.set_format(gst::Format::Time);

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("start export pipeline: {e}"))?;

        // The frames come off the very same GPU compositor the preview
        // canvas draws through — layer stack, grade, effect chain,
        // envelope and transitions — fed by the same media sessions, so
        // the file cannot disagree with what was on screen. Media that
        // has not hydrated falls back to its representative fill rather
        // than failing the render.
        //
        // `Exact` because an export has no interactive deadline to miss
        // and every frame must be the right one; the preview asks the
        // same sources for `Responsive` instead.
        let mut compositor = crate::gpu::HeadlessCompositor::new()
            .map_err(|e| format!("export compositor: {e}"))?;
        let mut sources = crate::gpu::MediaFrameSource::new(project, crate::gpu::Fetch::Exact);

        // Total frames across the timeline duration at the target rate.
        let duration = project.duration().max(1.0 / fps);
        let total = (duration * fps).ceil().max(1.0) as u64;
        let frame_bytes = (w * h * 4) as usize;
        let ns_per_frame = (1_000_000_000.0 / fps) as u64;
        let frame_dur = gst::ClockTime::from_nseconds(ns_per_frame);

        for i in 0..total {
            let t = i as f64 / fps;
            let rgba = compositor.frame(project, t, w, h, &mut sources);
            debug_assert_eq!(rgba.len(), frame_bytes);
            let mut buffer = gst::Buffer::with_size(frame_bytes)
                .map_err(|_| "allocate export frame buffer".to_string())?;
            {
                let bref = buffer.get_mut().expect("fresh buffer is uniquely owned");
                bref.set_pts(gst::ClockTime::from_nseconds(ns_per_frame * i));
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
        }
        appsrc
            .end_of_stream()
            .map_err(|e| format!("end export stream: {e:?}"))?;

        // Wait for EOS (or an error) so the muxer flushes a complete file.
        let bus = pipeline.bus().ok_or_else(|| "export pipeline bus".to_string())?;
        let mut result = Ok(());
        for msg in bus.iter_timed(gst::ClockTime::from_seconds(120)) {
            use gst::MessageView;
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
        result?;

        // Never report success unless a real, non-empty file exists.
        match std::fs::metadata(&out) {
            Ok(m) if m.len() > 0 => Ok(out_str),
            Ok(_) => Err("export produced an empty file".to_string()),
            Err(e) => Err(format!("export wrote no file: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_estimate_matches_the_bitrate_the_encoder_is_given() {
        // The estimate is only honest if it describes the encode that
        // actually runs.
        for container in Container::ALL {
            for resolution in Resolution::ALL {
                let s = ExportSettings { resolution, container, ..Default::default() };
                let d = pipeline_desc(&s, 1920, 1080, "/tmp/o");
                let bps = (resolution.target_mbps() * 1_000_000.0) as u64;
                assert!(
                    d.contains(&format!("={bps} !")),
                    "{container:?} at {} must encode at the quoted {bps} bps: {d}",
                    resolution.label(),
                );
                // 10s at that bitrate, in megabytes, video only.
                let want = resolution.target_mbps() * 10.0 / 8.0;
                assert!(
                    (s.estimated_mb(10.0) - want).abs() < 1e-9,
                    "the estimate quotes exactly the encoded bitrate, nothing more",
                );
            }
        }
    }

    #[test]
    fn the_label_names_the_codec_the_pipeline_produces() {
        // Every choice in the dialog has to name what the user will get.
        for container in Container::ALL {
            let (enc, _, _) = encode_chain(container);
            let label = container.label().to_lowercase();
            let codec = match enc {
                "openh264enc" => "h.264",
                "vp8enc" => "vp8",
                other => panic!("unlabelled encoder {other}"),
            };
            assert!(
                label.contains(codec),
                "{container:?} is labelled {:?} but encodes with {enc}",
                container.label(),
            );
        }
    }

    #[test]
    fn availability_agrees_with_what_export_actually_does() {
        let p = Project::new("t");
        let s = ExportSettings::default();
        match (unavailable_reason(), begin_export(&p, &s)) {
            // No encoder linked: the reason given to the dialog is the same
            // one the attempt fails with.
            (Some(reason), ExportStatus::Failed { reason: got }) => {
                assert_eq!(reason, got, "the dialog and the attempt must say the same thing");
            }
            // An encoder is linked, so nothing may claim otherwise. The
            // empty project has no clips but still renders a black frame.
            (None, status) => assert!(
                !matches!(&status, ExportStatus::Failed { reason } if reason.contains("GStreamer")),
                "a gst build must not report export unavailable, got {status:?}",
            ),
            (Some(reason), other) => {
                panic!("export claims to be unavailable ({reason}) yet returned {other:?}")
            }
        }
    }

    #[test]
    fn size_estimate_scales_with_resolution() {
        let d = 60.0;
        let p720 = ExportSettings { resolution: Resolution::P720, ..Default::default() }
            .estimated_mb(d);
        let p2160 = ExportSettings { resolution: Resolution::P2160, ..Default::default() }
            .estimated_mb(d);
        assert!(p2160 > p720 * 4.0, "4K should be much heavier than 720p");
    }

    #[test]
    fn filename_is_slugged_with_extension() {
        let s = ExportSettings { container: Container::WebM, ..Default::default() };
        assert_eq!(s.suggested_filename("My Doc v3"), "My_Doc_v3.webm");
    }

    #[cfg(not(feature = "gst"))]
    #[test]
    fn default_build_reports_export_unavailable() {
        let p = Project::new("t");
        let s = ExportSettings::default();
        // Without the gst feature no encoder is wired in, so begin_export must
        // be honest — a clear "needs GStreamer" status, never a faked progress
        // bar and never a `Done` for a file that was not written.
        match begin_export(&p, &s) {
            ExportStatus::Failed { reason } => {
                assert!(
                    reason.to_lowercase().contains("gstreamer"),
                    "the failure names the missing GStreamer build, got {reason:?}",
                );
            }
            other => panic!("expected an honest unavailable status, got {other:?}"),
        }
    }

    #[test]
    fn pipeline_desc_wires_the_right_chain_per_container() {
        let mp4 = ExportSettings { container: Container::Mp4, ..Default::default() };
        let d = pipeline_desc(&mp4, 1920, 1080, "/tmp/out.mp4");
        assert!(d.contains("appsrc name=qos_src"), "an appsrc feeds real frames: {d}");
        assert!(d.contains("format=RGBA,width=1920,height=1080"), "caps match the frames: {d}");
        assert!(d.contains("openh264enc"), "H.264 encoder: {d}");
        assert!(
            d.contains("openh264enc bitrate=16000000"),
            "the encoder is told to hit the bitrate the size estimate quotes: {d}",
        );
        assert!(d.contains("h264parse ! mp4mux"), "parsed + MP4-muxed: {d}");
        assert!(d.contains("filesink location=/tmp/out.mp4"), "writes a real file: {d}");

        let webm = ExportSettings { container: Container::WebM, ..Default::default() };
        let dw = pipeline_desc(&webm, 640, 360, "/tmp/o.webm");
        assert!(dw.contains("vp8enc") && dw.contains("webmmux"), "VP8/WebM chain: {dw}");
        assert!(
            dw.contains("vp8enc target-bitrate="),
            "VP8 takes its bitrate under a different property name: {dw}",
        );
        assert!(!dw.contains("h264parse"), "no h264parse for WebM: {dw}");

        let mov = ExportSettings { container: Container::Mov, ..Default::default() };
        assert!(pipeline_desc(&mov, 2, 2, "/tmp/o.mov").contains("qtmux"), "MOV muxes with qtmux");
    }

    #[test]
    fn output_dims_preserve_aspect_and_stay_even() {
        let mut p = Project::new("t");
        p.width = 1440;
        p.height = 1080; // 4:3
        let s = ExportSettings { resolution: Resolution::P720, ..Default::default() };
        let (w, h) = output_dims(&s, &p);
        assert_eq!(h, 720);
        assert_eq!(w, 960, "4:3 at 720p is 960×720");
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }

    // Real export through GStreamer: encodes a short two-frame project to a
    // real file and asserts a valid, non-empty container was written. Only
    // compiled under `--features gst`; the encoder/mux plugins must be present.
    #[cfg(feature = "gst")]
    #[test]
    #[serial_test::serial(video_editor_gst)]
    fn real_export_writes_a_playable_mp4() {
        use crate::model::{MediaItem, MediaKind, Track, TrackKind};
        use uuid::Uuid;
        // A tiny real project: one 0.2s clip so the export loop runs quickly.
        let mut p = Project::new("Export Test");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "c".into(),
            kind: MediaKind::Video,
            duration: 0.2,
            width: 320,
            height: 180,
            fps: 30.0,
            palette: 3,
            path: crate::media::workspace_path("c.mp4"),
            hydration: crate::media::Hydration::Projected,
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track { id: tid, name: "V1".into(), kind: TrackKind::Video, index: 1, muted: false, locked: false, solo: false });
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 0.2;

        let out = std::env::temp_dir().join(format!("qos-ve-export-{}.mp4", std::process::id()));
        // Safety: single-threaded test mutation of process env.
        unsafe { std::env::set_var("QOS_VE_EXPORT_OUT", &out) };
        let s = ExportSettings { resolution: Resolution::P720, container: Container::Mp4, frame_rate: FrameRate::F24 };
        let status = begin_export(&p, &s);
        unsafe { std::env::remove_var("QOS_VE_EXPORT_OUT") };

        match status {
            ExportStatus::Done { path } => {
                assert_eq!(path, out.to_string_lossy());
                let bytes = std::fs::read(&out).expect("export file exists");
                assert!(bytes.len() > 512, "a real encoded file, got {} bytes", bytes.len());
                // A valid ISO-BMFF file has an `ftyp` box type at offset 4.
                assert_eq!(&bytes[4..8], b"ftyp", "the output is a real MP4 container");
            }
            other => panic!("real export must write a file, got {other:?}"),
        }
        std::fs::remove_file(&out).ok();
    }
    // --- real footage, end to end ----------------------------------------

    /// The bundled demo clip, if this checkout has it. Its absence is
    /// reported by the caller rather than silently skipping — a test that
    /// cannot look is not a test that passed.
    #[cfg(feature = "gst")]
    fn demo_clip() -> Option<std::path::PathBuf> {
        // Set by the harness; otherwise walk up to the repository root.
        if let Some(p) = std::env::var_os("QOS_VE_TEST_CLIP") {
            let p = std::path::PathBuf::from(p);
            return p.exists().then_some(p);
        }
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join(".claude/demo-artifacts/qos-demo-57s.mp4");
            if candidate.exists() {
                return Some(candidate);
            }
            if !dir.pop() {
                break;
            }
        }
        None
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
            duration: 57.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            palette: 3,
            path: path.to_string_lossy().into_owned(),
            hydration: crate::media::Hydration::Local,
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
                        sum += 0.2126 * frame.rgba[i] as f64
                            + 0.7152 * frame.rgba[i + 1] as f64
                            + 0.0722 * frame.rgba[i + 2] as f64;
                        count += 1.0;
                    }
                }
                grid[gy * n + gx] = if count > 0.0 { sum / count / 255.0 } else { 0.0 };
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
        // Safety: these gst tests run serially in one process and the env
        // is restored immediately after the call.
        unsafe { std::env::set_var("QOS_VE_EXPORT_OUT", out) };
        let settings = ExportSettings {
            resolution: Resolution::P720,
            container: Container::Mp4,
            frame_rate: FrameRate::F30,
        };
        let status = begin_export(project, &settings);
        unsafe { std::env::remove_var("QOS_VE_EXPORT_OUT") };
        status
    }

    /// The bug this whole path exists to kill: the export used to encode a
    /// synthesized gradient built from a palette table, under *every*
    /// feature combination. Prove the encoded pictures are the source
    /// footage — structurally, frame by frame — and that they are nothing
    /// like the representative fill the palette would have produced.
    #[cfg(feature = "gst")]
    #[test]
    #[serial_test::serial(video_editor_gst)]
    fn the_export_encodes_the_source_footage() {
        let Some(clip) = demo_clip() else {
            panic!(
                "this test needs the bundled demo clip at \
                 .claude/demo-artifacts/qos-demo-57s.mp4 (or $QOS_VE_TEST_CLIP); \
                 it cannot verify the export without real footage",
            );
        };
        // A second and a half of real video, from a second in — far enough
        // past the head that the picture has content.
        let project = project_over(&clip, 8.0, 1.5);
        let out = std::env::temp_dir().join(format!("qos-ve-footage-{}.mp4", std::process::id()));
        match export_to(&project, &out) {
            ExportStatus::Done { .. } => {}
            other => panic!("the export must write a file, got {other:?}"),
        }

        // Three instants across the exported clip, each compared with the
        // source picture it was cut from.
        let mut matched = 0;
        for (timeline_t, source_t) in [(0.2, 8.2), (0.7, 8.7), (1.2, 9.2)] {
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
                        (0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64) / 255.0
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

    /// The grade used to be computed on the palette colour, so it never
    /// touched a decoded pixel. Prove it reaches the encoded footage.
    #[cfg(feature = "gst")]
    #[test]
    #[serial_test::serial(video_editor_gst)]
    fn the_grade_changes_the_encoded_footage() {
        let Some(clip) = demo_clip() else {
            panic!("this test needs the bundled demo clip; see the_export_encodes_the_source_footage");
        };
        let neutral = project_over(&clip, 8.0, 0.6);
        let mut bright = neutral.clone();
        let cid = bright.clips[0].id;
        bright.clip_mut(cid).unwrap().filters =
            crate::model::Filters { brightness: 0.85, contrast: 0.5, saturation: 0.5 };
        let mut grey = neutral.clone();
        grey.clip_mut(cid).unwrap().effects =
            vec![crate::model::ClipEffect { kind: crate::model::EffectKind::Grayscale, amount: 1.0 }];

        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let a = dir.join(format!("qos-ve-grade-neutral-{pid}.mp4"));
        let b = dir.join(format!("qos-ve-grade-bright-{pid}.mp4"));
        let c = dir.join(format!("qos-ve-grade-grey-{pid}.mp4"));
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
                let (r, g, bl) = (px[0] as f64, px[1] as f64, px[2] as f64);
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

    /// Every transition must reach the encoded file as its own picture.
    #[cfg(feature = "gst")]
    #[test]
    #[serial_test::serial(video_editor_gst)]
    fn transitions_reach_the_encoded_file_as_distinct_pictures() {
        use crate::model::Transition;
        let Some(clip) = demo_clip() else {
            panic!("this test needs the bundled demo clip; see the_export_encodes_the_source_footage");
        };
        // Two clips over one source, the incoming one inverted so the two
        // pictures are unmistakably different whatever the footage does.
        // Cutting two moments out of the same clip is not enough on its
        // own: the bundled demo is a screen recording that barely moves,
        // so two instants of it are the same picture, and then every
        // arrangement of them — dissolve, wipe, anything — looks alike.
        let build = |kind: Transition| {
            let mut p = project_over(&clip, 2.0, 1.0);
            let track = p.tracks[0].id;
            let media = p.media[0].id;
            let second = p.add_clip(track, media, 1.0).unwrap();
            {
                let c = p.clip_mut(second).unwrap();
                c.in_point = 40.0;
                c.out_point = 41.0;
                c.transition = kind;
                c.effects = vec![crate::model::ClipEffect {
                    kind: crate::model::EffectKind::Invert,
                    amount: 1.0,
                }];
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
            let out = dir.join(format!("qos-ve-tr-{}-{pid}.mp4", kind.as_str()));
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
            // Skip the feathered middle columns.
            (mean_of(0..5) - mean_of(7..12)).abs()
        };
        let wipe_split = half_split(&grids.iter().find(|(k, _)| *k == Transition::Wipe).unwrap().1);
        let dissolve_split =
            half_split(&grids.iter().find(|(k, _)| *k == Transition::CrossDissolve).unwrap().1);
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
