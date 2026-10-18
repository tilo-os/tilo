//! The Writer document: its storage, the operations people, agents and
//! scripts take on it, and the objects inside it they are offered on.
//!
//! Typing, deleting, formatting and paragraph styles are edits of the body
//! field ([`Document::paths`]`().body`), in the rich-text vocabulary
//! ([`RichTextAction`]); the framework applies them at the body, coalesces a
//! typed word into one undo step and names them in history. What is more
//! than an edit of the text — a table, a review decision, a comment — is a
//! verb of the document.

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use tilo_document::action::{TableEdit, TocTarget};
use tilo_document::{ClipBlock, RichText, RichTextAction, RichTextState, TextAnchor};
use ui::tilo_resource::{
    ActionCx, Collection, Demand, Derived, Doc, Edit, Id, Json, Model, ModelError, Place, Refusal,
    Resource, resource,
};

/// The field of [`Document`] the rich text is stored under.
pub const BODY: &str = "body";

/// A Writer document.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Document {
    /// The text: paragraphs, tables, images, styles, the page header and
    /// footer, notes and tracked changes.
    pub body: RichText,
    /// The comment threads, each about some text of the body. Each thread is
    /// a storage unit of its own: commenting never writes the text, so
    /// someone who may only comment is held to the threads.
    #[model(unit)]
    pub comments: Collection<Comment>,
}

/// A comment thread on some text of the document.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Comment {
    /// The text the comment is about.
    pub anchor: TextAnchor,
    /// Who wrote it.
    #[model(readonly)]
    pub author: String,
    /// When it was written, as an ISO-8601 time.
    #[model(readonly)]
    pub date: String,
    /// What it says.
    #[model(label)]
    pub text: String,
    /// The answers, in the order they were written.
    pub replies: Collection<Reply>,
}

/// An answer in a comment thread.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Reply {
    /// Who wrote it.
    #[model(readonly)]
    pub author: String,
    /// When it was written, as an ISO-8601 time.
    #[model(readonly)]
    pub date: String,
    /// What it says, as plain text.
    #[model(label)]
    pub text: String,
    /// Its paragraphs as written, with their formatting; empty when it is
    /// plain text.
    #[model(readonly)]
    pub body: Json<Vec<ClipBlock>>,
}

#[resource(
    kind = "word-processor.document",
    label = "Document",
    children = Document::objects,
    load = Document::demand,
    locate = Document::locate,
    lands = Document::lands,
    answer = Document::answer,
)]
impl Document {
    /// Add a paragraph.
    ///
    /// A new paragraph holding the text, at the end of the document.
    #[action(answers)]
    pub fn append_paragraph(&mut self, text: String) -> Result<(), Refusal> {
        if text.is_empty() {
            return Err(Refusal::new("the paragraph needs text"));
        }
        self.body.perform(RichTextAction::AppendParagraph { text });
        Ok(())
    }

    /// Insert a table.
    ///
    /// An empty table of `rows` by `columns` after the paragraph `after`.
    #[action(answers)]
    pub fn insert_table(&mut self, after: String, rows: usize, columns: usize) {
        self.body.perform(RichTextAction::InsertTable {
            after,
            rows,
            cols: columns,
        });
    }

    /// Add a row.
    ///
    /// A new empty row at the bottom of the table.
    #[action(answers)]
    pub fn add_row(&mut self, table: String) -> Result<(), Refusal> {
        let (rows, _) = self.table_size(&table)?;
        self.edit_table(table, TableEdit::InsertRow { at: rows });
        Ok(())
    }

    /// Add a column.
    ///
    /// A new empty column at the right of the table.
    #[action(answers)]
    pub fn add_column(&mut self, table: String) -> Result<(), Refusal> {
        let (_, columns) = self.table_size(&table)?;
        self.edit_table(table, TableEdit::InsertColumn { at: columns });
        Ok(())
    }

    /// Delete a row.
    #[action(destructive)]
    pub fn delete_row(&mut self, table: String, row: usize) {
        self.edit_table(table, TableEdit::DeleteRow { row });
    }

