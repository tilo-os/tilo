//! Music as a [`tilo_resource::Resource`]: the shared library (playlists,
//! likes, membership) is the state; now-playing presentation is View.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionParam, ActionSpec, EncodedAction, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    MUSIC_ACTION_TYPE, MusicAction, MusicResourceCtx, create_playlist_action, document_children,
    new_playlist_id, open_local_file_spec, register,
};
use crate::app::MusicApp;
use crate::doc;
use crate::model::Library;

/// The music library resource.
pub struct Music;

impl Resource for Music {
    const KIND: &'static str = "music.library";
    const LABEL: &'static str = "Music";
    const ACTION_TYPE: &'static str = MUSIC_ACTION_TYPE;

    type State = Library;
    type Action = MusicAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Library, ModelError> {
        Ok(doc::load_library(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new-playlist",
                "New playlist",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "playlist name…",
                )],
                |args, _| {
                    let name = args
                        .text(tilo_actions::PROMPTED_PARAM)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    let name = if name.is_empty() {
                        "New Playlist".to_owned()
                    } else {
                        name
                    };
                    Ok(create_playlist_action(&new_playlist_id(), &name))
                },
            )
            .describe("Create a new empty playlist."),
        ]
    }

    fn children(state: &Library) -> Vec<tilo_actions::Resource> {
        document_children(&MusicResourceCtx::from_library(state))
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Open-file is a device Effect. Navigation destinations are child
        // `music.view` resources (same tree `provide_resources` publishes).
        vec![open_local_file_spec()]
    }

    fn search(state: &Library) -> Vec<SearchEntry> {
        let tracks = state.tracks.values().map(|track| SearchEntry {
            anchor: Some(crate::actions::track_resource_id(&track.id)),
            title: track.title.clone(),
            text: state.artist_name(&track.artist_id).to_owned(),
            action: None,
        });
        let playlists = state.playlists.iter().map(|playlist| SearchEntry {
            anchor: Some(crate::actions::playlist_resource_id(&playlist.id)),
            title: playlist.name.clone(),
            text: String::new(),
            action: None,
        });
        tracks.chain(playlists).collect()
    }
}

/// Framework app: the music widget is the surface over [`Music`].
pub struct MusicShell;

impl ResourceApp for MusicShell {
    type Resource = Music;
    const ID: &'static str = "so.tilo.music";
    const TITLE: &'static str = "Music";
    const ICON: Option<&'static str> = Some("music");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Music>) -> Element {
        cx.surface(MusicApp::new())
    }

    fn label(state: &Library) -> String {
        if state.tracks.is_empty() && state.playlists.is_empty() {
            "Music".into()
        } else {
            format!(
                "{} tracks",
                state.tracks.len().saturating_add(state.playlists.len())
            )
        }
    }
}

impl ResourceSurface<Music> for MusicApp {
    fn present(&mut self, state: &Rc<Library>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Music>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Music>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Music>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &MusicAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<MusicShell>()
}

pub fn standalone_launch() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots"
            | "--responsive-matrix"
            | "--native-audit"
            | "--headless"
            | "--probe"
            | "--bench"
            | "--live" => return true,
            "--theme" | "--play" | "--viewport" => {
                let _ = args.next();
                return true;
            }
            file if !file.starts_with('-') => return true,
            _ => {}
        }
    }
    false
}
