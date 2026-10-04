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
    EffectAnswer, EffectCx, EffectStatus, Effected, FileHistory, Fixture, LiveRow, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::storage::{Scope, SettingsSchema};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, EncodedAction, ParamType,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::doc::{OpsDoc, PlacesDoc};
use crate::effects::FilesEffect;
use crate::file_ops::{FileOp, Outcome, QueuedOp};
use crate::files_app::{
    FileExplorerWidget, Ran, published_place_resources, published_template_resources,
};

/// A change to the Files document: the bookmarked places (stored as
/// [`crate::places`] names them, so they mean the same folder on every
/// device) and the filesystem operations queued from Files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilesDocAction {
    /// Bookmark `place` at position `at` (the end when `None`).
    AddBookmark { place: String, at: Option<usize> },
    /// Forget `place`.
    RemoveBookmark { place: String },
    /// Move `place` to position `to`.
    MoveBookmark { place: String, to: usize },
    /// Queue the filesystem change `op` for `device` to carry out. Undoing
    /// it queues the revert of the operation.
    QueueFileOp {
        id: String,
        device: String,
        op: FileOp,
    },
    /// The device carried out the operation `id`: record what it changed.
    SettleFileOp { id: String, outcome: Outcome },
}

impl Action for FilesDocAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::AddBookmark { place, at } => PlacesDoc::add(doc, place, *at)?,
            Self::RemoveBookmark { place } => PlacesDoc::remove(doc, place)?,
            Self::MoveBookmark { place, to } => PlacesDoc::move_to(doc, place, *to)?,
            Self::QueueFileOp { id, device, op } => OpsDoc::queue(doc, id, device, op)?,
            Self::SettleFileOp { id, outcome } => OpsDoc::settle(doc, id, outcome)?,
        }
        Ok(())
    }

    /// Undoing a queued filesystem change takes it back on the filesystem:
    /// the revert of the operation this action queues, queued the same way.
    fn inverse_before(&self, doc: &ui::tilo_actions::Doc) -> Option<Self> {
        let Self::QueueFileOp { id, device, .. } = self else {
            return None;
        };
        let queued = OpsDoc::fresh_id(doc, id);
        Some(Self::QueueFileOp {
            id: format!("{queued}-undo"),
            device: device.clone(),
            op: FileOp::Revert { op: queued },
        })
    }

    fn inverse_encoded(&self, doc: &ui::tilo_actions::Doc) -> Option<EncodedAction> {
        EncodedAction::encode(FileExplorer::ACTION_TYPE, &self.inverse_before(doc)?).ok()
    }

    /// What a device reports it did is content from outside: it syncs, but
    /// no undo takes it back (undoing the operation does).
    fn change_kind(&self) -> ui::tilo_actions::ChangeKind {
        match self {
            Self::SettleFileOp { .. } => ui::tilo_actions::ChangeKind::External,
            _ => ui::tilo_actions::ChangeKind::Content,
        }
    }
}

/// The Files document's state: the bookmarked places, in order, and the
/// filesystem operations queued from Files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilesState {
    pub bookmarks: Vec<String>,
    pub ops: Vec<QueuedOp>,
}