    /// Delete a column.
    #[action(destructive)]
    pub fn delete_column(&mut self, table: String, column: usize) {
        self.edit_table(table, TableEdit::DeleteColumn { column });
    }

    /// Delete a table.
    #[action(destructive)]
    pub fn delete_table(&mut self, table: String) {
        self.edit_table(table, TableEdit::Delete);
    }

    /// Delete an image.
    #[action(destructive)]
    pub fn delete_image(&mut self, image: String) {
        self.body.perform(RichTextAction::RemoveBlocks {
            blocks: vec![image],
        });
    }

    /// Accept change.
    ///
    /// Keep one tracked insertion or deletion as the text.
    #[action]
    pub fn accept_change(&mut self, change: String) {
        self.body
            .perform(RichTextAction::AcceptChanges { id: Some(change) });
    }

    /// Reject change.
    ///
    /// Undo one tracked insertion or deletion.
    #[action]
    pub fn reject_change(&mut self, change: String) {
        self.body
            .perform(RichTextAction::RejectChanges { id: Some(change) });
    }

    /// Accept all changes.
    #[action(destructive)]
    pub fn accept_all_changes(&mut self) {
        self.body
            .perform(RichTextAction::AcceptChanges { id: None });
    }

    /// Reject all changes.
    #[action(destructive)]
    pub fn reject_all_changes(&mut self) {
        self.body
            .perform(RichTextAction::RejectChanges { id: None });
    }

    /// Add a comment.
    ///
    /// A comment on the characters `start` to `end` of the paragraph `block`.
    #[action(writes = comments)]
    pub fn add_comment(
        &mut self,
        cx: &mut ActionCx,
        block: String,
        start: usize,
        end: usize,
        text: String,
    ) -> Result<(), Refusal> {
        if text.trim().is_empty() {
            return Err(Refusal::new("a comment says something"));
        }
        let anchor = TextAnchor::new(&self.body, &block, start, end).map_err(Refusal::new)?;
        self.comments.push(
            cx.id(),
            Comment {
                anchor,
                author: author(cx),
                date: date(cx),
                text,
                replies: Collection::new(),
            },
        );
        Ok(())
    }

    /// Reply to a comment.
    ///
    /// An answer below the comment; `body`, when given, is the answer's
    /// paragraphs with their formatting, and `text` its plain text.
    #[action(item = comments)]
    pub fn reply_to_comment(
        &mut self,
        cx: &mut ActionCx,
        comment: &Id,
        text: String,
        body: Option<Vec<ClipBlock>>,
    ) -> Result<(), Refusal> {
        if text.trim().is_empty() {
            return Err(Refusal::new("a reply says something"));
        }
        let id = cx.id();
        let author = author(cx);
        let date = date(cx);
        let thread = self
            .comments
            .get_mut(comment)
            .ok_or_else(|| Refusal::new("there is no such comment; it may have been resolved"))?;
        thread.replies.push(
            id,
            Reply {
                author,
                date,
                text,
                body: Json(body.unwrap_or_default()),
            },
        );
        Ok(())
    }

    /// Resolve a comment.
    ///
    /// The thread is done with and goes away.
    #[action(item = comments)]
    pub fn resolve_comment(&mut self, comment: &Id) -> Result<(), Refusal> {
        self.comments
            .remove(comment)
            .map(drop)
            .ok_or_else(|| Refusal::new("there is no such comment"))
    }

    /// Keep imported comments.
    ///
    /// The comments a file brought along with its text become the
    /// document's comment threads.
    #[action(writes = comments)]
    pub fn adopt_comments(&mut self, cx: &mut ActionCx) -> Result<(), Refusal> {
        let imported: Vec<tilo_doc::snapshot::CommentView> = self
            .body
            .state()
            .map(|state| state.comments().as_ref().clone())
            .unwrap_or_default();
        for comment in imported {
            self.body.perform(RichTextAction::ResolveComment {
                id: comment.id.clone(),
            });
            let Some((block, start, end)) = comment.range else {
                continue;
            };
            let Ok(anchor) = TextAnchor::new(&self.body, &block, start, end) else {
                continue;
            };
            let mut replies = Collection::new();
            for reply in &comment.replies {
                replies.push(
                    cx.id(),
                    Reply {
                        author: reply.author.clone(),
                        date: reply.date.clone(),
                        text: joined(&reply.blocks),
                        body: Json(reply.blocks.iter().map(clip_block).collect()),
                    },
                );
            }
            self.comments.push(
                cx.id(),
                Comment {
                    anchor,
                    author: comment.author,
                    date: comment.date,
                    text: joined(&comment.blocks),
                    replies,
                },
            );
        }
        Ok(())
    }

