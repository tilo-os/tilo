//! Files as a [`tilo_resource::Resource`]: the person's bookmarked places.
//!
//! The document holds the places (pinned in the sidebar), in order, and every
//! change to them is a [`FilesDocAction`] through the framework dispatcher.
//! How a window shows folders (list or grid, sort, item size, hidden files)
//! is the person's [`FilesPreferences`], which follow them to their other
//! devices and are never seen by collaborators. The folder a window shows,
//! its listing and selection are the window's own; filesystem work is
//! effects on the live rows the surface publishes.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, FileHistory, Fixture, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::storage::{Scope, SettingsSchema};
use ui::tilo_actions::{Action, ActionError, ActionParam, ActionRegistry, ActionSpec, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::doc::PlacesDoc;
use crate::effects::FilesEffect;
use crate::files_app::{
    FileExplorerWidget, folder_palette_operations, published_place_resources,
    published_template_resources,
};

/// A change to the bookmarked places. A place is stored as
/// [`crate::places`] names it, so it means the same folder on every device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilesDocAction {
    /// Bookmark `place` at position `at` (the end when `None`).
    AddBookmark { place: String, at: Option<usize> },
    /// Forget `place`.
    RemoveBookmark { place: String },
    /// Move `place` to position `to`.
    MoveBookmark { place: String, to: usize },
}

impl Action for FilesDocAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::AddBookmark { place, at } => PlacesDoc::add(doc, place, *at)?,
            Self::RemoveBookmark { place } => PlacesDoc::remove(doc, place)?,
            Self::MoveBookmark { place, to } => PlacesDoc::move_to(doc, place, *to)?,
        }
        Ok(())
    }
}

/// The Files document's state: the bookmarked places, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilesState {
    pub bookmarks: Vec<String>,
}

/// How the person likes folders shown, on every device they use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesPreferences {
    /// `list` or `grid`.
    pub view: String,
    /// `name`, `size`, `type` or `modified`.
    pub sort: String,
    /// `ascending` or `descending`.
    pub order: String,
    /// The item size step ([`crate::config::zoom_scale`]).
    pub zoom: i32,
    /// Whether dotfiles are listed.
    pub hidden: bool,
}

impl Default for FilesPreferences {
    fn default() -> Self {
        Self {
            view: "list".into(),
            sort: "name".into(),
            order: "ascending".into(),
            zoom: 0,
            hidden: false,
        }
    }
}

impl SettingsSchema for FilesPreferences {
    const SCOPE: Scope = Scope::Personal;
}

/// The Files resource: the bookmarked places behind every Files window.
pub struct FileExplorer;

impl Resource for FileExplorer {
    const KIND: &'static str = "files.folder";
    const LABEL: &'static str = "Folder";
    const ACTION_TYPE: &'static str = "files.action";

