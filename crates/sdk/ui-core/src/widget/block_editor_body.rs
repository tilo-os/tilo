//! A document's body: what [`BlockEditor`] edits when a view binds it to a field.
//!
//! A body holds a document, in either form the language gives it: a `Text` field holds it as
//! Markdown, so it stays readable text to every other view and to agents; a `RichText` field
//! holds it as its paragraphs, each with its kind and marks. The editor shows either as blocks,
//! and each edit the person makes is written back as the value now reads — Markdown for the
//! one, the document for the other — through the binding's own merge (text is spliced). A
//! change from elsewhere (a collaborator, an agent, undo) is shown when the person is not typing
//! in it.

use std::rc::Rc;

use tilo_ui_graph::rich::RichText;

use crate::component::{Bound, Value};
use crate::presence::{PathSegment, PresenceKey};
use crate::widget::block_editor::FindReplace;
use crate::widget::rich_doc;
use crate::widget::{Block, BlockContent, BlockEditor, PresenceKeyFn};

/// What the field a body edits holds.
#[derive(Clone)]
pub(super) enum Stored {
    /// A document written as Markdown, as a `Text` field holds it.
    Markdown(Bound<String>),
    /// A document written as its paragraphs, as a `RichText` field holds it.
    Rich(Bound<RichText>),
}

impl Stored {
    fn of(&self) -> Held {
        match self {
            Self::Markdown(value) => Held::Markdown(value.state.with(Clone::clone)),
            Self::Rich(value) => Held::Rich(value.state.with(Clone::clone)),
        }
    }

    fn revision(&self) -> u64 {
        match self {
            Self::Markdown(value) => value.state.revision(),
            Self::Rich(value) => value.state.revision(),
        }
    }
}

/// A document as one moment holds it: what the editor shows and what was last written.
#[derive(Clone, PartialEq)]
enum Held {
    Markdown(String),
    Rich(RichText),
}

/// The document a body editor shows and edits: the field it is bound to, what it showed when
/// it last read it, and the revision it read.
pub(super) struct BodyBinding {
    field: Stored,
    shown: Held,
    seen: u64,
}

impl BodyBinding {
    /// Whether a view bound the editor to a document rather than showing rich text: the
    /// binding the view named is the one that carries an edit.
    pub(super) fn bound(rich: &Bound<RichText>, markdown: &Bound<String>) -> bool {
        rich.edits.is_some() || markdown.edits.is_some()
    }

    /// The body bound to `rich` or `markdown`, whichever the view bound, and the hint shown
    /// while it is empty.
    pub(super) fn of(
        rich: Bound<RichText>,
        markdown: Bound<String>,
        placeholder: Option<String>,
    ) -> (Self, String) {
        let field = match (rich.edits.is_some(), markdown.edits.is_some()) {
            (false, true) => Stored::Markdown(markdown),
            _ => Stored::Rich(rich),
        };
        let shown = field.of();
        let seen = field.revision();
        (Self { field, shown, seen }, placeholder.unwrap_or_default())
    }

    /// The field is now the props' binding (`set_props`): what it showed stays as it was.
    pub(super) fn rebind(&mut self, rich: Bound<RichText>, markdown: Bound<String>) {
        self.field = match &self.field {
            Stored::Markdown(_) => Stored::Markdown(markdown),
            Stored::Rich(_) => Stored::Rich(rich),
        };
    }

    /// Where the body's blocks are for presence: the resource and the field they edit.
    pub(super) fn presence_path(&self) -> &[PathSegment] {
        match &self.field {
            Stored::Markdown(value) => &value.path,
            Stored::Rich(value) => &value.path,
        }
    }