    /// Set page margins.
    ///
    /// The four page margins, in pixels.
    #[action]
    pub fn set_page_margins(&mut self, top: f64, right: f64, bottom: f64, left: f64) {
        self.body.perform(RichTextAction::SetPageMargins {
            top,
            right,
            bottom,
            left,
        });
    }

    /// Generate table of contents.
    ///
    /// A table of contents of the document's headings.
    #[action]
    pub fn generate_toc(&mut self) -> Result<(), Refusal> {
        let entries: Vec<TocTarget> = self
            .body
            .state()
            .map(RichTextState::headings)
            .unwrap_or_default()
            .into_iter()
            .map(|heading| TocTarget {
                heading: heading.id,
                level: heading.level,
                text: heading.text,
                page: None,
            })
            .collect();
        if entries.is_empty() {
            return Err(Refusal::new("the document has no headings"));
        }
        self.body.perform(RichTextAction::GenerateToc { entries });
        Ok(())
    }

    /// Note the file written.
    ///
    /// The document was written to its file, whose bytes have the content
    /// address `digest`: what a later open compares the file with.
    #[action(external)]
    pub fn note_file_written(&mut self, digest: String) {
        self.body
            .perform(RichTextAction::NoteFileWritten { digest });
    }

    fn edit_table(&mut self, table: String, edit: TableEdit) {
        self.body.perform(RichTextAction::EditTable { table, edit });
    }

    /// The rows and columns of `table`.
    fn table_size(&self, table: &str) -> Result<(usize, usize), Refusal> {
        let cells = self
            .body
            .state()
            .map(|state| state.featured("cell"))
            .unwrap_or_default();
        let mut size = None;
        for cell in cells
            .iter()
            .filter(|cell| attr_str(cell, "table") == Some(table))
        {
            let (rows, columns) = size.unwrap_or((0, 0));
            size = Some((
                rows.max(attr_index(cell, "row").saturating_add(1)),
                columns.max(attr_index(cell, "col").saturating_add(1)),
            ));
        }
        size.ok_or_else(|| Refusal::new("there is no such table"))
    }

