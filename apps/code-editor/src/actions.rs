//! What the editor, the tests and agents build to change the project, and
//! the ids of what the editor publishes beside it.
//!
//! Every change is the [`Project`]'s action: a field edit at a path (a
//! file's text, its language) or one of its verbs (creating, renaming,
//! moving and deleting files and folders, recording what was read from
//! disk, recording the embedded terminal's session). Which file is open,
//! the tabs, the caret, selection and scroll, the sidebar and the bottom
//! panel are the window's own and never an action. The language server,
//! linter and terminal are services the window runs.

use tilo_actions::{ActionAddress, EncodedAction};
use ui::tilo_resource::{Edit, Id, Resource, Stamped, items};

use crate::project::{self, EntryKind, Project, ProjectAction, SourceEdit};

/// The project's action: a field edit or a verb, stamped with when it was
/// authored.
pub type Action = <Project as Resource>::Action;

fn stamped(operation: ProjectAction) -> Action {
    Stamped {
        operation,
        now_ms: 0,
    }
}

fn edit(edit: Edit<Project>) -> Action {
    stamped(ProjectAction::FieldEdit(edit))
}

/// The wire form of `action`, for applying it to a document directly.
///
/// # Errors
/// Returns the serialization error of `action`.
pub fn encode(action: &Action) -> Result<EncodedAction, tilo_actions::ActionError> {
    ui::tilo_resource::encode::<Project>(action)
}

/// The kind of the resources a project's files and folders are published as.
pub const FILE_KIND: &str = "code_editor.project.files";

/// The resource a file of the project is published as: its item.
pub fn file_resource_id(file: impl std::fmt::Display) -> String {
    items::item_resource_id::<Project>("files", &Id::from(file.to_string()))
}

/// The resource a folder of the project is published as: its item.
pub fn folder_resource_id(folder: impl std::fmt::Display) -> String {
    file_resource_id(folder)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRef {
    pub file_id: String,
    pub name: String,
    pub path: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultRef {
    pub file_id: Option<String>,
    pub path: String,
    pub disk_path: Option<String>,
    pub preview: String,
    pub line: usize,
    pub col: usize,
}

fn path_key(path: &str) -> String {
    hex::encode(path)
}

pub fn symbol_resource_id(symbol: &SymbolRef) -> String {
    format!(
        "code_editor.symbol:{}:{}:{}",
        symbol.file_id, symbol.line, symbol.col
    )
}

pub fn result_resource_id(result: &SearchResultRef) -> String {
    format!(
        "code_editor.search-result:{}:{}:{}",
        path_key(&result.path),
        result.line,
        result.col
    )
}

/// What a symbol's search hit invokes: the reveal its live row offers.
pub fn open_symbol_address(symbol: &SymbolRef) -> ActionAddress {
    crate::effects::CodeEditorEffect::REVEAL.on(symbol_resource_id(symbol))
}

/// What a search result's hit invokes: the reveal its live row offers.
pub fn reveal_result_address(result: &SearchResultRef) -> ActionAddress {
    crate::effects::CodeEditorEffect::REVEAL.on(result_resource_id(result))
}

/// The edit turning the text of `file` from `base`, as the editor showed
/// it, into `next`: the one smallest splice, carrying enough of the text
/// around it to land where it was made if a collaborator's typing got there
/// first.
///
/// # Panics
/// Never: a text edit is plain data that always serializes.
#[allow(
    clippy::expect_used,
    reason = "a text edit is plain data that always serializes"
)]
#[must_use]
pub fn text_edit_action(file: impl Into<String>, base: &str, next: &str) -> Action {
    edit(
        Project::paths()
            .buffers
            .key(file)
            .perform(&SourceEdit::between(base, next))
            .expect("a text edit serializes"),
    )
}

/// Replace the whole text of `file` with `text`, written as the smallest
/// splice against the text it meets.
#[cfg(test)]
#[must_use]
pub(crate) fn replace_text_action(file: impl Into<String>, text: impl Into<String>) -> Action {
    edit(
        Project::paths()
            .buffers
            .key(file)
            .edit(ui::tilo_resource::EditOp::Replace { text: text.into() }),
    )
}

/// Set the language of `file` to the one labelled `language`.
#[cfg(test)]
#[must_use]
pub(crate) fn set_language_action(file: &str, language: impl Into<String>) -> Action {
    edit(
        Project::paths()
            .files
            .item(&Id::from(file))
            .language
            .set(language.into()),
    )
}