impl FilesState {
    /// The operations `device` still has to carry out, each with the outcome
    /// of the operation it reverts (a revert waits until that one is
    /// carried out).
    pub fn pending_for(&self, device: &str) -> Vec<(String, (QueuedOp, Option<Outcome>))> {
        self.ops
            .iter()
            .filter(|op| op.device == device && op.outcome.is_none())
            .filter_map(|op| {
                let reverting = match &op.op {
                    FileOp::Revert { op: target } => {
                        match self.ops.iter().find(|queued| queued.id == *target) {
                            Some(queued) => Some(queued.outcome.clone()?),
                            None => None,
                        }
                    }
                    _ => None,
                };
                Some((op.id.clone(), (op.clone(), reverting)))
            })
            .collect()
    }
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
            ops: OpsDoc::ops(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        bookmark_declarations()
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

/// A row the window publishes, as the framework carries it.
fn live_row(resource: tilo_actions::Resource) -> LiveRow {
    let mut row = LiveRow::new(resource.id.as_str(), resource.kind.as_str(), resource.label);
    if let Some(presentation) = resource.presentation {
        row = row.presentation(presentation);
    }
    for action in resource.actions {
        row = row.operation(action);
    }
    for child in resource.children {
        row = row.row(live_row(child));
    }
    row
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
        // Mounted in the shell, Files starts where the shell says and opens
        // documents through the shell's services; in its own process, at
        // the person's home.
        let lent = cx.take_lent::<crate::StartupInputs>();
        let services = cx.take_lent::<crate::files_app::HostedFileServices>();
        let widget = match (cx.fixture::<crate::scenarios::Scene>(), lent) {
            (Some(scene), _) => FileExplorerWidget::for_scene(&scene),
            (None, Some(inputs)) => match services {
                Some(services) => {
                    FileExplorerWidget::with_startup_inputs_and_services(inputs, services)
                }
                None => FileExplorerWidget::with_startup_inputs(inputs),
            },
            (None, None) => FileExplorerWidget::new(),
        };
        cx.surface(widget)
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::scenarios()
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
        self.follow_ops(&state.ops);
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
        // Each device carries out the filesystem changes queued for it, and
        // records what it changed.
        let ops = cx.device().map(|device| {
            let device = device.to_owned();
            cx.drain(
                move |state: &FilesState| state.pending_for(&device),
                |(queued, reverting): (QueuedOp, Option<Outcome>), cx| {
                    let outcome = crate::file_ops::execute(&queued.op, reverting.as_ref());
                    cx.apply(&FilesDocAction::SettleFileOp {
                        id: queued.id,
                        outcome,
                    });
                    Ok(Effected::done())
                },
            )
        });
        self.attach_framework(
            cx.dispatcher(),
            cx.settings::<FilesPreferences>(),
            cx.device().map(str::to_owned),
            ops,
        );
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: FilesEffect,
        cx: &mut EffectCx<FileExplorer, Self>,
    ) -> EffectAnswer {
        Ok(match self.run_effect(effect)? {
            Ran::Done(Some(answer)) => Effected::value(answer),
            Ran::Done(None) => Effected::done(),
            Ran::Pending => {
                self.await_effect(cx.reply());
                Effected::Pending
            }
        })
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// Pin the folder shown to the Favourites, or unpin it.
    fn bound_declarations(&self) -> Vec<ActionDecl<FileExplorer>> {
        let Some(place) = self.current_place() else {
            return self.file_op_declarations();
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
        let mut declarations = vec![ActionDecl::new(id, label, Vec::new(), move |_, _| {
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
        })];
        declarations.extend(self.file_op_declarations());
        declarations
    }

    /// The listing's entries, remote accounts, nearby transfers and a
    /// failed listing's retry.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.published_rows().into_iter().map(live_row).collect()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.published_row(id).map(live_row)
    }

    /// Rename, move and trash the entry a row shows, through the queue.
    fn row_declarations(&self, row: &str) -> Vec<ActionDecl<FileExplorer>> {
        self.entry_declarations(row)
    }

    /// The places, the mount's collaborative records and the remote file
    /// accounts: what the explorer holds, not the document.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.search_entries_shown()
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
        for expected in ["add-bookmark", "remove-bookmark", "move-bookmark"] {
            assert!(ids.contains(&expected), "missing `{expected}`: {ids:?}");
        }
        let effects: Vec<String> = <FilesEffect as ui::tilo_resource::Effect>::specs()
            .into_iter()
            .map(|spec| spec.id)
            .collect();
        for expected in [
            "refresh",
            "paste",
            "go-to",
            "open",
            "list",
            "grid",
            "sort-name",
        ] {
            assert!(
                effects.iter().any(|id| id == expected),
                "the window's `{expected}` is an effect: {effects:?}"
            );
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

    /// Invoke the action `id` of the published resource `on` with `args`
    /// through the door, as an agent does, and let the device carry out
    /// what it queued.
    fn act_on(app: &mut TestApp<FileExplorerShell>, on: &str, id: &str, args: &serde_json::Value) {
        let instance = app.harness().component_instance_id().unwrap();
        app.harness()
            .invoke_bound_action_confirmed(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: instance,
                    address: ui::tilo_actions::ActionAddress::new(on, id),
                },
                args.as_object().unwrap(),
            )
            .unwrap();
        app.settle();
    }

    /// Copying and pasting are queued and undoable like every filesystem
    /// change: undo moves a copy to Trash and a cut-and-paste back. A row
    /// offers rename, move and trash on its entry; undoing the trash puts
    /// the file back.
    #[test]
    fn copies_and_row_edits_are_undoable() {
        // Inside the home, so Trash is the home's and its entries are known.
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap();
        let folder = tempfile::tempdir_in(&home).unwrap();
        std::fs::create_dir(folder.path().join("Projects")).unwrap();
        std::fs::write(folder.path().join("notes.txt"), b"n").unwrap();
        std::fs::write(folder.path().join("plan.txt"), b"p").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        let row = |name: &str| crate::search_provider::entry_resource_id(&folder.path().join(name));
        let projects = folder.path().join("Projects").display().to_string();

        app.effect_on(
            &row("notes.txt"),
            &FilesEffect::Copy {
                entry: row("notes.txt"),
            },
        )
        .unwrap();
        app.effect(&FilesEffect::GoTo {
            path: projects.clone(),
        })
        .unwrap();
        app.effect(&FilesEffect::Paste).unwrap();
        app.settle();
        assert!(folder.path().join("Projects/notes.txt").exists());
        assert!(app.undo());
        app.settle();
        assert!(
            !folder.path().join("Projects/notes.txt").exists(),
            "the copy goes"
        );
        assert!(
            folder.path().join("notes.txt").exists(),
            "the original stays"
        );

        app.effect(&FilesEffect::GoTo {
            path: folder.path().display().to_string(),
        })
        .unwrap();
        app.effect_on(
            &row("plan.txt"),
            &FilesEffect::Cut {
                entry: row("plan.txt"),
            },
        )
        .unwrap();
        app.effect(&FilesEffect::GoTo {
            path: projects.clone(),
        })
        .unwrap();
        app.effect(&FilesEffect::Paste).unwrap();
        app.settle();
        assert!(folder.path().join("Projects/plan.txt").exists());
        assert!(app.undo());
        app.settle();
        assert!(
            folder.path().join("plan.txt").exists(),
            "cut and paste moves back"
        );

        app.effect(&FilesEffect::GoTo {
            path: folder.path().display().to_string(),
        })
        .unwrap();
        act_on(
            &mut app,
            &row("plan.txt"),
            "rename",
            &serde_json::json!({"name": "agenda.txt"}),
        );
        assert!(folder.path().join("agenda.txt").exists());
        act_on(
            &mut app,
            &row("agenda.txt"),
            "move-to",
            &serde_json::json!({"folder": projects}),
        );
        assert!(folder.path().join("Projects/agenda.txt").exists());
        act_on(&mut app, &row("notes.txt"), "trash", &serde_json::json!({}));
        assert!(!folder.path().join("notes.txt").exists(), "trashed");
        assert!(app.undo());
        app.settle();
        assert!(
            folder.path().join("notes.txt").exists(),
            "undo restores it from Trash"
        );
    }

    /// Invoke the resource's action `id` with `args` through the door, as
    /// an agent does, and let the device carry out what it queued.
    fn act(app: &mut TestApp<FileExplorerShell>, id: &str, args: &serde_json::Value) {
        let instance = app.harness().component_instance_id().unwrap();
        app.harness()
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: instance,
                    address: ui::tilo_actions::ActionAddress::new(FileExplorer::KIND, id),
                },
                args.as_object().unwrap(),
            )
            .unwrap();
        app.settle();
    }