    /// The objects inside the document people act on: its tables, images
    /// and tracked changes, each with the operations it offers.
    fn objects(&self) -> Vec<Derived<Self>> {
        let Some(state) = self.body.state() else {
            return Vec::new();
        };
        let mut objects = Vec::new();
        let cells = state.featured("cell");
        let mut tables: Vec<String> = Vec::new();
        for cell in &cells {
            if let Some(table) = attr_str(cell, "table")
                && !tables.iter().any(|known| known == table)
            {
                tables.push(table.to_owned());
            }
        }
        for table in tables.into_iter().take(MAX_OBJECTS) {
            let mut node = Derived::new("Table")
                .of_kind("table")
                .identified(format!("table:{table}"))
                .with(Self::ADD_ROW.bind(document::AddRow::paths().table, table.clone()))
                .with(Self::ADD_COLUMN.bind(document::AddColumn::paths().table, table.clone()))
                .with(Self::DELETE_TABLE.bind(document::DeleteTable::paths().table, table.clone()));
            // Each cell goes to its text, and deletes its row or column.
            for cell in cells
                .iter()
                .filter(|cell| attr_str(cell, "table") == Some(table.as_str()))
            {
                let (row, column) = (attr_index(cell, "row"), attr_index(cell, "col"));
                node = node.with_child(
                    Derived::new(format!(
                        "Cell {}, {}",
                        row.saturating_add(1),
                        column.saturating_add(1)
                    ))
                    .of_kind("table-cell")
                    .identified(format!("cell:{}", cell.id))
                    .with(
                        Self::DELETE_ROW
                            .bind(document::DeleteRow::paths().table, table.clone())
                            .and(document::DeleteRow::paths().row, row),
                    )
                    .with(
                        Self::DELETE_COLUMN
                            .bind(document::DeleteColumn::paths().table, table.clone())
                            .and(document::DeleteColumn::paths().column, column),
                    )
                    .with_effect(&crate::effects::WordProcessorEffect::GoTo {
                        block: cell.id.clone(),
                        start: 0,
                        end: cell.text.chars().count(),
                    }),
                );
            }
            objects.push(node);
        }
        for image in state.featured("image").into_iter().take(MAX_OBJECTS) {
            objects.push(
                Derived::new("Image")
                    .of_kind("image")
                    .identified(format!("image:{}", image.id))
                    .with(
                        Self::DELETE_IMAGE
                            .bind(document::DeleteImage::paths().image, image.id.clone()),
                    ),
            );
        }
        objects.extend(Self::paragraph_places(state));
        for change in state.changes().iter().take(MAX_OBJECTS) {
            let label = match change.kind {
                tilo_doc::snapshot::ChangeKind::Ins => "Insertion",
                tilo_doc::snapshot::ChangeKind::Del => "Deletion",
            };
            objects.push(
                Derived::new(label)
                    .of_kind("change")
                    .identified(format!("change:{}", change.id))
                    .with(
                        Self::ACCEPT_CHANGE
                            .bind(document::AcceptChange::paths().change, change.id.clone()),
                    )
                    .with(
                        Self::REJECT_CHANGE
                            .bind(document::RejectChange::paths().change, change.id.clone()),
                    ),
            );
        }
        objects
    }

    /// The first paragraphs and headings, by their first words: the places
    /// an edit is told to act at (`at: {"item": <paragraph>}`). Headings are
    /// always read; a paragraph once it has been loaded.
    fn paragraph_places(state: &tilo_document::RichTextState) -> Vec<Derived<Self>> {
        let headings = state.featured("heading");
        state
            .body()
            .ids(0..PLACED_PARAGRAPHS)
            .filter_map(|id| {
                state
                    .block(id)
                    .or_else(|| headings.iter().find(|heading| heading.id == id))
            })
            .filter(|block| matches!(block.kind.as_str(), "paragraph" | "heading" | "list_item"))
            .filter(|block| !block.text.trim().is_empty())
            .map(|block| {
                Derived::new(first_words(&block.text))
                    .of_kind(if block.kind == "heading" {
                        "heading"
                    } else {
                        "paragraph"
                    })
                    .identified(block.id.clone())
                    .in_field("blocks")
                    .with_effect(&crate::effects::WordProcessorEffect::SelectBlock {
                        block: block.id.clone(),
                    })
            })
            .collect()
    }

    /// Read the paragraphs a view shows, and only those.
    fn demand(&mut self, doc: &Doc, demand: &Demand) -> Result<(), ModelError> {
        self.body.demand(doc, demand)
    }

    /// Where a verb leaves whoever took it: after the paragraph it appended,
    /// on the table it inserted. Body edits land through the body.
    fn lands(&self, action: &<Self as Resource>::Action) -> Option<Place> {
        match &action.operation {
            DocumentAction::AppendParagraph(append) => ui::tilo_resource::Operations::lands(
                &self.body,
                &RichTextAction::AppendParagraph {
                    text: append.text.clone(),
                },
            ),
            DocumentAction::InsertTable(table) => Some(Place::Item(table.after.clone())),
            _ => None,
        }
    }

    /// Where an object of a paragraph on screen sits, from the body's index.
    fn locate(&self, obj: &ObjId) -> Option<ui::tilo_resource::ObjPath> {
        self.body.object_path(obj)
    }
}

/// Objects of each kind a published document lists, in document order.
const MAX_OBJECTS: usize = 64;

fn attr_str<'a>(block: &'a tilo_doc::snapshot::BlockView, key: &str) -> Option<&'a str> {
    block.str_attr(key)
}

