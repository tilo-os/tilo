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

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};
use ui::framework::{Dispatcher, ResourceApp, ViewCx};
use ui::tilo_resource::{ActionDecl, ActionRef, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{VIDEO_EDITOR_ACTION_TYPE, VideoEditorAction, VideoResourceCtx};
use crate::app::{EMPTY_CAPTURE, IMPORT_CAPTURE, ShotScenario};
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
        });
        let markers = project.markers.iter().map(|marker| SearchEntry {
            anchor: Some(crate::actions::marker_resource_id(marker.id)),
            title: marker.name.clone(),
            text: marker.note.clone(),
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
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<VideoProject>) -> Element {
        cx.surface(crate::VideoEditorApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        ShotScenario::ALL_NAMES
    }

    /// Every capture but the blank one opens the bundled demo project,
    /// through the same typed action a script would use; the editor stages
    /// the rest (see `VideoEditorApp::stage_capture`).
    fn scenario(name: Option<&str>) -> Vec<Invoke<VideoProject>> {
        if name.is_some_and(|name| name == EMPTY_CAPTURE || name.starts_with(IMPORT_CAPTURE)) {
            return Vec::new();
        }
        Invoke::apply(&crate::actions::load_project_action(
            &crate::doc::sample_project(),
        ))
        .map_or_else(
            |error| {
                tracing::warn!(%error, "the demo project does not encode");
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

/// Where the editor's edits go: the framework's dispatcher when the editor is
/// a hosted surface, or the toolkit's local action host when it runs as a
/// standalone widget (its unit tests and benchmarks).
#[derive(Default)]
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<VideoProject>>,
    last: Option<EncodedAction>,
    /// An edit was refused before reaching the host; the optimistic local
    /// project must be re-read from the stored one.
    stale: bool,
    /// Test override for [`Self::is_hosted`] without a real dispatcher.
    #[cfg(test)]
    hosted_override: Option<bool>,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<VideoProject>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        #[cfg(test)]
        if let Some(hosted) = self.hosted_override {
            return hosted;
        }
        self.framework.is_some()
    }

    /// Mark this surface as framework-hosted so
    /// [`PublishesResources`](ui::resources::PublishesResources) keeps only
    /// playhead live extras beside the published project door.
    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_hosted_for_test(&mut self, hosted: bool) {
        self.hosted_override = Some(hosted);
    }

    /// Submit one edit. Hosted, the host applies it after this event with
    /// the participant's permissions, one undo step and attribution;
    /// standalone, it applies now through the local executor.
    pub(crate) fn dispatch(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &VideoEditorAction,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                self.stale |= dispatcher.read_only();
                dispatcher.apply(action);
            }
            None => self.local.dispatch(doc, type_id, action),
        }
    }

    /// Make every edit until the returned guard drops one host action — one
    /// undo step, one version — as one event already is on the standalone
    /// path. Hosted surfaces hold it for each input event, so a gesture that
    /// dispatches several edits (a linked split, a multi-clip delete, a
    /// paste) undoes in one step.
    pub(crate) fn gesture(&self) -> Option<ui::framework::DispatchGroup<VideoProject>> {
        self.framework.as_ref().map(Dispatcher::begin_group)
    }

    pub(crate) fn take_stale(&mut self) -> bool {
        std::mem::take(&mut self.stale)
    }

    /// The standalone undo boundary. A hosted edit is bracketed by the host.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        self.local.take_committed()
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::VideoEditorApp;
    use std::rc::Rc;
    use ui::framework::ResourceSurface;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one assertion covers publish ownership and hosted playhead extras"
    )]
    fn hosted_resources_keep_playhead_extras_without_project_root() {
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
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        assert!(
            !app.is_framework_hosted(),
            "standalone stays on the full action tree"
        );

        let sample = crate::doc::sample_project();
        let clip = sample
            .clips
            .first()
            .expect("sample project carries a clip for playhead extras");
        let at = clip.start + (clip.end() - clip.start) * 0.5;
        // Seed after sync so a blank-document reload cannot wipe the fixture.
        app.seed_unpublished_project_for_test(sample);
        app.set_playhead_for_test(at);

        let standalone = app.resources(&state.doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "video_editor.project");
        let standalone_playhead = standalone[0]
            .children
            .iter()
            .find(|child| child.kind == "video.playhead")
            .expect("standalone nests playhead conveniences under the project");
        assert!(
            standalone_playhead
                .actions
                .iter()
                .any(|action| action.id == "mark-in"),
            "standalone playhead slice offers mark-in"
        );
        assert!(
            standalone_playhead.children.iter().any(|clip| {
                clip.kind == "video.playhead-clip"
                    && clip
                        .actions
                        .iter()
                        .any(|action| action.id == "split" || action.id == "trim-in")
            }),
            "standalone playhead nests split/trim on playhead-clip rows"
        );

        fn find_clip(resources: &[tilo_actions::Resource]) -> Option<String> {
            for resource in resources {
                if resource.kind == "video.clip" {
                    return Some(resource.id.as_str().to_owned());
                }
                if let Some(found) = find_clip(&resource.children) {
                    return Some(found);
                }
            }
            None
        }
        app.set_framework_hosted(true);
        assert!(app.is_framework_hosted());
        assert!(ui::resources::resource(&app, &state.doc, "video.project").is_none());
        let clip_id = find_clip(&app.publish_resources(&state.doc)).expect("sample clip");
        assert!(
            ui::resources::resource(&app, &state.doc, &clip_id).is_some(),
            "a hosted clip stays the published row"
        );
        let extras = app.resources(&state.doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "video.project"
                    && resource.kind != "video_editor.project"),
            "hosted must not republish the project root beside ResourceRoot: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(extras.len(), 1, "hosted returns the playhead slice alone");
        assert_eq!(extras[0].kind, "video.playhead");
        assert_eq!(
            extras[0].id, standalone_playhead.id,
            "hosted playhead tree matches the standalone nested slice"
        );
        assert!(
            extras[0]
                .actions
                .iter()
                .any(|action| action.id == "mark-in" || action.id == "mark-out"),
            "hosted keeps playhead mark conveniences"
        );
        assert_eq!(
            extras[0].children.len(),
            standalone_playhead.children.len(),
            "hosted keeps the same clip split/trim rows"
        );
    }

    #[test]
    fn present_empty_does_not_wipe_unpublished_local_project() {
        let mut app = VideoEditorApp::new();
        let mut local = Project::new("local-only");
        local.media.push(crate::model::MediaItem {
            id: uuid::Uuid::new_v4(),
            name: "Not published yet".into(),
            ..Default::default()
        });
        app.seed_unpublished_project_for_test(local);
        let empty = Rc::new(Project::new(""));
        ResourceSurface::<VideoProject>::present(&mut app, &empty);
        assert_eq!(
            app.local_media_names_for_test(),
            vec!["Not published yet".to_owned()],
            "empty present must not wipe an unpublished local project"
        );
    }
}