    /// Creating, renaming and moving files are document actions: queued in
    /// the Files document, carried out by this device's drain, settled with
    /// what they changed, and undone by queueing their revert.
    #[test]
    fn filesystem_changes_are_undoable_document_actions() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("draft.txt"), b"x").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        act(
            &mut app,
            "new-folder",
            &serde_json::json!({"name": "Projects"}),
        );
        assert!(folder.path().join("Projects").is_dir());
        act(
            &mut app,
            "rename",
            &serde_json::json!({"entry": "draft.txt", "name": "final.txt"}),
        );
        assert!(folder.path().join("final.txt").exists());
        let projects = folder.path().join("Projects").display().to_string();
        act(
            &mut app,
            "move",
            &serde_json::json!({"entries": ["final.txt"], "folder": projects}),
        );
        assert!(folder.path().join("Projects/final.txt").exists());
        let ops = app.state().ops;
        assert_eq!(ops.len(), 3, "{ops:?}");
        assert!(
            ops.iter()
                .all(|op| op.outcome.as_ref().is_some_and(|o| o.error().is_none()))
        );

        // Undo takes the move back on the filesystem, then the rename.
        assert!(app.undo());
        app.settle();
        assert!(folder.path().join("final.txt").exists(), "moved back");
        assert!(app.undo());
        app.settle();
        assert!(folder.path().join("draft.txt").exists(), "named back");
        assert!(app.redo());
        app.settle();
        assert!(folder.path().join("final.txt").exists(), "renamed again");

        let instance = app.harness().component_instance_id().unwrap();
        let refused = app.harness().invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: instance,
                address: ui::tilo_actions::ActionAddress::new(FileExplorer::KIND, "trash"),
            },
            serde_json::json!({"entries": ["missing.txt"]})
                .as_object()
                .unwrap(),
        );
        assert!(refused.is_err(), "a misspelt entry changes nothing");
    }

    /// The window's other work is its effects: copying a row, pasting,
    /// showing a folder, pinning the folder shown.
    #[test]
    fn the_windows_work_is_its_effects() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir(folder.path().join("Projects")).unwrap();
        std::fs::write(folder.path().join("notes.txt"), b"n").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        // A row's effect, bound to it, as the palette offers it on the row.
        let row = crate::search_provider::entry_resource_id(&folder.path().join("notes.txt"));
        app.effect_on(&row, &FilesEffect::Copy { entry: row.clone() })
            .unwrap();
        app.effect(&FilesEffect::GoTo {
            path: folder.path().join("Projects").display().to_string(),
        })
        .unwrap();
        app.effect(&FilesEffect::Paste).unwrap();
        assert!(folder.path().join("Projects/notes.txt").exists());
        assert!(
            app.effect(&FilesEffect::EmptyTrash).is_err(),
            "Trash is emptied from the Trash folder only"
        );
        act(&mut app, "add-current-bookmark", &serde_json::json!({}));
        let place = folder.path().join("Projects").display().to_string();
        assert!(app.state().bookmarks.contains(&place), "{:?}", app.state());
    }
}