fn attr_index(block: &tilo_doc::snapshot::BlockView, key: &str) -> usize {
    block
        .int_attr(key)
        .map_or(0, |value| usize::try_from(value).unwrap_or_default())
}

/// The paragraphs of an imported comment as one text.
fn joined(blocks: &[tilo_doc::snapshot::BlockView]) -> String {
    blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// An imported comment's paragraph as the clipboard form a reply keeps.
fn clip_block(block: &tilo_doc::snapshot::BlockView) -> ClipBlock {
    ClipBlock {
        kind: block.kind.clone(),
        text: block.text.clone(),
        marks: block.marks.clone(),
        level: block.int_attr("level"),
        ordered: block.bool_attr("ordered"),
        align: block.str_attr("align").map(str::to_owned),
        attrs: std::collections::BTreeMap::default(),
    }
}

/// Who a comment or reply is by: whoever invoked the action.
fn author(cx: &ActionCx) -> String {
    cx.author().unwrap_or_default().to_owned()
}

/// When a comment or reply was written, as the ISO-8601 time tilo-doc stores.
fn date(cx: &ActionCx) -> String {
    iso_time(cx.now_ms())
}

/// `ms` after the Unix epoch as an ISO-8601 UTC time.
fn iso_time(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    // format: machine (an RFC 3339 timestamp the document stores)
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    )
}

/// The action that edits the body with `op`: what the editor, a control or
/// an agent submits for a keystroke, a mark or a paragraph style.
#[must_use]
pub fn edit(op: &RichTextAction) -> Edit<Document> {
    // A rich-text action is a plain serde enum over owned data, so it always
    // encodes; were it not to, the null it became is refused at the body.
    Document::paths()
        .body
        .edit(ui::tilo_resource::EditOp::Rich {
            rich: serde_json::to_value(op).unwrap_or_default(),
        })
}

/// [`edit`] as the document's action value.
#[must_use]
pub fn action(op: &RichTextAction) -> <Document as Resource>::Action {
    ui::tilo_resource::Stamped {
        operation: DocumentAction::FieldEdit(edit(op)),
        now_ms: 0,
    }
}

/// The body's rich text in `doc`: the map the `body` field is stored in.
#[must_use]
pub fn body(doc: &AutoCommit) -> Option<ObjId> {
    body_at(doc, &ROOT)
}

/// The body of the document stored under `record`.
#[must_use]
pub fn body_at(doc: &AutoCommit, record: &ObjId) -> Option<ObjId> {
    match doc.get(record, BODY) {
        Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
        _ => None,
    }
}

/// The body of `doc` opened as rich text.
///
/// # Errors
/// Returns why `doc` holds no Writer document.
pub fn open(doc: &AutoCommit) -> Result<tilo_doc::Containers, String> {
    let body = body(doc).ok_or("the document has no body")?;
    tilo_doc::model::open(doc, &body).map_err(|error| error.to_string())
}

/// `doc` as a file holds it: its body with the comment threads written
/// into the text the way file formats keep comments, for the codecs
/// ([`tilo_doc::Document::as_ref`]).
///
/// # Errors
/// Returns why `doc` holds no Writer document.
pub fn export_copy(doc: &AutoCommit) -> Result<tilo_doc::Document, String> {
    let document = <Document as Resource>::read(doc).map_err(|error| error.to_string())?;
    let threads = document.comment_views(doc);
    let mut copy = reopen(doc.clone())?;
    for thread in threads {
        let Some((block, start, end)) = &thread.range else {
            continue;
        };
        let text = thread
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let id = copy.add_comment(block, *start, *end, &thread.author, &text);
        for reply in &thread.replies {
            let text = reply
                .blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let _ = copy.add_comment_reply(&id, &reply.author, &reply.date, &text);
        }
    }
    Ok(copy)
}