    /// Report what the person changed since the last look, as the value the blocks now are.
    fn take_edits(&mut self, editor: &mut BlockEditor) -> bool {
        if editor.take_edit_ops().is_empty() {
            return false;
        }
        let now = match &self.shown {
            Held::Markdown(shown) => {
                let markdown = editor.to_markdown();
                if markdown == *shown {
                    return false;
                }
                Held::Markdown(markdown)
            }
            Held::Rich(shown) => {
                let blocks: Vec<BlockContent> =
                    editor.blocks().iter().map(Block::content).collect();
                let rich = rich_doc::rich_of_blocks(&blocks, shown);
                if rich == *shown {
                    return false;
                }
                Held::Rich(rich)
            }
        };
        match (&self.field, &now) {
            (Stored::Markdown(value), Held::Markdown(markdown)) => {
                value.state.set(markdown.clone());
                value.edited(Value::Text(markdown.clone()));
            }
            (Stored::Rich(value), Held::Rich(rich)) => {
                value.state.set(rich.clone());
                value.edited(Value::Rich(rich.clone()));
            }
            _ => return false,
        }
        self.shown = now;
        self.seen = self.field.revision();
        true
    }

    /// Apply what the window's search asked to replace, as the value the blocks now read:
    /// one edit of the field, recorded and undone as the person's own typing is. `false` when
    /// nothing waited, or what the search found moved on since it found it.
    fn take_find_replace(&mut self, editor: &mut BlockEditor) -> bool {
        let Some(request) = editor.take_find_replace() else {
            return false;
        };
        let (matches, replacement) = match request {
            FindReplace::One(target, with) => {
                // The block the search found, as the editor holds it now.
                let block = editor
                    .blocks()
                    .iter()
                    .position(|held| held.content() == target.block);
                let Some(block) = block else {
                    return false;
                };
                (vec![(block, target.start_byte..target.end_byte)], with)
            }
            FindReplace::All(batch) => (batch.matches, batch.replacement),
        };
        let now = match &self.shown {
            Held::Markdown(markdown) => {
                let mut blocks = blocks_of(&Held::Markdown(markdown.clone()));
                if !replaced_blocks(&mut blocks, &matches, &replacement) {
                    return false;
                }
                let mut written = BlockEditor::new();
                written.set_blocks(blocks);
                Held::Markdown(written.to_markdown())
            }
            Held::Rich(rich) => {
                let mut blocks: Vec<BlockContent> = rich_doc::blocks_as_written(rich);
                if !replaced_paragraphs(&mut blocks, &matches, &replacement) {
                    return false;
                }
                Held::Rich(rich_doc::rich_of_blocks(&blocks, rich))
            }
        };
        match (&self.field, &now) {
            (Stored::Markdown(value), Held::Markdown(markdown)) => {
                value.state.set(markdown.clone());
                value.edited(Value::Text(markdown.clone()));
            }
            (Stored::Rich(value), Held::Rich(rich)) => {
                value.state.set(rich.clone());
                value.edited(Value::Rich(rich.clone()));
            }
            _ => return false,
        }
        self.shown = now;
        self.seen = self.field.revision();
        editor.set_blocks(blocks_of(&self.shown));
        true
    }

    /// Show a value the field got from elsewhere, unless the person is typing here.
    fn follow(&mut self, editor: &mut BlockEditor) -> bool {
        let revision = self.field.revision();
        if revision == self.seen || editor.is_focused() {
            return false;
        }
        self.seen = revision;
        let held = self.field.of();
        if held == self.shown {
            return false;
        }
        editor.set_blocks(blocks_of(&held));
        self.shown = held;
        true
    }
}

/// `held` as the editor's blocks.
fn blocks_of(held: &Held) -> Vec<Block> {
    match held {
        Held::Markdown(markdown) => {
            std::mem::take(BlockEditor::from_markdown(markdown).blocks_mut())
        }
        Held::Rich(rich) => rich_doc::blocks_as_written(rich)
            .into_iter()
            .map(Block::from_content)
            .collect(),
    }
}

impl BlockEditor {
    /// Show the document the body is bound to, its blocks keyed for presence where the field
    /// lives: a collaborator's caret in the same body shows here.
    pub(super) fn show_body(&mut self, body: &BodyBinding) {
        self.set_blocks(blocks_of(&body.shown));
        if let Some(presence) = presence_of(body.presence_path()) {
            self.set_presence_key(presence);
        }
    }

