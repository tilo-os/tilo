//! The types Code publishes rows of: the files of the open project, the
//! symbols they declare and the hits of a project search. A file row is the
//! id of its document item, so it joins it and adds the committed read of its
//! text; a symbol and a hit live outside the document, so their type offers
//! showing them.
//!
//! A file's effects are its item's (`item = files`): opening it, reading its
//! text and saving it name the file's id, which is the item's.

use crate::effects::CodeEditorEffect;

use ui::tilo_resource;

/// A file of the open project, as a row of the project's file collection.
#[derive(tilo_resource::Row)]
#[row(
    kind = "code_editor.project.files",
    label = "File",
    effects = CodeEditorEffect
)]
pub struct FileRow;

/// A symbol a file of the project declares.
#[derive(tilo_resource::Row)]
#[row(
    kind = "code_editor.symbol",
    label = "Symbol",
    effects = CodeEditorEffect
)]
pub struct SymbolRow;

/// One hit of the project search.
#[derive(tilo_resource::Row)]
#[row(
    kind = "code_editor.search-result",
    label = "Search result",
    effects = CodeEditorEffect
)]
pub struct ResultRow;
