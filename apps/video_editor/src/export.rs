//! Export settings + the (gated) render pipeline.
//!
//! Real encode/decode runs through `GStreamer`, which is only linked when
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
    pub fn label(self) -> &'static str {
        match self {
            Container::Mp4 => "MP4 / H.264",
            Container::WebM => "WebM / VP9",
            Container::Mov => "MOV / ProRes",
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
    /// video bitrate only plus a flat audio allowance. Shown live in the
    /// dialog as the user flips presets.
    pub fn estimated_mb(&self, duration: f64) -> f64 {
        let video_mbits = self.resolution.target_mbps() * duration;
        let audio_mbits = 0.256 * duration; // ~256 kbps stereo
        (video_mbits + audio_mbits) / 8.0
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

/// The `GStreamer` element chain that encodes the export, per container: the
/// (encoder, optional parser, muxer) triple. Kept pure + shared so the pipeline
/// **construction** is verifiable without a gstreamer stack (see tests).
///
/// The encoders/muxers are the real elements the runner plugs:
///   * MP4 / MOV — `openh264enc ! h264parse ! (mp4mux|qtmux)` (H.264).
///   * `WebM` — `vp8enc ! webmmux` (VP8, `WebM`'s baseline codec).
pub fn encode_chain(container: Container) -> (&'static str, Option<&'static str>, &'static str) {
    match container {
        Container::Mp4 => ("openh264enc", Some("h264parse"), "mp4mux"),
        Container::Mov => ("openh264enc", Some("h264parse"), "qtmux"),
        Container::WebM => ("vp8enc", None, "webmmux"),
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
    format!(
        "appsrc name=qos_src is-live=false format=time \
         caps=video/x-raw,format=RGBA,width={w},height={h},framerate={fps}/1 \
         ! videoconvert ! {enc} ! {parse}{mux} ! filesink location={out}"
    )
}

/// Output dimensions for the export: the resolution preset's height, with the
/// width scaled to the project's aspect ratio and rounded to an even number
/// (H.264 requires even dimensions).
pub fn output_dims(settings: &ExportSettings, project: &Project) -> (u32, u32) {
    let (_pw, ph) = settings.resolution.dims();
    let aspect = if project.height > 0 {
        f64::from(project.width) / f64::from(project.height)
    } else {
        16.0 / 9.0
    };
    let h = ph.max(2);
    let mut w = (f64::from(h) * aspect).round() as u32;
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
    let dir = std::env::var_os("QOS_VE_EXPORT_DIR").map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    dir.join(settings.suggested_filename(&project.name))
}

/// Kick off an export. Returns the status the dialog should show next.
///
/// With `--features gst` this runs a real `GStreamer` pipeline — a raw-RGBA
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
            reason: "Export needs the GStreamer build (`--features gst`)".to_string(),
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

        // Total frames across the timeline duration at the target rate.
        let duration = project.duration().max(1.0 / fps);
        let total = (duration * fps).ceil().max(1.0) as u64;
        let frame_bytes = (w * h * 4) as usize;
        let ns_per_frame = (1_000_000_000.0 / fps) as u64;
        let frame_dur = gst::ClockTime::from_nseconds(ns_per_frame);

        for i in 0..total {
            let t = i as f64 / fps;
            let rgba = crate::preview::render_rgba(project, t, w, h);
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
        let bus = pipeline
            .bus()
            .ok_or_else(|| "export pipeline bus".to_string())?;
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
    fn size_estimate_scales_with_resolution() {
        let d = 60.0;
        let p720 = ExportSettings {
            resolution: Resolution::P720,
            ..Default::default()
        }
        .estimated_mb(d);
        let p2160 = ExportSettings {
            resolution: Resolution::P2160,
            ..Default::default()
        }
        .estimated_mb(d);
        assert!(p2160 > p720 * 4.0, "4K should be much heavier than 720p");
    }

    #[test]
    fn filename_is_slugged_with_extension() {
        let s = ExportSettings {
            container: Container::WebM,
            ..Default::default()
        };
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
        let mp4 = ExportSettings {
            container: Container::Mp4,
            ..Default::default()
        };
        let d = pipeline_desc(&mp4, 1920, 1080, "/tmp/out.mp4");
        assert!(
            d.contains("appsrc name=qos_src"),
            "an appsrc feeds real frames: {d}"
        );
        assert!(
            d.contains("format=RGBA,width=1920,height=1080"),
            "caps match the frames: {d}"
        );
        assert!(d.contains("openh264enc"), "H.264 encoder: {d}");
        assert!(d.contains("h264parse ! mp4mux"), "parsed + MP4-muxed: {d}");
        assert!(
            d.contains("filesink location=/tmp/out.mp4"),
            "writes a real file: {d}"
        );

        let webm = ExportSettings {
            container: Container::WebM,
            ..Default::default()
        };
        let dw = pipeline_desc(&webm, 640, 360, "/tmp/o.webm");
        assert!(
            dw.contains("vp8enc") && dw.contains("webmmux"),
            "VP8/WebM chain: {dw}"
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

    #[test]
    fn output_dims_preserve_aspect_and_stay_even() {
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

    // Real export through GStreamer: encodes a short two-frame project to a
    // real file and asserts a valid, non-empty container was written. Only
    // compiled under `--features gst`; the encoder/mux plugins must be present.
    #[cfg(feature = "gst")]
    #[test]
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
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
        });
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 0.2;

        let out = std::env::temp_dir().join(format!("qos-ve-export-{}.mp4", std::process::id()));
        // Safety: single-threaded test mutation of process env.
        unsafe { std::env::set_var("QOS_VE_EXPORT_OUT", &out) };
        let s = ExportSettings {
            resolution: Resolution::P720,
            container: Container::Mp4,
            frame_rate: FrameRate::F24,
        };
        let status = begin_export(&p, &s);
        unsafe { std::env::remove_var("QOS_VE_EXPORT_OUT") };

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
}
