//! The Writer document: its storage, the operations people, agents and
//! scripts take on it, and the objects inside it they are offered on.
//!
//! Typing, deleting, formatting and paragraph styles are edits of the body
//! field ([`Document::paths`]`().body`), in the rich-text vocabulary
//! ([`RichTextAction`]); the framework applies them at the body, coalesces a
//! typed word into one undo step and names them in history. What is more
//! than an edit of the text — a table, a review decision, a comment — is a
//! verb of the document.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use tilo_doc::snapshot::BlockView;
use tilo_document::action::{TableEdit, TocTarget};
use tilo_document::{ClipBlock, RichText, RichTextAction, RichTextState, TextAnchor};
use ui::tilo_resource::{
    ActionCx, Collection, Demand, Derived, Doc, Edit, Id, Model, ModelError, NonBlank, Place,
    Reactions, Refusal, Resource, resource,
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
    /// The reactions to the document, which its window's bar shows. A
    /// storage unit of its own, as the threads are: reacting never writes
    /// the text.
    #[model(unit)]
    pub reactions: Reactions,
}

/// A comment thread on some text of the document.
pub type Comment = tilo_document::annotation::Comment<TextAnchor>;
pub use tilo_document::annotation::Reply;

/// Bring a document of an older version up to this one. Version 1 kept a
/// reply's formatted paragraphs as one JSON string; they are a list of
/// paragraphs now. Versions 1 and 2 stored each mark's value in a comment's
/// paragraphs flat beside its variant's name; it is stored by its variant
/// now.
fn upgrade(doc: &mut Doc, from: u32) -> Result<(), ModelError> {
    use ui::tilo_resource::evolution::{collection_items, retype_json};
    if !(1..=2).contains(&from) {
        return Err(ModelError::invalid(format!(
            "no document migration from {from}"
        )));
    }
    if from == 1 {
        for comment in collection_items(doc, &ROOT, "comments")? {
            for reply in collection_items(doc, &comment, "replies")? {
                retype_json::<Vec<ClipBlock>>(doc, &reply, "body")?;
            }
        }
    }
    tilo_document::annotation::store_bodies_by_variant(doc, &ROOT, "comments")
}

impl Document {
    /// What the workspace lists of the document and search finds of it:
    /// the document by its first line (its heading, or its first
    /// paragraph), found by its words. A document with no text yet is not
    /// listed.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        let text = self.body.text();
        let Some(title) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
            return Vec::new();
        };
        vec![
            ui::tilo_resource::SearchEntry::new(title, text.clone())
                .anchored(<Self as Resource>::KIND)
                .with_icon("file-text"),
        ]
    }
}

#[resource(
    kind = "word-processor.document",
    label = "Document",
    children = Document::objects,
    load = Document::demand,
    locate = Document::locate,
    lands = Document::lands,
    answer = Document::answer,
    search = Document::search_entries,
    version = 3,
    migrate = upgrade,
)]
impl Document {
    /// Add a paragraph.
    ///
    /// A new paragraph holding the text, at the end of the document.
    #[action(answers, audience = "agents")]
    pub fn append_paragraph(&mut self, text: String) -> Result<(), Refusal> {
        if text.is_empty() {
            return Err(Refusal::new("the paragraph needs text"));
        }
        self.body.perform(RichTextAction::AppendParagraph { text });
        Ok(())
    }

    /// Replace the body.
    ///
    /// The whole body written again from Markdown: headings, lists, pipe
    /// tables, emphasis and links. Pictures are placed with place-image.
    #[action(answers, audience = "agents")]
    pub fn replace_body(
        &mut self,
        #[param("The whole new body as Markdown")] markdown: String,
    ) -> Result<(), Refusal> {
        let (action, inputs) = tilo_document::action::stage_import(
            tilo_document::action::ImportFormat::Markdown,
            markdown.as_bytes(),
        )
        .map_err(Refusal::new)?;
        if !inputs.is_empty() {
            return Err(Refusal::new(
                "a picture is not written into the Markdown: place it with place-image",
            ));
        }
        self.body.perform(action);
        Ok(())
    }