    type State = FilesState;
    type Action = FilesDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<FilesDocAction>(Self::ACTION_TYPE);
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        PlacesDoc::initialize(doc).map_err(|error| ModelError::invalid(error.to_string()))
    }

    fn read(doc: &Doc) -> Result<FilesState, ModelError> {
        Ok(FilesState {
            bookmarks: PlacesDoc::places(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        bookmark_declarations()
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Path-independent folder Views/Effects. Live listing rows and remote
        // accounts stay on the surface; places/templates are children below.
        folder_palette_operations()
    }

    fn children(state: &FilesState) -> Vec<tilo_actions::Resource> {
        let mut children =
            published_place_resources(&crate::files_app::device_places(&state.bookmarks));
        children.extend(published_template_resources());
        children
    }
}

/// The stored place a caller names: a place as stored, or a folder path on
/// this device.
fn named_place(text: &str) -> String {
    if crate::places::is_place(text) {
        return text.to_owned();
    }
    crate::places::place_of(
        std::path::Path::new(text),
        crate::model::user_home().as_deref(),
        &crate::favorites::UserDirs::from_system(),
    )
}

fn place_argument(args: &ui::tilo_actions::ActionArgs) -> Result<String, String> {
    args.text("path")
        .filter(|path| !path.trim().is_empty())
        .map(named_place)
        .ok_or_else(|| "missing folder path".to_owned())
}

fn path_param() -> ActionParam {
    ActionParam::required(
        "path",
        ParamType::Text,
        "Folder path on this device, or a stored place such as xdg:documents",
    )
}

fn bookmark_declarations() -> Vec<ActionDecl<FileExplorer>> {
    vec![
        ActionDecl::new(
            "add-bookmark",
            "Add to Favourites",
            vec![path_param()],
            |args, _| {
                Ok(FilesDocAction::AddBookmark {
                    place: place_argument(args)?,
                    at: None,
                })
            },
        )
        .describe("Pin a folder to the Favourites in the sidebar."),
        ActionDecl::new(
            "remove-bookmark",
            "Remove from Favourites",
            vec![path_param()],
            |args, _| {
                Ok(FilesDocAction::RemoveBookmark {
                    place: place_argument(args)?,
                })
            },
        )
        .describe("Unpin a folder from the Favourites."),
        ActionDecl::new(
            "move-bookmark",
            "Move favourite",
            vec![
                path_param(),
                ActionParam::required("to", ParamType::Integer, "New position, from 0"),
            ],
            |args, _| {
                let to = args
                    .integer("to")
                    .and_then(|to| usize::try_from(to).ok())
                    .ok_or_else(|| "missing or negative position".to_owned())?;
                Ok(FilesDocAction::MoveBookmark {
                    place: place_argument(args)?,
                    to,
                })
            },
        )
        .describe("Reorder the Favourites."),
    ]
}

/// Framework app: the files widget is the surface over [`FileExplorer`].
pub struct FileExplorerShell;

impl ResourceApp for FileExplorerShell {
    type Resource = FileExplorer;
    const ID: &'static str = "so.tilo.file-explorer";
    const TITLE: &'static str = "Files";
    const ICON: Option<&'static str> = Some("file-explorer");
    /// A folder to open (`file-explorer ~/Documents`), or a received share
    /// link (`file-explorer tilo-file://v2/…`) to admit and open.
    const ARGUMENTS: usize = 1;
    /// A received share link on standard input
    /// (`--open-share-stdin`), so its secret stays out of the process list.
    const FLAGS: &'static [&'static str] = &[crate::received_shares::STDIN_FLAG];
    const FILE_HISTORY: Option<FileHistory> = Some(FileHistory {
        title: "Selected file",
        preview: crate::framework_history::preview,
    });

    fn view(cx: &mut ViewCx<FileExplorer>) -> Element {
        let widget = match cx.fixture::<crate::scenarios::Scene>() {
            Some(scene) => FileExplorerWidget::for_scene(&scene),
            None => FileExplorerWidget::new(),
        };
        cx.surface(widget)
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1200, 720)
    }

    /// A capture browses folders written for it, never the machine's own.
    fn fixture(fixture: &mut Fixture<FileExplorer>) {
        crate::scenarios::prepare(fixture);
    }
}

impl ResourceSurface<FileExplorer, FilesEffect> for FileExplorerWidget {
    fn present(&mut self, state: &Rc<FilesState>) {
        self.present_places(&state.bookmarks);
    }

