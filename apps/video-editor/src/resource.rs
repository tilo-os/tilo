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
use tilo_actions::{ActionParam, EncodedAction, ParamType};
use ui::framework::{Dispatcher, ResourceApp, ViewCx};
use ui::tilo_resource::{ActionDecl, ActionRef, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{VIDEO_EDITOR_ACTION_TYPE, VideoEditorAction};
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

    fn read(doc: &Doc) -> Result<Project, ModelError> {
        Ok(VideoDoc::load_project(doc))
    }

    /// View-independent named actions. The editor publishes its richer tree
    /// (split at the playhead, trim to the playhead, per-clip grades) itself,
    /// because those are bound to where its user is.
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
/// standalone widget (its unit tests and legacy captures).
#[derive(Default)]
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<VideoProject>>,
    last: Option<EncodedAction>,
    /// An edit was refused before reaching the host; the optimistic local
    /// project must be re-read from the stored one.
    stale: bool,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<VideoProject>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
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

    pub(crate) fn take_stale(&mut self) -> bool {
        std::mem::take(&mut self.stale)
    }

    /// The standalone undo boundary. A hosted edit is bracketed by the host.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        self.local.take_committed()
    }

    #[cfg(test)]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}
