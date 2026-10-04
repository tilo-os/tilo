//! The project as a framework resource, and the editor as its surface.
//!
//! The timeline has its own storage layout and merge semantics — keyed maps
//! of clips, tracks and media, clips patched field by field against the
//! values an edit started from — so it implements the resource contract by
//! hand rather than deriving it: [`VideoProject::read`] is the existing
//! document projection, and the existing [`VideoEditorAction`] vocabulary is
//! the replayable action, under its existing registry type id. The editor
//! keeps its own custom rendering and input; as a
//! [`ResourceSurface`](ui::framework::ResourceSurface) it is presented the
//! project and submits every edit through the framework's dispatcher, which
//! is what gives it permission checks, per-author undo, attributed history
//! and a History mode that draws past versions with the real timeline.

use tilo_actions::{ActionParam, ActionSpec, OperationKind, ParamType};
use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_resource::{ActionDecl, ActionRef, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{VIDEO_EDITOR_ACTION_TYPE, VideoEditorAction, VideoResourceCtx};
use crate::app::{EMPTY_CAPTURE, NARROW_CAPTURE, ShotScenario};
use crate::doc::VideoDoc;
use crate::model::{Marker, Project};

/// The video project resource.
pub struct VideoProject;

impl VideoProject {
    /// Bind or apply the `add-marker` declaration: seconds, then name.
    pub const ADD_MARKER: ActionRef<VideoProject, (f64, String)> =
        ActionRef::new("add-marker", &["at", "name"]);
}

impl Resource for VideoProject {
    const KIND: &'static str = "video_editor.project";
    const LABEL: &'static str = "Project";
    const ACTION_TYPE: &'static str = VIDEO_EDITOR_ACTION_TYPE;

    type State = Project;
    type Action = VideoEditorAction;

    /// The editor's one vocabulary, registered where every other caller
    /// (tests, tools) registers it.
    fn register(registry: &mut tilo_actions::ActionRegistry) {
        crate::actions::register(registry);
    }

    fn read(doc: &Doc) -> Result<Project, ModelError> {
        Ok(VideoDoc::load_project(doc))
    }

    /// Named catalogue actions an agent can invoke without the playhead.
    /// Playhead-bound conveniences (split/trim/mark-at-playhead, mix trim) ride
    /// [`crate::actions::live_playhead_resources`] on the live surface — nested
    /// under the project when standalone, returned alone when hosted.
    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "add-marker",
                "Add a timeline marker",
                vec![
                    ActionParam::required_owned("at", ParamType::Number, "Seconds from the start"),
                    ActionParam::required_owned("name", ParamType::Text, "Marker name"),
                ],
                |args, _now| {
                    let at = args.number("at").ok_or("missing time")?;
                    if !at.is_finite() || at < 0.0 {
                        return Err("a marker needs a time at or after the start".into());
                    }
                    let name = args.text("name").unwrap_or_default();
                    Ok(VideoEditorAction::AddProjectMarker {
                        marker: Marker::new(at, name, 0),
                    })
                },
            )
            .describe("Place a named marker on the project timeline."),
        ]
    }

    /// Tracks, clips, media and markers — the richer palette tree, published
    /// through `tilo_resource::publish` rather than a second hand-built root.
    /// Playhead/mix live extras are stripped so they stay on the surface door.
    fn children(project: &Project) -> Vec<tilo_actions::Resource> {
        let ctx = VideoResourceCtx {
            project: project.clone(),
            playhead: 0.0,
            mix: crate::audio::MixSnapshot::default(),
        };
        crate::actions::project_tree(&ctx)
            .into_iter()
            .next()
            .map(|root| {
                root.children
                    .into_iter()
                    .filter(|child| child.kind != "video.playhead")
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Document actions whose payload is bound from the current project
    /// (rename, frame size, add track/title, …). Playhead-bound conveniences
    /// stay on [`crate::actions::live_playhead_resources`].
    fn bound_actions(project: &Project) -> Vec<ActionSpec> {
        let ctx = VideoResourceCtx {
            project: project.clone(),
            playhead: 0.0,
            mix: crate::audio::MixSnapshot::default(),
        };
        crate::actions::project_tree(&ctx)
            .into_iter()
            .next()
            .map(|root| {
                root.actions
                    .into_iter()
                    .filter(|spec| spec.kind == OperationKind::Document)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Import / open / save / export and other non-document project jobs.
    fn local_operations() -> Vec<ActionSpec> {
        let ctx = VideoResourceCtx::default();
        crate::actions::project_tree(&ctx)
            .into_iter()
            .next()
            .map(|root| {
                root.actions
                    .into_iter()
                    .filter(|spec| spec.kind != OperationKind::Document)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn search(project: &Project) -> Vec<SearchEntry> {
        let clips = project.clips.iter().map(|clip| SearchEntry {
            anchor: Some(crate::actions::clip_resource_id(clip.id)),
            title: project
                .media(clip.media)
                .map_or_else(|| "Clip".to_owned(), |media| media.name.clone()),
            text: format!("{} s", clip.start),
            action: None,
        });
        let markers = project.markers.iter().map(|marker| SearchEntry {
            anchor: Some(crate::actions::marker_resource_id(marker.id)),
            title: marker.name.clone(),
            text: marker.note.clone(),
            action: None,
        });
        clips.chain(markers).collect()
    }
}

/// The editor as a framework app: one surface, the whole window.
pub struct VideoEditor;

impl ResourceApp for VideoEditor {
    type Resource = VideoProject;
    const ID: &'static str = "so.tilo.video-editor";
    const TITLE: &'static str = "Video Editor";
    const ICON: Option<&'static str> = Some("video-editor");
    /// A media file the editor is launched on (`video-editor CLIP`) is
    /// imported into the project and placed on the timeline, as a pick from
    /// the import chooser is (see the surface's `attach`).
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<VideoProject>) -> Element {
        cx.surface(crate::VideoEditorApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        static SCENARIOS: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
            ShotScenario::ALL_NAMES
                .iter()
                .copied()
                .chain([EMPTY_CAPTURE, NARROW_CAPTURE])
                .collect()
        });
        SCENARIOS.as_slice()
    }

    /// The narrow capture is below the bin's collapse breakpoint, so it
    /// shows the overlay-flap layout.
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario == Some(NARROW_CAPTURE) {
            (720, 820)
        } else {
            (1280, 800)
        }
    }

    /// The demo project's sources resolve to the stand-ins bundled with the
    /// crate, so a capture reads local bytes without a workspace mount.
    fn fixture(_fixture: &mut ui::framework::Fixture<VideoProject>) {
        let bundled = crate::media::bundled_sample_media();
        crate::media::serve_sample_media(bundled.exists().then_some(bundled));
    }

    /// Every capture but the blank one opens a project through the same
    /// typed action a script would use: the bundled demo project, or the
    /// composed one; the editor stages the rest (see
    /// `VideoEditorApp::stage_capture`).
    fn scenario(name: Option<&str>) -> Vec<Invoke<VideoProject>> {
        let project = match name {
            Some(EMPTY_CAPTURE) => return Vec::new(),
            Some("composed") => crate::app::composed_project(),
            _ => crate::doc::sample_project(),
        };
        Invoke::apply(&crate::actions::load_project_action(&project)).map_or_else(
            |error| {
                tracing::warn!(%error, "the scenario's project does not encode");
                Vec::new()
            },
            |invoke| vec![invoke],
        )
    }

    fn label(project: &Project) -> String {
        if project.name.is_empty() {
            "Project".to_owned()
        } else {
            project.name.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::VideoEditorApp;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one assertion covers publish ownership and hosted playhead extras"
    )]
    fn the_surface_publishes_only_playhead_extras_beside_the_resource() {
        let published = ui::tilo_resource::publish::<VideoProject>(
            "video.project",
            "Project",
            &Project::new("Untitled"),
        );
        assert_eq!(published.kind, "video_editor.project");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "add-marker",
            "import",
            "open",
            "save",
            "export",
            "rename-project",
            "set-frame-size",
            "apply",
            "group",
        ] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride publish: {ids:?}"
            );
        }
        let add_marker = published
            .actions
            .iter()
            .find(|action| action.id == "add-marker")
            .expect("add-marker");
        assert_eq!(add_marker.kind, OperationKind::Document);
        assert!(
            !published
                .children
                .iter()
                .any(|child| child.kind == "video.playhead"),
            "playhead live extras must not ride Resource children: {:?}",
            published
                .children
                .iter()
                .map(|child| child.kind.as_str())
                .collect::<Vec<_>>()
        );

        let mut app = VideoEditorApp::new();
        let state = ui::State::new();
        let sample = crate::doc::sample_project();
        let clip = sample
            .clips
            .first()
            .expect("sample project carries a clip for playhead extras");
        let at = clip.start + (clip.end() - clip.start) * 0.5;
        app.seed_unpublished_project_for_test(sample.clone());
        app.set_playhead_for_test(at);
        let extras = app.resources(&state.doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "video.project"
                    && resource.kind != "video_editor.project"),
            "the surface must not publish the project root beside the resource: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            extras.len(),
            1,
            "the surface publishes the playhead slice alone"
        );
        assert_eq!(extras[0].kind, "video.playhead");
        assert!(
            extras[0]
                .actions
                .iter()
                .any(|action| action.id == "mark-in" || action.id == "mark-out"),
            "the playhead slice offers its mark conveniences"
        );
        assert!(
            extras[0].children.iter().any(|clip| {
                clip.kind == "video.playhead-clip"
                    && clip
                        .actions
                        .iter()
                        .any(|action| action.id == "split" || action.id == "trim-in")
            }),
            "the playhead slice nests split/trim on playhead-clip rows"
        );
    }
}
