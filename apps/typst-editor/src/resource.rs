//! Typst Editor as a [`tilo_resource::Resource`]: a workspace of Typst files,
//! each with its name, the path it is saved at and its source, a collaborative
//! text every keystroke splices through [`TypstAction::EditSource`]. Compile,
//! open, save, export and zoom are View and Effect operations of the app;
//! live diagnostics and preview pages are rows the surface publishes.

use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use ui::framework::{
    Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::record::RecordId;
use ui::tilo_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, ParamType,
};
use ui::tilo_resource::items::{ItemAction, ItemCollection, item_children, item_resource_id};
use ui::tilo_resource::{
    ActionDecl, Doc, Id, Invoke, ModelError, PlainText, Resource, SearchEntry, TextEdit,
    publish_child,
};
use ui::widget::Element;

use crate::app::TypstApp;
use crate::samples::{DEFAULT_INDEX, SAMPLES};

/// Key of the file list under the document root.
const FILES: &str = "files";

/// One Typst file of the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypstFile {
    /// Stable id, unique in the workspace.
    pub id: String,
    /// The file name shown in the switcher (`report.typ`).
    pub name: String,
    /// Where the file was opened from or last saved to, if anywhere.
    pub path: Option<String>,
    /// The Typst source.
    pub source: String,
}

/// The workspace: its files, in the order they were added.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypstState {
    pub files: Vec<TypstFile>,
}

impl TypstState {
    /// The file `id`, if the workspace has it.
    pub fn file(&self, id: &str) -> Option<&TypstFile> {
        self.files.iter().find(|file| file.id == id)
    }
}

/// Every change of the workspace document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypstAction {
    /// Add a file with its source.
    AddFile {
        id: String,
        name: String,
        path: Option<String>,
        source: String,
    },
    /// One edit of a file's source: a keystroke's splice, or a whole new text.
    EditSource { file: String, edit: TextEdit },
    /// Rename a file.
    RenameFile { file: String, name: String },
    /// The file now lives at `path` (saved there); it takes the path's name.
    SetPath { file: String, path: String },
    /// Close a file: it leaves the workspace.
    CloseFile { file: String },
}

impl Action for TypstAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::AddFile {
                id,
                name,
                path,
                source,
            } => {
                let list = files_list(doc)?;
                if find_file(doc, &list, id).is_some() {
                    return Ok(());
                }
                let row = doc.insert_object(&list, doc.length(&list), ObjType::Map)?;
                doc.put(&row, "id", id.as_str())?;
                doc.put(&row, "name", name.as_str())?;
                if let Some(path) = path {
                    doc.put(&row, "path", path.as_str())?;
                }
                let text = doc.put_object(&row, "source", ObjType::Text)?;
                doc.splice_text(&text, 0, 0, source)?;
            }
            Self::EditSource { file, edit } => {
                let text = source_text(doc, file)
                    .ok_or_else(|| ActionError::Refused(format!("no file `{file}`")))?;
                edit.apply_under(doc, &text)?;
            }
            Self::RenameFile { file, name } => {
                let row = file_row(doc, file)?;
                doc.put(&row, "name", name.as_str())?;
            }
            Self::SetPath { file, path } => {
                let row = file_row(doc, file)?;
                doc.put(&row, "path", path.as_str())?;
                doc.put(&row, "name", file_name(path).as_str())?;
            }
            Self::CloseFile { file } => {
                let list = files_list(doc)?;
                if let Some(index) = find_file(doc, &list, file) {
                    doc.delete(&list, index)?;
                }
            }
        }
        Ok(())
    }
}

/// The last component of `path`, which names a file saved there.
pub(crate) fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("untitled.typ")
        .to_owned()
}

fn files_object(doc: &Doc) -> Option<ObjId> {
    match doc.get(&ROOT, FILES) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => None,
    }
}

fn files_list(doc: &mut Doc) -> Result<ObjId, ActionError> {
    match files_object(doc) {
        Some(list) => Ok(list),
        None => Ok(doc.put_object(&ROOT, FILES, ObjType::List)?),
    }
}

