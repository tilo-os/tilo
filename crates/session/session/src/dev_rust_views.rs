//! A Rust app's resources and views in a development session, so a shell draws them live.
//!
//! Built only with `dev-rust-views` (a shot of a Rust app's view on the canvas): the sample
//! catalogue of Tube's channels and videos, each a resource of the app `tube`, published as the
//! session publishes any app's resources, and the views `#[view]` declared in the crates
//! registered with the shell's runtime, so a place that shows a channel draws that app's own
//! `page` (one-app-model.md §2). A person's session, and every production image, carries none
//! of it.

use serde_json::Value as Json;
use tilo_actions::{ApiError, ResourceSpec};
use tilo_service::{Service, Started};

/// Tube's sample channels and videos, as the session publishes them: each a resource of its
/// app, a channel through its `page` view.
pub struct RustApps {
    model: tube::model::TubeModel,
}

impl RustApps {
    /// The app's sample catalogue, and the views this crate links registered with the shell.
    #[must_use]
    pub fn new() -> Self {
        Self {
            model: tube::sample::model(),
        }
    }
}

impl Default for RustApps {
    fn default() -> Self {
        Self::new()
    }
}

/// The development session's sample rows, as declared types: Tube's channels and videos,
/// exactly as that app declares them.
pub struct SampleChannel;

impl tilo_resource::words::RowType for SampleChannel {
    const KIND: &'static str = "tube.channel";
    const LABEL: &'static str = "Channel";
}

pub struct SampleVideo;

impl tilo_resource::words::RowType for SampleVideo {
    const KIND: &'static str = "tube.video";
    const LABEL: &'static str = "Video";
}

impl Service for RustApps {
    fn resources(&self) -> Vec<ResourceSpec> {
        let channels = self.model.channels.iter().map(|channel| {
            let mut spec = tilo_resource::typed_resource::<SampleChannel>(
                format!("tube.channel:{}", channel.id),
                channel.name.clone(),
            )
            // Tube's own words for a channel, as its window publishes it.
            .with_type(tilo_actions::ResourceType {
                noun: tilo_actions::Noun::new("channel", "channels"),
                app: tilo_actions::Noun::new("Tube", "Tube"),
                root: false,
            })
            .with_view(tilo_actions::ResourceViewSpec::new(
                "page",
                channel.name.clone(),
                "open",
            ));
            spec.app_id = Some("tube".to_owned());
            spec
        });
        let videos = self.model.videos.iter().map(|video| {
            let mut spec = tilo_resource::typed_resource::<SampleVideo>(
                format!("tube.video:{}", video.id),
                video.title.clone(),
            )
            // Tube's own words for a video, as its window publishes it.
            .with_type(tilo_actions::ResourceType {
                noun: tilo_actions::Noun::new("video", "videos"),
                app: tilo_actions::Noun::new("Tube", "Tube"),
                root: false,
            });
            spec.app_id = Some("tube".to_owned());
            spec
        });
        channels.chain(videos).collect()
    }

    fn read(&self, resource: &str) -> Option<Json> {
        if let Some(id) = resource.strip_prefix("tube.channel:") {
            return tube::views::Channel::from_model(&self.model, id)
                .and_then(|channel| serde_json::to_value(channel).ok());
        }
        if let Some(id) = resource.strip_prefix("tube.video:") {
            return self
                .model
                .videos
                .iter()
                .find(|video| video.id == id)
                .and_then(|video| serde_json::to_value(video).ok());
        }
        None
    }

    fn invoke(
        &mut self,
        _resource: &str,
        _action: &str,
        _arguments: &serde_json::Map<String, Json>,
    ) -> Result<Started, ApiError> {
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}