    /// Find and replace.
    ///
    /// Replace a phrase where it is in the document, the first occurrence
    /// or every one, keeping the rest of each paragraph, its marks and the
    /// other paragraphs. Refused, saying so, when the text is not there.
    #[action(answers, audience = "agents")]
    pub fn replace_text(
        &mut self,
        #[param("The exact text to find, within one paragraph, as read-text shows it")]
        find: String,
        #[param("The text that takes its place; empty deletes it")] replace: String,
        #[param("Replace every occurrence; only the first when left out")] all: Option<bool>,
    ) -> Result<(), Refusal> {
        if find.is_empty() {
            return Err(Refusal::new("name the text to find"));
        }
        self.body.perform(RichTextAction::ReplaceText {
            find,
            replace,
            all: all.unwrap_or(false),
        });
        Ok(())
    }

    /// Insert a table.
    ///
    /// An empty table of `rows` by `columns` after the paragraph `after`.
    #[action(answers, audience = "agents")]
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
    #[action(answers, audience = "agents")]
    pub fn add_row(&mut self, table: String) -> Result<(), Refusal> {
        let (rows, _) = self.table_size(&table)?;
        self.edit_table(table, TableEdit::InsertRow { at: rows });
        Ok(())
    }

    /// Add a column.
    ///
    /// A new empty column at the right of the table.
    #[action(answers, audience = "agents")]
    pub fn add_column(&mut self, table: String) -> Result<(), Refusal> {
        let (_, columns) = self.table_size(&table)?;
        self.edit_table(table, TableEdit::InsertColumn { at: columns });
        Ok(())
    }

    /// Delete a row.
    #[action(destructive, audience = "agents")]
    pub fn delete_row(&mut self, table: String, row: usize) {
        self.edit_table(table, TableEdit::DeleteRow { row });
    }

    /// Delete a column.
    #[action(destructive, audience = "agents")]
    pub fn delete_column(&mut self, table: String, column: usize) {
        self.edit_table(table, TableEdit::DeleteColumn { column });
    }

    /// Delete a table.
    #[action(destructive, audience = "agents")]
    pub fn delete_table(&mut self, table: String) {
        self.edit_table(table, TableEdit::Delete);
    }