/// A new file named `name` in `folder`, holding `text`.
#[must_use]
pub fn new_file_action(
    name: impl Into<String>,
    folder: Option<&str>,
    text: impl Into<String>,
) -> Action {
    stamped(ProjectAction::NewFile(project::project::NewFile {
        name: Some(name.into()),
        folder: folder.map(Id::from),
        text: Some(text.into()),
    }))
}

/// A new folder named `name` in `folder`.
#[cfg(test)]
#[must_use]
pub(crate) fn new_folder_action(name: impl Into<String>, folder: Option<&str>) -> Action {
    stamped(ProjectAction::NewFolder(project::project::NewFolder {
        name: Some(name.into()),
        folder: folder.map(Id::from),
    }))
}

/// Rename the file or folder `entry`.
#[cfg(test)]
#[must_use]
pub(crate) fn rename_action(entry: &str, name: impl Into<String>) -> Action {
    stamped(ProjectAction::Rename(project::project::Rename {
        entry: Id::from(entry),
        name: name.into(),
    }))
}

/// Move the file or folder `entry` into `folder`, before `before`.
#[cfg(test)]
#[must_use]
pub(crate) fn move_action(entry: &str, folder: Option<&str>, before: Option<&str>) -> Action {
    stamped(ProjectAction::MoveEntry(project::project::MoveEntry {
        entry: Id::from(entry),
        folder: folder.map(Id::from),
        before: before.map(Id::from),
    }))
}

/// Delete the file or folder `entry`.
#[cfg(test)]
#[must_use]
pub(crate) fn delete_action(entry: &str) -> Action {
    stamped(ProjectAction::Delete(project::project::Delete {
        entry: Id::from(entry),
    }))
}

/// Record an entry the editor read from disk.
#[must_use]
pub fn record_entry_action(
    id: &str,
    kind: EntryKind,
    name: impl Into<String>,
    folder: Option<&str>,
    language: impl Into<String>,
) -> Action {
    stamped(ProjectAction::RecordEntry(project::project::RecordEntry {
        id: Id::from(id),
        kind,
        name: name.into(),
        folder: folder.map(Id::from),
        language: language.into(),
    }))
}

/// Keep `text`, read from disk, as the text of `file` unless it has one.
#[must_use]
pub fn record_text_action(file: impl Into<String>, text: impl Into<String>) -> Action {
    stamped(ProjectAction::RecordText(project::project::RecordText {
        file: file.into(),
        text: text.into(),
    }))
}

/// The project's action recording what the embedded terminal's session
/// recorded.
#[must_use]
pub fn terminal(action: <terminal::resource::Terminal as Resource>::Action) -> Action {
    use terminal::resource::TerminalAction;
    let operation = match action.operation {
        TerminalAction::RecordTarget(record) => {
            ProjectAction::RecordTerminalTarget(project::project::RecordTerminalTarget {
                target: record.target,
            })
        }
        TerminalAction::RecordRelaySession(record) => {
            ProjectAction::RecordTerminalRelay(project::project::RecordTerminalRelay {
                session: record.session,
            })
        }
        TerminalAction::ClaimPane(claim) => {
            ProjectAction::ClaimTerminalPane(project::project::ClaimTerminalPane {
                pane: claim.pane,
            })
        }
        TerminalAction::Run(run) => ProjectAction::RunInTerminal(project::project::RunInTerminal {
            command: run.command,
        }),
        TerminalAction::Rerun(rerun) => {
            ProjectAction::RerunInTerminal(project::project::RerunInTerminal { run: rerun.runs })
        }
        TerminalAction::RerunTest(rerun) => {
            ProjectAction::RerunTestInTerminal(project::project::RerunTestInTerminal {
                run: rerun.runs,
                test: rerun.test,
            })
        }
        TerminalAction::RecordRun(record) => {
            ProjectAction::RecordTerminalRun(project::project::RecordTerminalRun {
                run: record.run,
            })
        }
        // A session's fields change only by its verbs.
        TerminalAction::FieldEdit(edit) => ProjectAction::FieldEdit(Edit::new(
            std::iter::once(ui::tilo_resource::Segment::Field("terminal_panel".into()))
                .chain(edit.path().iter().cloned())
                .collect(),
            edit.op().clone(),
        )),
    };
    Stamped {
        operation,
        now_ms: action.now_ms,
    }
}

/// How many live rows of one kind (files, symbols, search hits) the published
/// tree carries; every omitted row still resolves by id. Keeping this small
/// prevents a large project (or a noisy search result set) from being
/// flattened on every palette interaction.
pub const EAGER_RESOURCE_SNAPSHOT_LIMIT: usize = 96;
