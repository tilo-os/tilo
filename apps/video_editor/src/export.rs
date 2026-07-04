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

/// Kick off an export. Returns the status the dialog should show next.
///
/// TODO(gst): the `gst` build wires this to a real
/// `gstreamer` pipeline — a `compositor`/`audiomixer` graph fed by one
/// `uridecodebin` per source clip, muxed via `x264enc ! mp4mux` (or the
/// VP9/ProRes equivalents) on a background thread that streams progress
/// back through `Widget::on_external_event`. The default build returns a
/// staged "rendering" status so the UI is exercisable offline.
pub fn begin_export(_project: &Project, _settings: &ExportSettings) -> ExportStatus {
    #[cfg(feature = "gst")]
    {
        // Real pipeline construction lives behind the feature so the
        // default fleet build never needs the gstreamer dev headers.
        match gst_pipeline::run(_project, _settings) {
            Ok(path) => ExportStatus::Done { path },
            Err(e) => ExportStatus::Failed { reason: e },
        }
    }
    #[cfg(not(feature = "gst"))]
    {
        ExportStatus::Rendering { progress: 0.0 }
    }
}

#[cfg(feature = "gst")]
mod gst_pipeline {
    use super::*;
    /// Build + run the GStreamer graph. Stubbed signature kept compiling
    /// only under the feature; the body is where the real `gst::parse`
    /// pipeline string is assembled from the project's clip graph.
    pub fn run(_project: &Project, settings: &ExportSettings) -> Result<String, String> {
        // Intentionally unimplemented in this slice — see the module TODO.
        Err(format!(
            "gst export to {} not yet wired",
            settings.container.label()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn default_build_stages_a_rendering_status() {
        let p = Project::new("t");
        let s = ExportSettings::default();
        // Without the gst feature, begin_export returns a live progress
        // state the dialog can animate.
        match begin_export(&p, &s) {
            ExportStatus::Rendering { progress } => assert_eq!(progress, 0.0),
            other => panic!("unexpected status {other:?}"),
        }
    }
}
