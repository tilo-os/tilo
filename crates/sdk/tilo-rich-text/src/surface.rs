//! The block editor taking part in rich text.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use qdoc::snapshot::BlockView;
use qdoc_rich_text::{ChangeCursor, RichText, RichTextAction, RichTextState};
use ui::automerge::{AutoCommit, ChangeHash, ObjId};
use ui::framework::{ResourceSurface, SurfaceCx};
use ui::tilo_actions::ActionError;
use ui::tilo_resource::Doc;
use ui::widget::{Block, BlockContent, BlockEditor, Decorator, EditScope, EventCtx, Widget};
use ui::winit::event::WindowEvent;

use crate::convert::content_of;
use crate::edit::{self, Host};
use crate::source::{Hosted, RichTextField, RichTextSource};

/// The toolkit's [`BlockEditor`] showing rich text and editing it through
/// the document's own actions.
///
/// Every keystroke is a [`RichTextAction`] its source applies: in a
/// framework app ([`RichTextViews::rich_text`](crate::RichTextViews::rich_text))
/// the host permission-checks, attributes and replicates it; for rich text
/// inside another resource ([`RichTextSurface::field`]) the owning
/// resource's action carries it. Either way typing a word is one undo step;
/// a collaborator's edit, an undo or a restore is presented without moving
/// the caret of the person typing; a participant who may only view can
/// read, select and copy, and their edits never reach the document; and
/// collaborators' carets are keyed to paragraphs rather than positions on
/// screen. In a framework app, History mode shows past versions in the same
/// editor, read-only.
pub struct RichTextSurface<S = Hosted> {
    editor: BlockEditor,
    source: S,
    /// What the editor was last shown, and the paragraph each entry was
    /// read from: an entry is converted again only when its paragraph
    /// changed.
    presented: Vec<BlockContent>,
    sources: Vec<Rc<BlockView>>,
    /// Where the document's change history stood when it was presented:
    /// the next presentation converts only the paragraphs changed since.
    cursor: Option<ChangeCursor>,
    revision: Option<u64>,
    keyed: bool,
}