fn text_field(doc: &Doc, row: &ObjId, key: &str) -> Option<String> {
    match doc.get(row, key) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn find_file(doc: &Doc, list: &ObjId, id: &str) -> Option<usize> {
    (0..doc.length(list)).find(|&index| {
        matches!(doc.get(list, index), Ok(Some((Value::Object(ObjType::Map), row)))
            if text_field(doc, &row, "id").as_deref() == Some(id))
    })
}

fn file_row(doc: &Doc, id: &str) -> Result<ObjId, ActionError> {
    let list = files_object(doc).ok_or_else(|| ActionError::Refused("no files".into()))?;
    let index =
        find_file(doc, &list, id).ok_or_else(|| ActionError::Refused(format!("no file `{id}`")))?;
    match doc.get(&list, index)? {
        Some((Value::Object(ObjType::Map), row)) => Ok(row),
        _ => Err(ActionError::Refused(format!("no file `{id}`"))),
    }
}

/// The source text object of file `id`, if the workspace has it.
pub(crate) fn source_text(doc: &Doc, id: &str) -> Option<ObjId> {
    let row = file_row(doc, id).ok()?;
    match doc.get(&row, "source") {
        Ok(Some((Value::Object(ObjType::Text), text))) => Some(text),
        _ => None,
    }
}

/// The workspace resource.
pub struct TypstEditor;

/// The item actions of the workspace's files, offered on each file.
const ITEM_ACTIONS: &[ItemAction] = &[
    ItemAction {
        action: "rename_file",
        param: "file",
        collection: Some(FILES),
    },
    ItemAction {
        action: "close_file",
        param: "file",
        collection: Some(FILES),
    },
];

fn text_arg(args: &ActionArgs, name: &str) -> Result<String, String> {
    args.text(name)
        .map(str::to_owned)
        .ok_or_else(|| format!("`{name}` is required"))
}

impl Resource for TypstEditor {
    const KIND: &'static str = "typst.workspace";
    const LABEL: &'static str = "Typst workspace";
    const ACTION_TYPE: &'static str = "typst.action";

    type State = TypstState;
    type Action = TypstAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<TypstAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<TypstState, ModelError> {
        let Some(list) = files_object(doc) else {
            return Ok(TypstState::default());
        };
        let files = (0..doc.length(&list))
            .filter_map(|index| match doc.get(&list, index) {
                Ok(Some((Value::Object(ObjType::Map), row))) => Some(row),
                _ => None,
            })
            .filter_map(|row| {
                let source = match doc.get(&row, "source") {
                    Ok(Some((Value::Object(ObjType::Text), text))) => doc.text(&text).ok(),
                    _ => None,
                }?;
                Some(TypstFile {
                    id: text_field(doc, &row, "id")?,
                    name: text_field(doc, &row, "name").unwrap_or_default(),
                    path: text_field(doc, &row, "path"),
                    source,
                })
            })
            .collect();
        Ok(TypstState { files })
    }

    /// A new workspace starts with the file list every replica adds to, so
    /// two people who open it before syncing add to the same list.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        doc.put_object(&ROOT, FILES, ObjType::List)
            .map(drop)
            .map_err(|error| ModelError::invalid(error.to_string()))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new_file",
                "New Typst file",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "File name",
                )],
                |args, _| {
                    Ok(new_file(
                        &text_arg(args, tilo_actions::PROMPTED_PARAM)?,
                        None,
                        "",
                    ))
                },
            ),
            ActionDecl::new(
                "rename_file",
                "Rename file",
                vec![
                    ActionParam::required("file", ParamType::Text, "File id"),
                    ActionParam::required("name", ParamType::Text, "File name"),
                ],
                |args, _| {
                    Ok(TypstAction::RenameFile {
                        file: text_arg(args, "file")?,
                        name: text_arg(args, "name")?,
                    })
                },
            ),
            ActionDecl::new(
                "close_file",
                "Close file",
                vec![ActionParam::required("file", ParamType::Text, "File id")],
                |args, _| {
                    Ok(TypstAction::CloseFile {
                        file: text_arg(args, "file")?,
                    })
                },
            )
            .destructive(),
        ]
    }

    fn children(state: &TypstState) -> Vec<tilo_actions::Resource> {
        let collection = ItemCollection {
            field: FILES,
            items: state
                .files
                .iter()
                .map(|file| (Id::from_raw(file.id.clone()), file.name.clone()))
                .collect(),
        };
        let mut children = item_children::<Self>(&[collection], ITEM_ACTIONS);
        for child in &mut children {
            if let Some(file) = state
                .files
                .iter()
                .find(|file| file_resource_id(&file.id) == child.id.as_str())
            {
                let id = file.id.clone();
                child.children.push(publish_child::<PlainText, Self>(
                    &source_resource_id(&file.id),
                    &format!("{} source", file.name),
                    Some(&file.source),
                    move |edit| TypstAction::EditSource {
                        file: id.clone(),
                        edit,
                    },
                ));
            }
        }
        children
    }

    fn search(state: &TypstState) -> Vec<SearchEntry> {
        state
            .files
            .iter()
            .map(|file| {
                SearchEntry::new(&file.name, file.path.clone().unwrap_or_default())
                    .anchored(file_resource_id(&file.id))
            })
            .collect()
    }
}

