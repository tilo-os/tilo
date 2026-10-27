//! An agent's renders themselves: the timeline encoded through
//! [`pipeline_desc`] into a file the caller was handed, one still as PNG,
//! and the warm-up that loads every shader and font a render draws with.
//! The agent host's render helper (`tilo-video-render`, a platform crate)
//! runs them confined; this is only what they draw and encode.

use std::sync::Arc;
use std::time::Duration;

use super::{
    AGENT_ENCODER_MISSING, AGENT_VIDEO_ENCODER, AgentExportSettings, AgentFrameSettings,
    AgentRendered, pipeline_desc,
};
use crate::export::HandedOutput;
use crate::model::Project;

/// How long a still waits for every source's real picture.
const STILL_PATIENCE: Duration = Duration::from_secs(30);

/// Encode `project` as an agent's video into `output`, the file the graph's
/// `sink` (a `GStreamer` sink element on that file) writes, telling
/// `report` each frame written and how many there are.
///
/// # Errors
///
/// Why nothing was rendered: this host lacks [`AGENT_VIDEO_ENCODER`], a
/// placed source cannot be read, or the encode failed (`output` is
/// discarded then).
pub fn render_video(
    project: &Project,
    settings: &AgentExportSettings,
    sink: &str,
    output: &dyn HandedOutput,
    report: impl Fn(u64, u64) + Send + Sync + 'static,
) -> Result<AgentRendered, String> {
    if !media::encode::registry_has(AGENT_VIDEO_ENCODER) {
        return Err(AGENT_ENCODER_MISSING.to_owned());
    }
    let with_audio = crate::audio::has_audio(project);
    let encode = crate::export::Encode {
        width: settings.width,
        height: settings.height,
        fps: f64::from(settings.fps),
        desc: pipeline_desc(settings, sink, with_audio),
        output: crate::export::Output::Handed(output),
    };
    let frames = crate::export::encode_to(project, &encode, Arc::default(), report)?;
    Ok(AgentRendered {
        frames,
        duration: frames as f64 / f64::from(settings.fps),
    })
}

/// Draw the timeline at `frame.at` as PNG, waiting for every source's real
/// picture rather than a stand-in.
///
/// # Errors
///
/// A placed source cannot be read or gave no picture in time, there is no
/// GPU, or the PNG could not be encoded.
pub fn render_still(project: &Project, frame: &AgentFrameSettings) -> Result<Vec<u8>, String> {
    let missing = crate::export::unreadable_sources(project);
    if let Some((name, _)) = missing.first() {
        return Err(format!("{name} cannot be read"));
    }
    let mut compositor = crate::gpu::HeadlessCompositor::new()
        .map_err(|error| format!("the render helper has no GPU: {error}"))?;
    let mut sources = crate::gpu::MediaFrameSource::new(project, crate::gpu::Fetch::Exact)
        .with_timeout(Duration::from_secs(2))
        .strict();
    let asked = ui::time::now();
    let rgba = loop {
        let rgba = compositor.frame(project, frame.at, frame.width, frame.height, &mut sources);
        if sources.missing_pictures().is_empty() {
            break rgba;
        }
        if ui::time::now().saturating_duration_since(asked) >= STILL_PATIENCE {
            return Err("the footage under that time gave no picture".to_owned());
        }
    };
    ::thumbnail::encode_png(&::thumbnail::RgbaThumb {
        rgba,
        width: frame.width,
        height: frame.height,
    })
    .ok_or_else(|| "the still could not be encoded as PNG".to_owned())
}

/// Draw a title over a picture decoded as YUV at `width` x `height`, so the
/// GPU device, every pipeline the compositor, the YUV converter and the
/// title raster build, and the fonts, are made: a helper does this before
/// it confines itself, after which it loads no code, compiles no shader and
/// opens no font.
///
/// # Errors
///
/// There is no GPU.
pub fn warm_up(width: u32, height: u32) -> Result<(), String> {
    let mut compositor = crate::gpu::HeadlessCompositor::new()
        .map_err(|error| format!("the render helper has no GPU: {error}"))?;
    let mut project = Project::new("warm-up");
    project.set_frame_size(width, height);
    let track = crate::model::Track {
        id: uuid::Uuid::from_u128(1),
        kind: crate::model::TrackKind::Video,
        index: 1,
        ..Default::default()
    };
    let below = crate::model::Track {
        id: uuid::Uuid::from_u128(4),
        kind: crate::model::TrackKind::Video,
        ..Default::default()
    };
    let title = crate::model::MediaItem {
        id: uuid::Uuid::from_u128(2),
        kind: crate::model::MediaKind::Text,
        duration: 1.0,
        text: Some(crate::model::TextSource::new("Warm-up")),
        ..Default::default()
    };
    let footage = crate::model::MediaItem {
        id: uuid::Uuid::from_u128(5),
        kind: crate::model::MediaKind::Video,
        duration: 1.0,
        width: 16,
        height: 16,
        ..Default::default()
    };
    for (id, track, media) in [(3, track.id, title.id), (6, below.id, footage.id)] {
        project.clips.push(crate::model::Clip {
            id: uuid::Uuid::from_u128(id),
            track,
            media,
            out_point: 1.0,
            ..Default::default()
        });
    }
    project.tracks.extend([track, below]);
    project.media.extend([title, footage]);
    let _ = compositor.frame(&project, 0.5, width, height, &mut PlanarWarmUp);
    Ok(())
}

/// Every picture a small mid-grey NV12 frame converted on the GPU, as
/// decoded footage is: drawing one builds the YUV converter's pipeline.
struct PlanarWarmUp;

impl crate::gpu::FrameSource for PlanarWarmUp {
    fn resolve(
        &mut self,
        device: &vello::wgpu::Device,
        queue: &vello::wgpu::Queue,
        _media: crate::model::MediaId,
        _source_time: f64,
    ) -> Option<crate::gpu::SourceTexture> {
        const SIDE: u32 = 16;
        let plane = |value: u8, width: u32, height: u32, stride: u32| media::PlaneData {
            bytes: Arc::new(vec![value; (stride * height) as usize]),
            stride,
            width,
            height,
        };
        let image = media::PlanarImage {
            format: media::PlanarFormat::Nv12,
            planes: vec![
                plane(128, SIDE, SIDE, SIDE),
                plane(128, SIDE / 2, SIDE / 2, SIDE),
            ],
            colorimetry: media::Colorimetry::BT709_LIMITED,
        };
        let planes = ui::gpu_video::upload_planar(device, queue, &image, SIDE, SIDE)?;
        let mut converter =
            ui::gpu_video::YuvConverter::new(device, vello::wgpu::TextureFormat::Rgba8Unorm);
        let texture = converter.convert(device, queue, &planes).clone();
        let view = Arc::new(texture.create_view(&vello::wgpu::TextureViewDescriptor::default()));
        Some(crate::gpu::SourceTexture {
            texture: Arc::new(texture),
            view,
            width: SIDE,
            height: SIDE,
        })
    }
}