#[cfg(test)]
thread_local! {
    /// Blocks compared with what was presented and paragraphs presented,
    /// for the test that pins a keystroke's work.
    pub(crate) static WORK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
fn count_work(compared: usize, presented: usize) {
    WORK.with(|work| {
        let (c, p) = work.get();
        work.set((c.saturating_add(compared), p.saturating_add(presented)));
    });
}

#[cfg(not(test))]
fn count_work(_compared: usize, _presented: usize) {}

impl Default for RichTextSurface {
    fn default() -> Self {
        Self::new()
    }
}

fn default_editor() -> BlockEditor {
    BlockEditor::new().with_wrap_lines(true)
}

impl RichTextSurface {
    /// An editor over a framework app's rich-text resource.
    pub fn new() -> Self {
        Self::over(Hosted::default())
    }
}

impl<A: ui::tilo_actions::Action + serde::Serialize> RichTextSurface<RichTextField<A>> {
    /// An editor over rich text stored in a field of another resource.
    pub fn field(field: RichTextField<A>) -> Self {
        Self::over(field)
    }
}

impl<S: RichTextSource> RichTextSurface<S> {
    /// An editor over rich text from `source`.
    pub fn over(source: S) -> Self {
        Self {
            editor: default_editor(),
            source,
            presented: Vec::new(),
            sources: Vec::new(),
            cursor: None,
            revision: None,
            keyed: false,
        }
    }

    /// Present the document in an editor configured by the caller (its
    /// empty hint, spacing, image store).
    #[must_use]
    pub fn with_editor(mut self, editor: BlockEditor) -> Self {
        self.editor = editor;
        self.revision = None;
        self.cursor = None;
        self.keyed = false;
        self
    }

    /// The hint an empty document shows.
    #[must_use]
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.editor.set_empty_hint(hint);
        self
    }

    /// Show the document as `doc` holds it now, without writing to it: for a
    /// view shown before its first sync (a page just opened, a past version).
    /// `false` when the source cannot read without writing, or holds no
    /// rich text there.
    pub fn present_from(&mut self, doc: &Doc) -> bool {
        if !self.source.read_from(doc) {
            return false;
        }
        self.present_state(None);
        true
    }

    /// Whether the editor's block `id` is a paragraph of the document, not
    /// the empty line an empty document shows.
    #[must_use]
    pub fn shows_stored(&self, id: &str) -> bool {
        self.presented.iter().any(|block| block.id == id)
    }

    /// The editor, for what the surface does not configure itself.
    pub fn editor(&self) -> &BlockEditor {
        &self.editor
    }

    /// The editor, mutably. What the caller changes through it is compared
    /// with the document whole on the next commit.
    pub fn editor_mut(&mut self) -> &mut BlockEditor {
        self.editor.note_edit_anywhere();
        &mut self.editor
    }

    /// Where the text comes from.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// Where the text comes from, mutably (its permissions).
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    /// The blocks the person changed since the last presentation: the
    /// presented range `[start, presented_end)` became the editor's
    /// `[start, edited_end)`. `None` when nothing changed. Only the blocks
    /// the editor says the person may have changed are compared, so typing
    /// in a paragraph costs a comparison of that paragraph and its
    /// neighbours; anything else is found by comparing from both ends.
    fn changed(&mut self) -> Option<(usize, usize, usize)> {
        let scope = self.editor.take_edit_scope();
        let blocks = self.editor.blocks();
        let same = |block: &Block, shown: &BlockContent| {
            block.id == shown.id
                && block.kind == shown.kind
                && block.text() == shown.text
                && block.line.marks() == shown.marks.as_slice()
                && block.nesting == shown.nesting
                && block.code_group == shown.code_group
        };
        match scope {
            EditScope::Nothing => return None,
            EditScope::Blocks(range) if blocks.len() == self.presented.len() => {
                let range = range.start.min(blocks.len())..range.end.min(blocks.len());
                count_work(range.len(), 0);
                let presented = &self.presented;
                let differs = |index: &usize| match (blocks.get(*index), presented.get(*index)) {
                    (Some(block), Some(shown)) => !same(block, shown),
                    _ => true,
                };
                let start = range.clone().find(differs)?;
                let end = range
                    .rev()
                    .find(differs)
                    .map_or(start, |last| last.saturating_add(1));
                return Some((start, end, end));
            }
            EditScope::Blocks(_) | EditScope::Anywhere => {}
        }
        count_work(blocks.len(), 0);
        let start = blocks
            .iter()
            .zip(&self.presented)
            .take_while(|(block, shown)| same(block, shown))
            .count();
        if start == blocks.len() && start == self.presented.len() {
            return None;
        }
        let limit = blocks.len().min(self.presented.len()).saturating_sub(start);
        let suffix = blocks
            .iter()
            .rev()
            .zip(self.presented.iter().rev())
            .take(limit)
            .take_while(|(block, shown)| same(block, shown))
            .count();
        Some((
            start,
            self.presented.len().saturating_sub(suffix),
            blocks.len().saturating_sub(suffix),
        ))
    }

    /// Show the document as it is now. `local` names editor blocks that may
    /// show an edit the document did not take (a refused one), to be shown
    /// as the document has them.
    fn present_state(&mut self, local: Option<&Range<usize>>) {
        if !self.keyed
            && let Some(keys) = self.source.presence_keys()
        {
            self.editor.set_presence_key(keys);
            self.keyed = true;
        }
        if self.present_changes(local) {
            return;
        }
        let mut previous = Previous::new(
            std::mem::take(&mut self.sources),
            std::mem::take(&mut self.presented),
        );
        let shown = self.source.with_state(|state: &RichTextState| {
            state
                .body()
                .all()
                .filter_map(|id| state.block(id))
                .map(|view| match previous.take(view) {
                    Some(content) => (content, Rc::clone(view)),
                    None => (content_of(view), Rc::clone(view)),
                })
                .unzip::<_, _, Vec<BlockContent>, Vec<Rc<BlockView>>>()
        });
        let Some((presented, sources)) = shown else {
            return;
        };
        count_work(0, presented.len());
        self.revision = self.source.revision();
        self.cursor = self.source.with_state(RichTextState::change_cursor);
        self.editor.present_blocks(&presented);
        self.presented = presented;
        self.sources = sources;
    }

    /// Present only the paragraphs changed since the last presentation, and
    /// the editor blocks `local` names: `false`, presenting nothing, when
    /// that cannot be told (the body was reordered or read again, the editor
    /// holds other blocks) and the document must be presented whole.
    fn present_changes(&mut self, local: Option<&Range<usize>>) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        if self.editor.blocks().len() != self.presented.len()
            || self.sources.len() != self.presented.len()
        {
            return false;
        }
        let presented = &mut self.presented;
        let sources = &mut self.sources;
        let found = self.source.with_state(|state: &RichTextState| {
            let changed = state.changed_since(cursor)?;
            if state.body().len() != presented.len() {
                return None;
            }
            let mut indices = changed
                .iter()
                .map(|id| state.body().position(id))
                .collect::<Option<Vec<usize>>>()?;
            indices.extend(local.cloned().unwrap_or(0..0));
            indices.sort_unstable();
            indices.dedup();
            for &index in &indices {
                let id = state.body().id_at(index)?;
                let view = state.block(id)?;
                let source = sources.get_mut(index)?;
                if !Rc::ptr_eq(source, view) {
                    *presented.get_mut(index)? = content_of(view);
                    *source = Rc::clone(view);
                }
            }
            Some((indices, state.change_cursor()))
        });
        let Some(Some((indices, cursor))) = found else {
            return false;
        };
        count_work(0, indices.len());
        let changed: Vec<(usize, &BlockContent)> = indices
            .iter()
            .filter_map(|&index| Some((index, self.presented.get(index)?)))
            .collect();
        if self.editor.present_changed(&changed).is_none() {
            return false;
        }
        self.revision = self.source.revision();
        self.cursor = Some(cursor);
        true
    }

    /// Apply what the person did since the last presentation, then show the
    /// document as the edit left it. A participant who may only view is
    /// shown the document again, as if their edit never happened.
    fn commit(&mut self, doc: &mut AutoCommit) {
        let Some((start, presented_end, edited_end)) = self.changed() else {
            return;
        };
        if !self.source.read_only() && self.source.revision().is_some() {
            // The changed blocks and one on either side: a merge reaches
            // back into the block before, a split names the one after.
            let from = start.saturating_sub(1);
            let presented = self
                .presented
                .get(from..presented_end.saturating_add(1).min(self.presented.len()))
                .unwrap_or(&[]);
            let edited: Vec<BlockContent> = self
                .editor
                .blocks()
                .get(from..edited_end.saturating_add(1).min(self.editor.blocks().len()))
                .unwrap_or(&[])
                .iter()
                .map(Block::content)
                .collect();
            let author = self.editor.local_author().map(str::to_owned);
            let mut host = Through(&mut self.source, author);
            match edit::commit(&mut host, doc, presented, &edited) {
                Ok(renames) => rename(&mut self.editor, &renames),
                Err(error) => tracing::debug!(%error, "rich-text edit not applied"),
            }
            self.source.sync(doc);
        }
        let local = (presented_end == edited_end).then_some(start..edited_end);
        self.present_state(local.as_ref());
    }
}