/// The action that adds a new file named `name` with `source`, saved at
/// `path` if anywhere.
pub(crate) fn new_file(name: &str, path: Option<&str>, source: &str) -> TypstAction {
    TypstAction::AddFile {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_owned(),
        path: path.map(str::to_owned),
        source: source.to_owned(),
    }
}

/// The id file `id` is published under.
pub(crate) fn file_resource_id(id: &str) -> String {
    item_resource_id::<TypstEditor>(FILES, &Id::from_raw(id))
}

/// The id the source of file `id` is published under, as a plain-text child
/// of the file.
pub(crate) fn source_resource_id(id: &str) -> String {
    format!("{}/source", file_resource_id(id))
}

/// What a new workspace opened without a file starts with: the `.typ`
/// files of the Typst library in `dir`, name-sorted, or one empty file.
pub(crate) fn starting_files(dir: Option<&std::path::Path>) -> Vec<TypstAction> {
    let files = dir.map(library_files).unwrap_or_default();
    if files.is_empty() {
        vec![new_file("untitled.typ", None, "")]
    } else {
        files
    }
}

fn library_files(dir: &std::path::Path) -> Vec<TypstAction> {
    let Ok(folder) = tilo_files::Dir::open(tilo_files::Location::Native(dir.to_path_buf())) else {
        return Vec::new();
    };
    tilo_files::blocking::list(&folder, &tilo_files::VPath::root())
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| {
            entry.kind == tilo_files::Kind::File
                && std::path::Path::new(&entry.name)
                    .extension()
                    .is_some_and(|extension| extension == "typ")
        })
        .filter_map(|entry| {
            let name = tilo_files::VPath::new(&entry.name).ok()?;
            let bytes = tilo_files::blocking::read(&folder, &name).ok()?;
            let path = dir.join(&entry.name);
            Some(new_file(
                &entry.name,
                path.to_str(),
                &String::from_utf8(bytes).ok()?,
            ))
        })
        .collect()
}

/// The bundled examples a capture shows, each with a fixed id so every
/// capture of a scenario is the same.
pub(crate) fn sample_files(scenario: Option<&str>) -> Vec<TypstAction> {
    SAMPLES
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let source = match scenario {
                Some("scrolled") if index == DEFAULT_INDEX => crate::app::SCROLL_DEMO,
                Some("errors") if index == DEFAULT_INDEX => crate::samples::ERROR_WARNING,
                _ => sample.source,
            };
            TypstAction::AddFile {
                id: sample_id(index),
                name: sample.name.to_owned(),
                path: None,
                source: source.to_owned(),
            }
        })
        .collect()
}

/// The id of the bundled example at `index` in a capture.
pub(crate) fn sample_id(index: usize) -> String {
    format!("sample-{index}")
}

/// Framework app: the typst widget is the surface over [`TypstEditor`].
pub struct TypstEditorShell;

impl ResourceApp for TypstEditorShell {
    type Resource = TypstEditor;
    const ID: &'static str = "so.tilo.typst-editor";
    const TITLE: &'static str = "Typst";
    const ICON: Option<&'static str> = Some("typst-editor");
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<TypstEditor>) -> Element {
        cx.surface(TypstApp::new())
    }

    fn local_operations() -> Vec<ActionSpec> {
        crate::actions::local_operations()
    }

    /// A `.typ` path on the command line opens that file's own workspace.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input.argument().map(RecordId::identify_file)
    }

    /// A new workspace opened on a file starts with that file; one opened
    /// on nothing starts with the user's Typst library, or an empty file.
    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<TypstEditor>> {
        if !created {
            return Vec::new();
        }
        let files = if let Some(path) = input.argument() {
            vec![new_file(
                &file_name(path),
                Some(path),
                &crate::app::read_text(std::path::Path::new(path)).unwrap_or_default(),
            )]
        } else if let Some(file) = input.file() {
            vec![new_file(
                file.name(),
                None,
                &String::from_utf8_lossy(file.bytes()),
            )]
        } else {
            starting_files(crate::app::library_dir().as_deref())
        };
        files
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "preview",
            "menu",
            "problems",
            "scrolled",
            "errors",
            "editor-select",
            "preview-zoom",
            "preview-pan",
            "select",
            "stale",
            "status",
        ]
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn fixture(fixture: &mut Fixture<TypstEditor>) {
        fixture.provide(CaptureFile(sample_id(DEFAULT_INDEX)));
    }

    /// Every capture shows the bundled examples.
    fn scenario(name: Option<&str>) -> Vec<Invoke<TypstEditor>> {
        sample_files(name)
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }
}

