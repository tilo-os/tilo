//! The `render-timeline` effect: the timeline rendered on this device into a video
//! kept as content, with the render's progress reported to its caller.
//!
//! A person's Export writes a file they chose; this writes nothing of
//! theirs. The video goes into the content store the host lent the window,
//! and the caller gets its address, which an action that takes content
//! (attaching it, saving it to a file) then uses under its own permission.

use ui::framework::content::ContentStore;
use ui::framework::{EffectAnswer, Effected};

use super::VideoEditorApp;
use crate::export::{Container, ExportSettings, Resolution};
use crate::model::Project;

type Cx = ui::framework::EffectCx<Project, VideoEditorApp>;

/// What a render was asked for.
pub(super) struct RenderRequest {
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub format: Option<String>,
    pub size: Option<String>,
}

/// A finished render, kept.
struct Rendered {
    hash: String,
    bytes: u64,
}

impl VideoEditorApp {
    /// Start rendering what `request` names of the stored project.
    pub(super) fn render_effect(&self, request: &RenderRequest, cx: &mut Cx) -> EffectAnswer {
        if let Some(reason) = crate::export::unavailable_reason() {
            return Err(reason.to_owned());
        }
        let content = self
            .content
            .clone()
            .ok_or("this window has no content store to keep a render in")?;
        let settings = settings(request)?;
        let project = ranged(&self.stored, request.start, request.end)?;
        let (start, end) = project.export_range();
        let (width, height) = crate::export::output_dims(&settings, &project);
        let destination = tilo_paths::temp_dir()
            .ok_or("this device has no place to render to")?
            .join(format!(
                "video-editor-render-{}.{}",
                uuid::Uuid::new_v4(),
                settings.container.extension()
            ));
        Ok(cx.thread(
            move |progress| {
                let report = progress.clone();
                let rendered = crate::export::render_to(
                    &project,
                    settings,
                    &destination,
                    progress.cancellation_signal(),
                    move |done, total| report.report("Rendering", Some(done), Some(total)),
                )
                .and_then(|_| keep(&content, &destination));
                crate::files::remove(&destination);
                rendered
            },
            move |_, rendered, _| {
                let Rendered { hash, bytes } = rendered?;
                let address = ui::tilo_actions::content_resource(&hash)
                    .ok_or("the render has no content address")?;
                Ok(Effected::Done(ui::tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "content": address,
                        "start": start,
                        "end": end,
                        "width": width,
                        "height": height,
                        "format": settings.container.extension(),
                        "bytes": bytes,
                    })),
                    resources: vec![address],
                }))
            },
        ))
    }
}

/// The export settings `request` names: its format and size, at 30 frames
/// a second.
fn settings(request: &RenderRequest) -> Result<ExportSettings, String> {
    let mut settings = ExportSettings::default();
    if let Some(format) = &request.format {
        settings.container = Container::ALL
            .into_iter()
            .find(|container| container.extension() == format)
            .ok_or_else(|| format!("no format {format:?}: mp4, webm or mov"))?;
    }
    if let Some(size) = &request.size {
        settings.resolution = Resolution::ALL
            .into_iter()
            .find(|resolution| resolution.label() == size)
            .ok_or_else(|| format!("no size {size:?}: 720p, 1080p or 2160p"))?;
    }
    Ok(settings)
}

/// `project` with its render range set to `start..end`, each defaulting to
/// the range it has.
fn ranged(project: &Project, start: Option<f64>, end: Option<f64>) -> Result<Project, String> {
    if project.clips.is_empty() {
        return Err("the timeline is empty: there is nothing to render".into());
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

fn keep(content: &ContentStore, path: &std::path::Path) -> Result<Rendered, String> {
    let bytes =
        crate::files::read(path).map_err(|error| format!("the render cannot be read: {error}"))?;
    let hash = content
        .put(&bytes)
        .map_err(|error| format!("the render could not be kept: {error}"))?;
    Ok(Rendered {
        hash,
        bytes: bytes.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_checked_against_the_timeline() {
        let project = crate::seed::sample_project();
        let (_, last) = {
            let mut whole = project.clone();
            whole.clear_marks();
            whole.export_range()
        };
        let part = ranged(&project, Some(1.0), Some(2.0)).unwrap();
        assert_eq!(part.export_range(), (1.0, 2.0));
        let tail = ranged(&project, Some(1.0), Some(last + 10.0)).unwrap();
        assert_eq!(tail.export_range(), (1.0, last));
        assert!(ranged(&project, Some(last), None).is_err());
        assert!(ranged(&project, Some(2.0), Some(1.0)).is_err());
        assert!(ranged(&project, Some(f64::NAN), None).is_err());
        assert!(ranged(&Project::new("Empty"), None, None).is_err());
    }

    #[test]
    fn formats_and_sizes_are_named_as_published() {
        let request = |format: &str, size: &str| RenderRequest {
            start: None,
            end: None,
            format: Some(format.into()),
            size: Some(size.into()),
        };
        let chosen = settings(&request("webm", "720p")).unwrap();
        assert_eq!(chosen.container, Container::WebM);
        assert_eq!(chosen.resolution, Resolution::P720);
        assert!(settings(&request("avi", "720p")).is_err());
        assert!(settings(&request("mp4", "480p")).is_err());
    }
}