/// What the editor was last shown, reused for paragraphs that did not
/// change: looked up by position first (the usual case, nothing moved), by
/// id only when the order changed.
struct Previous {
    entries: Vec<Option<(Rc<BlockView>, BlockContent)>>,
    next: usize,
    by_id: Option<HashMap<String, usize>>,
}

impl Previous {
    fn new(sources: Vec<Rc<BlockView>>, presented: Vec<BlockContent>) -> Self {
        Self {
            entries: sources.into_iter().zip(presented).map(Some).collect(),
            next: 0,
            by_id: None,
        }
    }

    /// The content shown for `view`, when `view` is the paragraph it was
    /// converted from.
    fn take(&mut self, view: &Rc<BlockView>) -> Option<BlockContent> {
        let at = self.next;
        let here = self
            .entries
            .get(at)
            .and_then(Option::as_ref)
            .is_some_and(|(source, _)| source.id == view.id);
        let index = if here {
            at
        } else {
            let entries = &self.entries;
            let by_id = self.by_id.get_or_insert_with(|| {
                entries
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| Some((entry.as_ref()?.0.id.clone(), index)))
                    .collect()
            });
            *by_id.get(view.id.as_str())?
        };
        self.next = index.saturating_add(1);
        let (source, content) = self.entries.get_mut(index)?.take()?;
        Rc::ptr_eq(&source, view).then_some(content)
    }
}