    /// Delete an image.
    #[action(destructive, audience = "agents")]
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
    #[action(writes = comments, audience = "agents")]
    pub fn add_comment(
        &mut self,
        cx: &mut ActionCx,
        block: String,
        start: usize,
        end: usize,
        text: NonBlank,
    ) -> Result<(), Refusal> {
        let anchor = TextAnchor::new(&self.body, &block, start, end).map_err(Refusal::new)?;
        self.comments
            .push(cx.id(), Comment::written(cx, anchor, text.into_string()));
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
        text: NonBlank,
        body: Option<Vec<ClipBlock>>,
    ) -> Result<(), Refusal> {
        let id = cx.id();
        let reply = Reply {
            body: body.unwrap_or_default(),
            ..Reply::written(cx, text.into_string())
        };
        let thread = self
            .comments
            .get_mut(comment)
            .ok_or_else(|| Refusal::new("there is no such comment; it may have been resolved"))?;
        thread.replies.push(id, reply);
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
            let mut thread = Comment::new(
                anchor,
                comment.author,
                comment.date,
                joined(&comment.blocks),
            );
            for reply in &comment.replies {
                thread.replies.push(
                    cx.id(),
                    Reply {
                        author: reply.author.clone(),
                        date: reply.date.clone(),
                        text: joined(&reply.blocks),
                        body: reply.blocks.iter().map(clip_block).collect(),
                    },
                );
            }
            self.comments.push(cx.id(), thread);
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
    #[action(external, audience = "agents")]
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
    /// and tracked changes, each with the operations it offers. The very
    /// ones of the last call while what they are made of is the same, so a
    /// keystroke in a paragraph or reading more of a long document lists
    /// nothing again.
    fn objects(&self) -> Rc<[Derived<Self>]> {
        let Some(state) = self.body.state() else {
            return Rc::default();
        };
        OBJECTS.with_borrow_mut(|kept| kept.get_or_insert_with(Objects::default).follow(state))
    }

    /// The document's tables, each with its cells, then its images.
    fn tables_and_images(state: &RichTextState) -> Vec<Derived<Self>> {
        let mut objects = Vec::new();
        let mut tables: Vec<(&str, Vec<&Rc<BlockView>>)> = Vec::new();
        let mut table_at: HashMap<&str, usize> = HashMap::new();
        let cells = state.featured("cell");
        for cell in &cells {
            if let Some(table) = attr_str(cell, "table") {
                let at = *table_at.entry(table).or_insert_with(|| {
                    tables.push((table, Vec::new()));
                    tables.len() - 1
                });
                if let Some((_, cells)) = tables.get_mut(at) {
                    cells.push(cell);
                }
            }
        }
        for (table, cells) in tables.into_iter().take(MAX_OBJECTS) {
            let mut node = Derived::new("Table")
                .of_kind("table")
                .identified(format!("table:{table}"))
                .with(Self::ADD_ROW.bind(document::AddRow::paths().table, table.to_owned()))
                .with(Self::ADD_COLUMN.bind(document::AddColumn::paths().table, table.to_owned()))
                .with(
                    Self::DELETE_TABLE.bind(document::DeleteTable::paths().table, table.to_owned()),
                );
            // Each cell goes to its text, and deletes its row or column.
            for cell in cells {
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
                            .bind(document::DeleteRow::paths().table, table.to_owned())
                            .and(document::DeleteRow::paths().row, row),
                    )
                    .with(
                        Self::DELETE_COLUMN
                            .bind(document::DeleteColumn::paths().table, table.to_owned())
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
        objects
    }

    /// The tracked changes, each to accept or reject.
    fn tracked_changes(state: &RichTextState) -> Vec<Derived<Self>> {
        state
            .changes()
            .iter()
            .take(MAX_OBJECTS)
            .map(|change| {
                let label = match change.kind {
                    tilo_doc::snapshot::ChangeKind::Ins => "Insertion",
                    tilo_doc::snapshot::ChangeKind::Del => "Deletion",
                };
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
                    )
            })
            .collect()
    }

    /// The first paragraphs and headings, by their first words: the places
    /// an edit is told to act at (`at: {"item": <paragraph>}`). Headings are
    /// always read; a paragraph once it has been loaded.
    fn paragraph_places(state: &RichTextState) -> Vec<Derived<Self>> {
        state
            .body()
            .ids(0..PLACED_PARAGRAPHS)
            .filter_map(|id| placed_block(state, id))
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

thread_local! {
    static OBJECTS: RefCell<Option<Objects>> = const { RefCell::new(None) };
}

/// The objects [`Document::objects`] listed last, in their parts, and what
/// each part was made of: the tables and images of the featured blocks in
/// paragraph order, the places of the first paragraphs as read, the tracked
/// changes. Held weakly, so the state still changes them in place.
struct Objects {
    featured: Weak<HashMap<String, Rc<BlockView>>>,
    body: Weak<tilo_document::BodyOrder>,
    tables_and_images: Vec<Derived<Document>>,
    placed: Vec<Option<Weak<BlockView>>>,
    places: Vec<Derived<Document>>,
    changes: Weak<Vec<tilo_doc::snapshot::ChangeView>>,
    tracked_changes: Vec<Derived<Document>>,
    listed: Option<Rc<[Derived<Document>]>>,
}

impl Default for Objects {
    fn default() -> Self {
        Self {
            featured: Weak::new(),
            body: Weak::new(),
            tables_and_images: Vec::new(),
            placed: Vec::new(),
            places: Vec::new(),
            changes: Weak::new(),
            tracked_changes: Vec::new(),
            listed: None,
        }
    }
}

impl Objects {
    /// The objects of `state`, listing again only the parts whose making
    /// moved.
    fn follow(&mut self, state: &RichTextState) -> Rc<[Derived<Document>]> {
        let same_body = Weak::as_ptr(&self.body) == Rc::as_ptr(state.body());
        let mut moved = self.listed.is_none();
        if !same_body || Weak::as_ptr(&self.featured) != Rc::as_ptr(state.featured_blocks()) {
            self.featured = Rc::downgrade(state.featured_blocks());
            self.tables_and_images = Document::tables_and_images(state);
            moved = true;
        }
        let placed: Vec<Option<&Rc<BlockView>>> = state
            .body()
            .ids(0..PLACED_PARAGRAPHS)
            .map(|id| state.block(id))
            .collect();
        let same_placed = same_body
            && placed.len() == self.placed.len()
            && placed
                .iter()
                .zip(&self.placed)
                .all(|(now, was)| now.map(Rc::as_ptr) == was.as_ref().map(Weak::as_ptr));
        if !same_placed {
            self.placed = placed
                .into_iter()
                .map(|block| block.map(Rc::downgrade))
                .collect();
            self.places = Document::paragraph_places(state);
            moved = true;
        }
        self.body = Rc::downgrade(state.body());
        if Weak::as_ptr(&self.changes) != Rc::as_ptr(state.changes()) {
            self.changes = Rc::downgrade(state.changes());
            self.tracked_changes = Document::tracked_changes(state);
            moved = true;
        }
        match &self.listed {
            Some(objects) if !moved => Rc::clone(objects),
            _ => {
                let objects: Rc<[Derived<Document>]> = self
                    .tables_and_images
                    .iter()
                    .chain(&self.places)
                    .chain(&self.tracked_changes)
                    .cloned()
                    .collect();
                self.listed = Some(Rc::clone(&objects));
                objects
            }
        }
    }
}

/// The paragraph `id` as read, or the heading: what a place is made of.
fn placed_block<'a>(state: &'a RichTextState, id: &str) -> Option<&'a Rc<BlockView>> {
    state.block(id).or_else(|| {
        state
            .featured_blocks()
            .get(id)
            .filter(|block| block.kind == "heading")
    })
}

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
    if reply.body.is_empty() {
        return vec![paragraph(format!("{id}.0"), &reply.text)];
    }
    reply
        .body
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
        Comment::new(anchor, author.to_owned(), String::new(), text.to_owned()),
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
        Reply::new(author.to_owned(), date.to_owned(), text.to_owned()),
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

    /// A document with a comment and a reply to it written in
    /// `paragraphs`: a strong word, and a checked item.
    fn replied() -> (Replica<Document>, Vec<ClipBlock>) {
        let (mut replica, block) = written("The budget doubled this year.", "ada");
        replica
            .apply(Document::ADD_COMMENT.with(document::AddComment {
                block,
                start: 4,
                end: 10,
                text: "By how much exactly?".parse().unwrap(),
            }))
            .unwrap();
        let comment = replica
            .state()
            .unwrap()
            .comments
            .ids()
            .next()
            .unwrap()
            .clone();
        let paragraphs = vec![ClipBlock {
            kind: "paragraph".into(),
            text: "Twice, see the sheet.".into(),
            marks: vec![tilo_doc::document::MarkSpan {
                start: 0,
                end: 5,
                name: "strong".into(),
                value: tilo_doc::document::MarkValue::Bool(true),
            }],
            level: None,
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::from([(
                "checked".to_owned(),
                tilo_document::ClipAttr::Bool(true),
            )]),
        }];
        replica
            .apply(Document::REPLY_TO_COMMENT.with(document::ReplyToComment {
                comment,
                text: "Twice, see the sheet.".parse().unwrap(),
                body: Some(paragraphs.clone()),
            }))
            .unwrap();
        (replica, paragraphs)
    }

    /// A document from before a reply's formatted paragraphs were a list:
    /// they were one JSON string. Opened now, the reply reads with its
    /// paragraphs, formatting included.
    #[test]
    fn a_reply_kept_as_a_json_string_migrates_to_its_paragraphs() {
        use ui::tilo_resource::evolution::{self, collection_items};
        let (mut replica, paragraphs) = replied();
        // Back to the old shape: the paragraphs as one JSON string, version 1.
        let doc = replica.doc_mut();
        let comment = collection_items(doc, &ROOT, "comments").unwrap()[0].clone();
        let reply = collection_items(doc, &comment, "replies").unwrap()[0].clone();
        let body = ui::tilo_resource::Slot::Key(&reply, "body");
        body.remove(doc).unwrap();
        body.put_scalar(
            doc,
            automerge::ScalarValue::Str(serde_json::to_string(&paragraphs).unwrap().into()),
        )
        .unwrap();
        ui::tilo_resource::Slot::Key(&ROOT, evolution::VERSION_KEY)
            .remove(doc)
            .unwrap();
        doc.commit();
        let mut old = doc.fork();
        assert!(
            evolution::read_checked::<Document>(&old).is_err(),
            "unmigrated"
        );
        assert!(evolution::migrate::<Document>(&mut old).unwrap());
        let migrated = evolution::read_checked::<Document>(&old).unwrap();
        let thread = migrated.comments.values().next().unwrap();
        assert_eq!(thread.replies.values().next().unwrap().body, paragraphs);
    }

    /// A document of version 2 stored each mark's value and block attribute
    /// in a reply's paragraphs flat beside its variant's name. Opened now,
    /// the reply reads with the same formatting, stored by its variant, and
    /// the document is marked as the version a reader of version 2 refuses
    /// as newer rather than misreading.
    #[test]
    fn a_reply_kept_with_flat_mark_values_migrates_to_values_stored_by_variant() {
        use automerge::ReadDoc as _;
        use ui::tilo_resource::Model as _;
        use ui::tilo_resource::evolution::{self, collection_items};
        let (mut replica, paragraphs) = replied();
        let doc = replica.doc_mut();
        let comment = collection_items(doc, &ROOT, "comments").unwrap()[0].clone();
        let reply = collection_items(doc, &comment, "replies").unwrap()[0].clone();
        ui::tilo_resource::Slot::Key(&reply, "body")
            .remove(doc)
            .unwrap();
        evolution::fixture(
            doc,
            &reply,
            &serde_json::json!({ "body": paragraphs.to_json() }),
        )
        .unwrap();
        ui::tilo_resource::Slot::Key(&ROOT, evolution::VERSION_KEY)
            .put_scalar(doc, automerge::ScalarValue::Uint(2))
            .unwrap();
        doc.commit();
        let mut old = doc.fork();
        assert!(evolution::migrate::<Document>(&mut old).unwrap());
        assert_eq!(evolution::stored_version(&old), 3);
        let migrated = evolution::read_checked::<Document>(&old).unwrap();
        let thread = migrated.comments.values().next().unwrap();
        assert_eq!(thread.replies.values().next().unwrap().body, paragraphs);
        let (_, body) = old.get(&reply, "body").unwrap().unwrap();
        let (_, block) = old.get(&body, 0).unwrap().unwrap();
        let (_, marks) = old.get(&block, "marks").unwrap().unwrap();
        let (_, mark) = old.get(&marks, 0).unwrap().unwrap();
        let (_, value) = old.get(&mark, "value").unwrap().unwrap();
        let (_, attrs) = old.get(&block, "attrs").unwrap().unwrap();
        let (_, checked) = old.get(&attrs, "checked").unwrap().unwrap();
        for value in [value, checked] {
            assert!(
                old.keys(&value)
                    .all(|key| key == "$variant" || key.starts_with("$field/")),
                "a value is stored by its variant"
            );
        }
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
                text: "By how much exactly?".parse().unwrap(),
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
            text: "Friday is a holiday".parse().unwrap(),
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
                text: "Seven sharp?".parse().unwrap(),
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
}

#[cfg(test)]
pub(crate) mod testing;
