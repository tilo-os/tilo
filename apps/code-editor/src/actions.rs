//! What the editor, the tests and agents build to change the project, and
//! the ids of what the editor publishes beside it.

use tilo_actions::{ActionAddress, EncodedAction};
use ui::tilo_resource::{Edit, Id, Resource, Stamped, items};

use crate::project::{self, EntryKind, Project, ProjectAction, SourceEdit};

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

pub fn encode(action: &Action) -> Result<EncodedAction, tilo_actions::ActionError> {
    ui::tilo_resource::encode::<Project>(action)
}

pub const FILE_KIND: &str = "code_editor.project.files";

pub fn file_resource_id(file: impl std::fmt::Display) -> String {
    items::item_resource_id::<Project>("files", &Id::from(file.to_string()))
}

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

pub fn open_symbol_address(symbol: &SymbolRef) -> ActionAddress {
    crate::effects::CodeEditorEffect::REVEAL.on(symbol_resource_id(symbol))
}

pub fn reveal_result_address(result: &SearchResultRef) -> ActionAddress {
    crate::effects::CodeEditorEffect::REVEAL.on(result_resource_id(result))
}

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

#[cfg(test)]
#[must_use]
pub(crate) fn new_folder_action(name: impl Into<String>, folder: Option<&str>) -> Action {
    stamped(ProjectAction::NewFolder(project::project::NewFolder {
        name: Some(name.into()),
        folder: folder.map(Id::from),
    }))
}

#[cfg(test)]
#[must_use]
pub(crate) fn rename_action(entry: &str, name: impl Into<String>) -> Action {
    stamped(ProjectAction::Rename(project::project::Rename {
        entry: Id::from(entry),
        name: name.into(),
    }))
}

#[cfg(test)]
#[must_use]
pub(crate) fn move_action(entry: &str, folder: Option<&str>, before: Option<&str>) -> Action {
    stamped(ProjectAction::MoveEntry(project::project::MoveEntry {
        entry: Id::from(entry),
        folder: folder.map(Id::from),
        before: before.map(Id::from),
    }))
}

#[cfg(test)]
#[must_use]
pub(crate) fn delete_action(entry: &str) -> Action {
    stamped(ProjectAction::Delete(project::project::Delete {
        entry: Id::from(entry),
    }))
}

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

#[must_use]
pub fn record_text_action(file: impl Into<String>, text: impl Into<String>) -> Action {
    stamped(ProjectAction::RecordText(project::project::RecordText {
        file: file.into(),
        text: text.into(),
    }))
}

#[must_use]
pub fn terminal(action: <terminal::resource::Terminal as Resource>::Action) -> Action {
    use terminal::resource::TerminalAction;
    let operation = match action.operation {
        TerminalAction::RecordTarget(record) => {
            ProjectAction::RecordTerminalTarget(project::project::RecordTerminalTarget {
                target: record.target,
                device: record.device,
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
                device: claim.device,
            })
        }
        TerminalAction::Run(run) => ProjectAction::RunInTerminal(project::project::RunInTerminal {
            command: run.command,
        }),
        TerminalAction::Stop(_) => {
            ProjectAction::StopInTerminal(project::project::StopInTerminal {})
        }
        TerminalAction::Rerun(rerun) => {
            ProjectAction::RerunInTerminal(project::project::RerunInTerminal {
                run: rerun.runs.to_string(),
            })
        }
        TerminalAction::RerunTest(rerun) => {
            ProjectAction::RerunTestInTerminal(project::project::RerunTestInTerminal {
                run: rerun.runs.to_string(),
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

pub const EAGER_RESOURCE_SNAPSHOT_LIMIT: usize = 96;