    fn attach(&mut self, cx: SurfaceCx<FileExplorer>) {
        if let Some(source) = cx.file_history() {
            self.show_file_history_in(source);
        }
        if let Some(argument) = cx.launch_arguments().first() {
            self.open_launch_argument(argument);
        }
        if cx.mode().is_live() && cx.launch_flag(crate::received_shares::STDIN_FLAG) {
            match crate::received_shares::read_link(std::io::stdin()) {
                Ok(link) => self.admit_received_share(link),
                Err(error) => self.set_external_status(error),
            }
        }
        self.attach_framework(cx.dispatcher(), cx.settings::<FilesPreferences>());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: FilesEffect,
        _cx: &mut EffectCx<FileExplorer, Self>,
    ) -> EffectAnswer {
        Ok(match self.run_effect(effect)? {
            Some(answer) => Effected::value(answer),
            None => Effected::done(),
        })
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// Pin the folder shown to the Favourites, or unpin it.
    fn bound_declarations(&self) -> Vec<ActionDecl<FileExplorer>> {
        let Some(place) = self.current_place() else {
            return Vec::new();
        };
        let pinned = self.current_is_favourite();
        let id = if pinned {
            "remove-current-bookmark"
        } else {
            "add-current-bookmark"
        };
        let label = if pinned {
            "Remove this folder from Favourites"
        } else {
            "Add this folder to Favourites"
        };
        vec![ActionDecl::new(id, label, Vec::new(), move |_, _| {
            Ok(if pinned {
                FilesDocAction::RemoveBookmark {
                    place: place.clone(),
                }
            } else {
                FilesDocAction::AddBookmark {
                    place: place.clone(),
                    at: None,
                }
            })
        })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use ui::prelude::TestApp;
    use ui::tilo_resource::Invoke;

    /// A new document holds the standard places, published as the resource's
    /// place children; the folder operations ride its publication.
    #[test]
    fn a_new_document_publishes_the_standard_places() {
        let mut doc = AutoCommit::new();
        FileExplorer::initialize(&mut doc).unwrap();
        let state = FileExplorer::read(&doc).unwrap();
        assert_eq!(state.bookmarks, crate::places::DEFAULT_PLACES);

        let published = ui::tilo_resource::publish::<FileExplorer>("files", "Folder", &state);
        assert_eq!(published.kind, "files.folder");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "refresh",
            "paste",
            "go-to",
            "list",
            "grid",
            "add-bookmark",
            "remove-bookmark",
            "move-bookmark",
        ] {
            assert!(ids.contains(&expected), "missing `{expected}`: {ids:?}");
        }
        for gone in [
            "set-view",
            "set-sort",
            "set-zoom",
            "set-hidden",
            "set-bookmarks",
        ] {
            assert!(!ids.contains(&gone), "`{gone}` is a preference: {ids:?}");
        }
        assert!(
            published
                .children
                .iter()
                .filter(|child| child.kind == "files.place")
                .all(|place| place.actions.iter().any(|a| a.id == "open")),
            "each place opens"
        );
    }

    /// Bookmarks are edited place by place, so two people pinning at once
    /// both keep their pin.
    #[test]
    fn bookmark_edits_are_granular() {
        let mut app = TestApp::<FileExplorerShell>::new().unwrap();
        app.invoke(
            Invoke::apply(&FilesDocAction::AddBookmark {
                place: "~/Projects".into(),
                at: None,
            })
            .unwrap(),
        )
        .unwrap();
        app.merge_peer_edit(|peer| {
            FilesDocAction::AddBookmark {
                place: "/mnt/data".into(),
                at: Some(1),
            }
            .apply(peer)
            .unwrap();
        });
        let places = app.state().bookmarks;
        assert!(places.contains(&"~/Projects".to_owned()), "{places:?}");
        assert_eq!(places[1], "/mnt/data");

        app.invoke(
            Invoke::apply(&FilesDocAction::MoveBookmark {
                place: "~/Projects".into(),
                to: 0,
            })
            .unwrap(),
        )
        .unwrap();
        app.invoke(
            Invoke::apply(&FilesDocAction::RemoveBookmark {
                place: "xdg:music".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let places = app.state().bookmarks;
        assert_eq!(places[0], "~/Projects");
        assert!(!places.contains(&"xdg:music".to_owned()));
    }

    /// Pinning a folder in the window is the resource's action: it lands in
    /// the document, undo takes it back, and the sidebar follows.
    #[test]
    fn pinning_a_folder_is_a_bookmark_action() {
        let folder = tempfile::tempdir().unwrap();
        let mut app = TestApp::<FileExplorerShell>::new().unwrap();
        assert_eq!(app.state().bookmarks, crate::places::DEFAULT_PLACES);
        app.with_surface(|files: &mut FileExplorerWidget| {
            files.apply_command(crate::FileCommand::AddFavorite(folder.path().to_path_buf()))
        })
        .unwrap();
        app.settle();
        let place = folder.path().display().to_string();
        assert!(app.state().bookmarks.contains(&place), "{:?}", app.state());
        assert!(app.undo());
        assert!(!app.state().bookmarks.contains(&place));
        let shown = app
            .with_surface(|files: &mut FileExplorerWidget| {
                files.favorites().iter().any(|f| f.path == folder.path())
            })
            .unwrap();
        assert!(!shown, "the sidebar shows the stored places");

        app.invoke(Invoke::apply(&FilesDocAction::AddBookmark { place, at: None }).unwrap())
            .unwrap();
        let shown = app
            .with_surface(|files: &mut FileExplorerWidget| {
                files.favorites().iter().any(|f| f.path == folder.path())
            })
            .unwrap();
        assert!(shown, "an agent's pin reaches the sidebar");
    }

    /// The view preferences are the person's settings, not the document.
    #[test]
    fn view_preferences_are_personal_settings() {
        let mut app = TestApp::<FileExplorerShell>::new().unwrap();
        let before = app.peer_document().get_heads();
        app.with_surface(|files: &mut FileExplorerWidget| {
            files.set_view_mode(crate::model::ViewMode::Grid);
            files.apply_command(crate::FileCommand::ZoomIn);
        })
        .unwrap();
        app.settle();
        assert_eq!(
            app.peer_document().get_heads(),
            before,
            "no document change"
        );
        let stored = app.settings::<FilesPreferences>().get();
        assert_eq!(stored.view, "grid");
        assert_eq!(stored.zoom, 1);
    }

    /// A capture browses the folders written for its scenario and reads
    /// nothing of the machine it runs on; its view state is staged once laid
    /// out.
    #[test]
    fn captures_browse_their_own_scene() {
        let guard = crate::model::fixture_io_guard::DenyAmbientIo::new();
        let mut select = TestApp::<FileExplorerShell>::capture(Some("select")).unwrap();
        assert_eq!(select.harness().inspection().viewport, [1200, 720]);
        let frame = select.harness().inspection_json().unwrap();
        assert!(frame.contains("Holiday plan.md"), "{frame}");
        assert!(frame.contains("Development"), "{frame}");
        assert_eq!(guard.attempts(), 0, "a capture read ambient state");
        drop(guard);

        let mut menu = TestApp::<FileExplorerShell>::capture(Some("context-menu")).unwrap();
        let frame = menu.harness().inspection_json().unwrap();
        assert!(
            frame.contains("Meeting notes"),
            "the scene's templates: {frame}"
        );
    }

    /// A launch naming a folder opens it.
    #[test]
    fn a_launch_opens_the_folder_it_names() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("launched.txt"), b"x").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[directory.path().to_str().unwrap()]).unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("launched.txt"), "{frame}");
    }

    /// Filesystem work reaches the window's handler through the door, as the
    /// palette or an agent invokes it; pinning the folder shown is the
    /// resource's action, bound to that folder.
    #[test]
    fn filesystem_work_is_the_windows_effects() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("draft.txt"), b"x").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        app.effect(&FilesEffect::NewFolder {
            name: "Projects".into(),
        })
        .unwrap();
        assert!(folder.path().join("Projects").is_dir());
        app.effect(&FilesEffect::Rename {
            entry: "draft.txt".into(),
            name: "final.txt".into(),
        })
        .unwrap();
        assert!(folder.path().join("final.txt").exists());
        let moved = app
            .effect(&FilesEffect::Move {
                entries: vec!["final.txt".into()],
                folder: folder.path().join("Projects").display().to_string(),
                create: None,
            })
            .unwrap();
        assert!(
            folder.path().join("Projects/final.txt").exists(),
            "{moved:?}"
        );
        assert!(
            app.effect(&FilesEffect::EmptyTrash).is_err(),
            "Trash is emptied from the Trash folder only"
        );

        let instance = app.harness().component_instance_id().unwrap();
        app.harness()
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: instance,
                    address: ui::tilo_actions::ActionAddress::new(
                        FileExplorer::KIND,
                        "add-current-bookmark",
                    ),
                },
                &serde_json::Map::new(),
            )
            .unwrap();
        app.settle();
        let place = folder.path().display().to_string();
        assert!(app.state().bookmarks.contains(&place), "{:?}", app.state());
    }
}