/// Give editor blocks the ids the document minted for them. Two steps, so a
/// chain of renames never collides with an id still in use.
fn rename(editor: &mut BlockEditor, renames: &[(String, String)]) {
    let parked: Vec<(String, &str)> = renames
        .iter()
        .enumerate()
        .filter_map(|(index, (from, to))| {
            let park = format!("\u{0}renaming-{index}");
            editor
                .rename_block(from, &park)
                .then_some((park, to.as_str()))
        })
        .collect();
    for (park, to) in parked {
        editor.rename_block(&park, to);
    }
}

/// A source as the edit host: edits applied through it, results read back.
struct Through<'a, S>(&'a mut S, Option<String>);

impl<S: RichTextSource> Host for Through<'_, S> {
    fn apply(&mut self, doc: &mut Doc, action: &RichTextAction) -> Result<(), ActionError> {
        self.0.apply(doc, action)
    }

    fn read<T>(&mut self, doc: &mut Doc, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        self.0.sync(doc);
        self.0.with_state(read)
    }

    fn author(&self) -> Option<&str> {
        self.1.as_deref()
    }
}

impl ResourceSurface<RichText> for RichTextSurface {
    /// The surface reads the live state when it syncs; a new state only
    /// asks for the next sync to present it.
    fn present(&mut self, _state: &Rc<RichTextState>) {
        self.revision = None;
    }

    fn attach(&mut self, cx: SurfaceCx<RichText>) {
        self.source.attach(cx.live(), cx.dispatcher());
        self.revision = None;
        self.cursor = None;
        self.keyed = false;
    }
}

impl<S: RichTextSource + 'static> Decorator for RichTextSurface<S> {
    type Inner = BlockEditor;

    fn inner(&self) -> &BlockEditor {
        &self.editor
    }

    fn inner_mut(&mut self) -> &mut BlockEditor {
        &mut self.editor
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.source.sync(state);
        // An edit made outside an input event (an accessibility action).
        self.commit(state);
        if self.source.revision() != self.revision {
            self.present_state(None);
        }
        self.editor.sync(state, path);
    }

    fn handle_event_ctx(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.editor.handle_event(
            event,
            ctx.scale_factor,
            ctx.modifiers,
            ctx.state,
            ctx.path,
            ctx.handled,
            ctx.focus_target,
            ctx.node_id,
            ctx.taffy,
        );
        self.commit(ctx.state);
    }

    // Every edit is an action bounded into undo steps by its source or the
    // host; the editor's own commit requests would close a word mid-typing.
    fn needs_commit(&self) -> bool {
        false
    }

    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.source.take_committed()
    }

    fn keeps_undo_step_open(&self) -> bool {
        self.source.keeps_step_open()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qdoc_rich_text::RICH_TEXT_ACTION_TYPE;
    use ui::automerge::ROOT;
    use ui::headless::FrameHarness;
    use ui::tilo_actions::Action;

    /// Blocks compared and paragraphs presented while typing `typed` into
    /// the first of `paragraphs` paragraphs.
    fn typing_work(paragraphs: usize, typed: &str) -> (usize, usize) {
        let mut state = ui::State::new();
        RichText::initialize_under(&mut state.doc, &ROOT).unwrap();
        let mut markdown = String::new();
        for index in 0..paragraphs {
            use std::fmt::Write as _;
            let _ = write!(markdown, "Paragraph {index} of a long note.\n\n");
        }
        RichTextAction::ReplaceBody { markdown }
            .apply(&mut state.doc)
            .unwrap();
        state.doc.commit();
        let field = RichTextField::new(RICH_TEXT_ACTION_TYPE, |_| Some(ROOT), |edit| edit);
        let surface = RichTextSurface::field(field);
        let mut harness = FrameHarness::new(state, Box::new(surface), (800, 600));
        harness.click_at(40.0, 10.0);
        harness.type_text("warm ");
        WORK.with(|work| work.set((0, 0)));
        harness.type_text(typed);
        WORK.with(std::cell::Cell::get)
    }

    #[test]
    fn a_keystroke_compares_and_presents_what_it_touched_not_the_document() {
        let small = typing_work(200, "typed");
        let large = typing_work(2000, "typed");
        assert!(
            small.0 > 0 && small.1 > 0,
            "the typing reached the document: {small:?}"
        );
        assert_eq!(
            small, large,
            "blocks compared and paragraphs presented per keystroke grow with the document"
        );
        assert!(
            small.0 <= 3 * 2 * "typed".len() && small.1 <= 3 * 2 * "typed".len(),
            "a keystroke compares and presents its paragraph and neighbours: {small:?}"
        );
    }
}