    /// Take the props' binding in place: `false` when the empty hint names another field, so
    /// the instance is built again.
    pub(super) fn rebind_body(
        &mut self,
        rich: Bound<RichText>,
        markdown: Bound<String>,
        placeholder: &str,
    ) -> bool {
        let Some(mut body) = self.body.take() else {
            return false;
        };
        if placeholder != self.empty_hint() {
            self.body = Some(body);
            return false;
        }
        body.rebind(rich, markdown);
        if let Some(presence) = presence_of(body.presence_path()) {
            self.set_presence_key(presence);
        }
        self.body = Some(body);
        let _ = self.follow_body();
        true
    }

    /// Report what the person changed in the document it is bound to, as the value the field
    /// now holds: what they typed, and what the window's search replaced.
    pub(super) fn take_body_edits(&mut self) -> bool {
        let Some(mut body) = self.body.take() else {
            return false;
        };
        let replaced = body.take_find_replace(self);
        let edited = body.take_edits(self);
        self.body = Some(body);
        replaced || edited
    }

    /// Show the document the field got from elsewhere, unless the person is typing here.
    pub(super) fn follow_body(&mut self) -> bool {
        let Some(mut body) = self.body.take() else {
            return false;
        };
        let followed = body.follow(self);
        self.body = Some(body);
        followed
    }
}

/// Replace the bytes `matches` name of each block's text with `replacement`: every match must
/// hold the text the search found, and stand on character boundaries, or nothing changes.
fn replaced_blocks(
    blocks: &mut [Block],
    matches: &[(usize, std::ops::Range<usize>)],
    replacement: &str,
) -> bool {
    let mut texts: Vec<String> = blocks.iter().map(|block| block.text().to_owned()).collect();
    if !replaced(&mut texts, matches, replacement) {
        return false;
    }
    for (block, text) in blocks.iter_mut().zip(texts) {
        if block.text() != text {
            block.line.set_text(&text);
        }
    }
    true
}

/// [`replaced_blocks`] over a document's paragraphs.
fn replaced_paragraphs(
    blocks: &mut [BlockContent],
    matches: &[(usize, std::ops::Range<usize>)],
    replacement: &str,
) -> bool {
    let mut texts: Vec<String> = blocks.iter().map(|block| block.text.clone()).collect();
    if !replaced(&mut texts, matches, replacement) {
        return false;
    }
    for (block, text) in blocks.iter_mut().zip(texts) {
        block.text = text;
    }
    true
}

/// `texts` with the bytes `matches` name replaced by `replacement`, each block's latest first
/// so every range still stands where the search found it: whether they did, all of them
/// holding the one text the search matched.
fn replaced(
    texts: &mut [String],
    matches: &[(usize, std::ops::Range<usize>)],
    replacement: &str,
) -> bool {
    if matches.is_empty() {
        return false;
    }
    let mut ranged: std::collections::BTreeMap<usize, Vec<std::ops::Range<usize>>> =
        std::collections::BTreeMap::new();
    for (block, range) in matches {
        ranged.entry(*block).or_default().push(range.clone());
    }
    let mut matched: Option<String> = None;
    for (block, mut ranges) in ranged {
        let Some(text) = texts.get_mut(block) else {
            return false;
        };
        ranges.sort_by_key(|range| range.start);
        for range in ranges.iter().rev() {
            if range.start > range.end
                || range.end > text.len()
                || !text.is_char_boundary(range.start)
                || !text.is_char_boundary(range.end)
            {
                return false;
            }
            let Some(found) = text.get(range.clone()) else {
                return false;
            };
            if matched.as_ref().is_some_and(|seen| *seen != found) {
                return false;
            }
            matched = Some(found.to_owned());
            text.replace_range(range.clone(), replacement);
        }
    }
    matched.is_some()
}

/// Where the body's blocks are for presence under `path`, so a collaborator's caret in the
/// same body shows here.
fn presence_of(path: &[PathSegment]) -> Option<PresenceKeyFn> {
    (!path.is_empty()).then(|| {
        let path = path.to_vec();
        Rc::new(move |block: &str| {
            let mut path = path.clone();
            path.push(PathSegment::Field(block.to_owned()));
            PresenceKey::Widget { path }
        }) as PresenceKeyFn
    })
}