/// Start a document's body in `doc` with the rich-text schema and nothing
/// else, for tools and tests that build a document block by block.
///
/// # Errors
/// Returns the document's write error.
pub fn init(doc: &mut AutoCommit) -> Result<tilo_doc::Containers, String> {
    let body = match body(doc) {
        Some(body) => body,
        None => ui::tilo_resource::Slot::Key(&ROOT, BODY)
            .object(doc, ObjType::Map)
            .map_err(|error| error.to_string())?,
    };
    Ok(tilo_doc::model::init(doc, &body))
}

impl Document {
    /// What a change to the document made, for its caller: the blocks it
    /// changed, each with the handle that edits it, so an agent fills
    /// what it made without reading the document again.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "the resource's answer hook: some records answer nothing"
    )]
    fn answer(
        doc: &AutoCommit,
        _action: &str,
        changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "changed": crate::semantic_read::changes::summary(doc, changes),
        }))
    }
}

/// The body of the Writer document `doc` as a tilo-doc [`tilo_doc::Document`].
///
/// # Errors
/// Returns why `doc` holds no Writer document.
pub fn reopen(doc: AutoCommit) -> Result<tilo_doc::Document, String> {
    let body = body(&doc).ok_or("the document has no body")?;
    tilo_doc::Document::from_automerge_under(doc, &body).map_err(|error| error.to_string())
}

/// Apply `op` to the body of `doc` directly, outside any host: for tools and
/// tests that build a document.
///
/// # Errors
/// Returns the action's refusal.
pub fn apply(doc: &mut AutoCommit, op: &RichTextAction) -> Result<(), tilo_actions::ActionError> {
    tilo_actions::Action::apply(&action(op), doc)
}

/// [`action`] encoded for the wire, as the door and replay carry it.
#[must_use]
pub fn encoded(op: &RichTextAction) -> tilo_actions::EncodedAction {
    let action = action(op);
    ui::tilo_resource::encode::<Document>(&action).unwrap_or_else(|_| tilo_actions::EncodedAction {
        type_id: Document::KIND.to_owned(),
        payload: Vec::new(),
    })
}