/// The file a capture shows first.
pub(crate) struct CaptureFile(pub(crate) String);

impl ResourceSurface<TypstEditor> for TypstApp {
    fn present(&mut self, state: &Rc<TypstState>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<TypstEditor>) {
        self.attach_surface(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_rows()
            .into_iter()
            .flat_map(|row| {
                let resource = row.into_resource();
                std::iter::once(resource.clone()).chain(resource.children)
            })
            .find(|resource| resource.id.as_str() == id)
            .map(crate::actions::live_row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;

    /// A `.typ` file in a fresh directory.
    fn typ_file(name: &str, source: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, source).unwrap();
        (dir, path.to_str().unwrap().to_owned())
    }

    #[test]
    fn a_launched_file_opens_its_own_workspace_once() {
        let (_dir, path) = typ_file("notes.typ", "= Notes\n");
        let app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
        let record = app.record().clone();
        let state = app.state();
        assert_eq!(state.files.len(), 1);
        assert_eq!(state.files[0].name, "notes.typ");
        assert_eq!(state.files[0].path.as_deref(), Some(path.as_str()));
        assert_eq!(state.files[0].source, "= Notes\n");

        let app = app.relaunch(&[&path]).unwrap();
        assert_eq!(app.record(), &record, "the file keeps its own workspace");
        assert_eq!(app.state().files.len(), 1, "a reopen adds nothing");
    }

    #[test]
    fn a_files_source_is_its_own_child_every_edit_an_undoable_splice() {
        let (_dir, path) = typ_file("notes.typ", "= Notes\n");
        let mut app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
        let file = app.state().files[0].id.clone();
        let other = new_file("other.typ", None, "= Other\n");
        app.invoke(Invoke::apply(&other).unwrap()).unwrap();

        app.invoke_child::<PlainText>(
            &source_resource_id(&file),
            Invoke::<PlainText>::apply(&TextEdit::Splice {
                at: 8,
                delete: 0,
                insert: "Body".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let state = app.state();
        assert_eq!(state.file(&file).unwrap().source, "= Notes\nBody");
        assert!(
            state
                .files
                .iter()
                .any(|other| other.id != file && other.source == "= Other\n"),
            "another file's text is its own"
        );
        let inspection = app.harness().inspection_json().unwrap();
        assert!(
            inspection.contains("Body"),
            "the editor shows it: {inspection}"
        );

        assert!(app.undo());
        assert_eq!(app.state().file(&file).unwrap().source, "= Notes\n");
    }

    #[test]
    fn typing_splices_the_open_file_and_undoes_a_word_at_a_time() {
        let (_dir, path) = typ_file("notes.typ", "");
        let mut app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
        app.harness().type_text("hello world");
        assert_eq!(app.state().files[0].source, "hello world");
        assert!(app.undo());
        assert_eq!(app.state().files[0].source, "hello ");
        app.set_role(ui::Role::Viewer);
        app.harness().type_text("!");
        assert_eq!(
            app.state().files[0].source,
            "hello ",
            "a viewer types nothing"
        );
    }

    #[test]
    fn the_workspace_publishes_its_files_and_the_apps_operations() {
        let (_dir, path) = typ_file("notes.typ", "= Notes\n");
        let mut app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
        let file = app.state().files[0].id.clone();
        let surface = app.agent_surface().to_string();
        for needle in [
            "new_file",
            "rename_file",
            "close_file",
            "\"open\"",
            "\"save\"",
            "export-pdf",
            "compile",
            "zoom-in",
            file_resource_id(&file).as_str(),
            source_resource_id(&file).as_str(),
        ] {
            assert!(surface.contains(needle), "missing {needle} in {surface}");
        }
        app.call_on(
            &file_resource_id(&file),
            "rename_file",
            serde_json::json!({ "name": "renamed.typ" }),
        )
        .unwrap();
        assert_eq!(app.state().files[0].name, "renamed.typ");
    }

    #[test]
    fn a_capture_shows_the_examples_with_their_problems_as_live_rows() {
        let mut app = TestApp::<TypstEditorShell>::capture(Some("errors")).unwrap();
        let state = app.state();
        assert_eq!(state.files.len(), SAMPLES.len());
        assert_eq!(
            state.file(&sample_id(DEFAULT_INDEX)).unwrap().source,
            crate::samples::ERROR_WARNING
        );
        let surface = app.agent_surface().to_string();
        assert!(surface.contains("typst.diagnostic"), "{surface}");
    }
}