/// The body edit an encoded document action carries, when it is one.
#[must_use]
pub fn decoded(encoded: &tilo_actions::EncodedAction) -> Option<RichTextAction> {
    let action: <Document as Resource>::Action = serde_json::from_slice(&encoded.payload).ok()?;
    match action.operation {
        DocumentAction::FieldEdit(edit) => match edit.op() {
            ui::tilo_resource::EditOp::Rich { rich } => serde_json::from_value(rich.clone()).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// One paragraph of comment text, as the review margin draws it.
fn paragraph(id: String, text: &str) -> tilo_doc::snapshot::BlockView {
    tilo_doc::snapshot::BlockView {
        id,
        kind: "paragraph".into(),
        text: text.to_owned(),
        marks: Vec::new(),
        attrs: std::collections::BTreeMap::default(),
        children: Vec::new(),
    }
}

/// A reply's paragraphs as written, or its text as one paragraph.
fn reply_blocks(id: &Id, reply: &Reply) -> Vec<tilo_doc::snapshot::BlockView> {
    if reply.body.0.is_empty() {
        return vec![paragraph(format!("{id}.0"), &reply.text)];
    }
    reply
        .body
        .0
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let mut attrs = std::collections::BTreeMap::new();
            if let Some(level) = block.level {
                attrs.insert("level".to_owned(), tilo_doc::AttrVal::Int(level));
            }
            if let Some(ordered) = block.ordered {
                attrs.insert("ordered".to_owned(), tilo_doc::AttrVal::Bool(ordered));
            }
            tilo_doc::snapshot::BlockView {
                id: format!("{id}.{index}"),
                kind: block.kind.clone(),
                text: block.text.clone(),
                marks: block.marks.clone(),
                attrs,
                children: Vec::new(),
            }
        })
        .collect()
}

impl Document {
    /// The comment threads as the review margin draws them, each with where
    /// its text is in `doc` now.
    #[must_use]
    pub fn comment_views(&self, doc: &AutoCommit) -> Vec<tilo_doc::snapshot::CommentView> {
        self.comments
            .iter()
            .map(|(id, comment)| tilo_doc::snapshot::CommentView {
                id: id.to_string(),
                author: comment.author.clone(),
                date: comment.date.clone(),
                blocks: vec![paragraph(format!("{id}.0"), &comment.text)],
                replies: comment
                    .replies
                    .iter()
                    .map(|(reply_id, reply)| tilo_doc::snapshot::CommentReplyView {
                        id: reply_id.to_string(),
                        author: reply.author.clone(),
                        date: reply.date.clone(),
                        blocks: reply_blocks(reply_id, reply),
                    })
                    .collect(),
                range: comment.anchor.range(doc),
            })
            .collect()
    }

    /// What views read of the body, once it has been read.
    #[must_use]
    pub fn text(&self) -> Option<&RichTextState> {
        self.body.state()
    }
}

/// Write a comment thread by `author` on `start..end` of `block` straight
/// into `doc`, outside any host, as a tool importing comments does.
#[cfg(test)]
pub(crate) fn comment_by(
    doc: &mut AutoCommit,
    author: &str,
    block: &str,
    (start, end): (usize, usize),
    text: &str,
) -> String {
    let mut document = <Document as Resource>::read(doc).expect("a Writer document");
    let id = Id::from_raw(format!("comment-{}", document.comments.len()));
    let anchor = TextAnchor::new(&document.body, block, start, end).expect("text to anchor to");
    document.comments.push(
        id.clone(),
        Comment {
            anchor,
            author: author.to_owned(),
            date: String::new(),
            text: text.to_owned(),
            replies: Collection::new(),
        },
    );
    ui::tilo_resource::Fields::write_fields(&document, doc, &ROOT).expect("the comment is written");
    id.to_string()
}

/// Write a reply by `author` to the thread `comment` straight into `doc`.
#[cfg(test)]
pub(crate) fn reply_by(doc: &mut AutoCommit, comment: &str, author: &str, date: &str, text: &str) {
    let mut document = <Document as Resource>::read(doc).expect("a Writer document");
    let thread = document
        .comments
        .get_mut(&Id::from_raw(comment))
        .expect("the thread exists");
    let id = Id::from_raw(format!("{comment}-reply-{}", thread.replies.len()));
    thread.replies.push(
        id,
        Reply {
            author: author.to_owned(),
            date: date.to_owned(),
            text: text.to_owned(),
            body: Json(Vec::new()),
        },
    );
    ui::tilo_resource::Fields::write_fields(&document, doc, &ROOT).expect("the reply is written");
}

/// How many of the document's first paragraphs are published as places an
/// edit can be told to act at.
pub const PLACED_PARAGRAPHS: usize = 128;

/// The words a paragraph published as a place is labelled with.
const LABEL_WORDS: usize = 8;

/// `text`'s first words, as a paragraph published as a place is labelled.
fn first_words(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut label = words
        .iter()
        .take(LABEL_WORDS)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if words.len() > LABEL_WORDS {
        label.push('…');
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::{Invoke, Replica};

    /// A document whose one paragraph says `text`, as `author` would edit it.
    fn written(text: &str, author: &str) -> (Replica<Document>, String) {
        let _ = author;
        let mut replica = Replica::<Document>::new();
        let block = replica
            .state()
            .unwrap()
            .text()
            .unwrap()
            .body()
            .id_at(0)
            .unwrap()
            .to_owned();
        let typed = RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block: block.clone(),
            at: 0,
            del: 0,
            ins: text.to_owned(),
        };
        replica.apply(Invoke::edit(&edit(&typed)).unwrap()).unwrap();
        (replica, block)
    }

    #[test]
    fn a_comment_writes_its_thread_and_never_the_text() {
        let (mut replica, block) = written("The budget doubled this year.", "ada");
        let before = replica.heads();
        replica
            .apply(Document::ADD_COMMENT.with(document::AddComment {
                block: block.clone(),
                start: 4,
                end: 10,
                text: "By how much exactly?".into(),
            }))
            .unwrap();
        let after = replica.heads();
        let comments = match replica.doc().get(ROOT, "comments") {
            Ok(Some((_, comments))) => comments,
            other => panic!("the threads are stored: {other:?}"),
        };
        let patches = replica.doc_mut().diff(&before, &after);
        assert!(!patches.is_empty());
        for patch in &patches {
            assert!(
                patch.obj == comments || patch.path.iter().any(|(obj, _)| *obj == comments),
                "the comment wrote outside its thread: {patch:?}"
            );
        }
        let threads = replica.state().unwrap().comment_views(replica.doc());
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].range, Some((block, 4, 10)));
        // The thread is offered as an item a reply and a resolve act on.
        let published = replica.published().unwrap();
        let thread = format!("{}/comments/{}", Document::KIND, threads[0].id);
        let item = published
            .children
            .iter()
            .find(|child| child.id.as_str() == thread)
            .expect("the thread is published");
        assert!(
            item.actions
                .iter()
                .any(|action| action.id == "reply-to-comment")
        );
        assert!(
            item.actions
                .iter()
                .any(|action| action.id == "resolve-comment")
        );
    }

    #[test]
    fn a_comment_follows_its_text_through_concurrent_edits() {
        let (mut here, block) = written("Ship it on Friday.", "ada");
        here.apply(Document::ADD_COMMENT.with(document::AddComment {
            block: block.clone(),
            start: 11,
            end: 17,
            text: "Friday is a holiday".into(),
        }))
        .unwrap();
        let mut there = here.fork();
        let typed = |at: usize, text: &str| {
            Invoke::edit(&edit(&RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block: block.clone(),
                at,
                del: 0,
                ins: text.to_owned(),
            }))
            .unwrap()
        };
        here.apply(typed(0, "Please ")).unwrap();
        there.apply(typed(5, " the build")).unwrap();
        here.sync_with(&mut there).unwrap();
        let text = "Please Ship the build it on Friday.";
        let start = text.find("Friday").unwrap();
        for replica in [&here, &there] {
            let threads = replica.state().unwrap().comment_views(replica.doc());
            assert_eq!(threads[0].range, Some((block.clone(), start, start + 6)));
        }
    }

    #[test]
    fn a_file_keeps_the_comments_in_its_text_and_brings_them_back() {
        let (mut replica, block) = written("Doors open at seven.", "ada");
        replica
            .apply(Document::ADD_COMMENT.with(document::AddComment {
                block,
                start: 14,
                end: 19,
                text: "Seven sharp?".into(),
            }))
            .unwrap();
        let file = export_copy(replica.doc()).unwrap();
        let bytes = crate::WpFormat::Docx
            .export(file.as_ref(), &tilo_content::NoContent)
            .unwrap();

        let mut reopened = Replica::<Document>::new();
        let import = crate::import_action(
            &bytes,
            crate::WpFormat::Docx,
            &tilo_content::ContentStore::memory(),
        )
        .unwrap();
        reopened
            .apply(Invoke::group([
                Invoke::edit(&edit(&import)).unwrap(),
                Document::ADOPT_COMMENTS.invoke(),
            ]))
            .unwrap();
        let threads = reopened.state().unwrap().comment_views(reopened.doc());
        assert_eq!(threads.len(), 1, "{threads:?}");
        assert_eq!(threads[0].blocks[0].text, "Seven sharp?");
        let (block, start, end) = threads[0].range.clone().expect("about some text");
        let text =
            tilo_doc::model::block_text(reopened.doc(), &open(reopened.doc()).unwrap(), &block)
                .unwrap();
        assert_eq!(
            text.chars()
                .skip(start)
                .take(end - start)
                .collect::<String>(),
            "seven"
        );
        // The text it came in no longer holds comments of its own.
        let snapshot = tilo_doc::model::materialize(reopened.doc(), &open(reopened.doc()).unwrap());
        assert!(snapshot.comments.is_empty());
    }

    #[test]
    fn a_comment_is_dated_in_iso_utc() {
        use super::iso_time;
        assert_eq!(iso_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_time(1_791_551_281_000), "2026-10-09T13:08:01Z");
        assert_eq!(iso_time(951_868_799_000), "2000-02-29T23:59:59Z");
    }
}

#[cfg(test)]
pub(crate) mod testing;
