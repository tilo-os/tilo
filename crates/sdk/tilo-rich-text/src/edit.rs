//! What the person did to the blocks, as rich-text actions.
//!
//! The editor edits its own blocks: typing splices a line, Enter splits
//! one, Backspace at a line's start merges two, a toolbar toggle marks a
//! range. [`commit`] compares the blocks it was presented with the blocks it
//! holds after the input and applies the difference as the document's own
//! actions, reading back the paragraphs the document created — so every
//! edit is permission-checked, attributed, undoable and replayable like any
//! other, and the document keeps its own ids.
//!
//! Structure and text come first, in the forms that keep the most meaning:
//! a split stays a split ([`RichTextAction::SplitBlock`]) and a merge or a
//! deleted selection one [`RichTextAction::ReplaceSelection`]. Kinds and
//! marks are then brought to what the editor shows, only where the input
//! changed them, so the document's own rules (a mark growing over text
//! typed at its end) are never fought keystroke by keystroke. Last, blocks
//! the person dragged elsewhere are moved ([`RichTextAction::MoveBlocks`]),
//! the fewest that put the rest in the editor's order.

use std::collections::{HashMap, HashSet};

use tilo_doc::snapshot::BlockView;
use tilo_document::action::{RegionKind, Span};
use tilo_document::{RichTextAction, RichTextState};
use ui::tilo_actions::ActionError;
use ui::widget::marks::{self, MarkSpan};
use ui::widget::{BlockContent, BlockKind};

use crate::convert::{self, KindSpec, chars};

/// Where edits go and where their result is read back.
pub(crate) trait Host {
    /// The document the edits go to, as the host holds it.
    type Doc: ?Sized;

    /// Apply one edit to `doc` now.
    ///
    /// # Errors
    /// Returns the host's refusal; nothing is written.
    fn apply(&mut self, doc: &mut Self::Doc, action: &RichTextAction) -> Result<(), ActionError>;

    /// Read the state as of `doc`, after the edits applied so far.
    fn read<T>(&mut self, doc: &mut Self::Doc, read: impl FnOnce(&RichTextState) -> T)
    -> Option<T>;
}

/// `(at, removed, inserted)` in characters: the one splice turning `before`
/// into `after`.
pub(crate) fn char_splice(before: &str, after: &str) -> (usize, usize, String) {
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let (before_len, after_len) = (chars(before), chars(after));
    let limit = before_len.min(after_len).saturating_sub(prefix);
    let suffix = before
        .chars()
        .rev()
        .zip(after.chars().rev())
        .take(limit)
        .take_while(|(a, b)| a == b)
        .count();
    let removed = before_len.saturating_sub(prefix).saturating_sub(suffix);
    let inserted: String = after
        .chars()
        .skip(prefix)
        .take(after_len.saturating_sub(prefix).saturating_sub(suffix))
        .collect();
    (prefix, removed, inserted)
}

/// The splices turning `before` into `after`, each in characters and in
/// the order they apply. One, unless the edit kept a run and only removed
/// (or only added) text on both sides of it, as formatting typed markdown
/// does (`**bold**` becomes `bold`): then two, so the kept run stays the
/// text it was rather than being written again.
pub(crate) fn char_splices(before: &str, after: &str) -> Vec<(usize, usize, String)> {
    let (at, del, ins) = char_splice(before, after);
    if del == 0 || ins.is_empty() {
        return vec![(at, del, ins)];
    }
    let removed: String = before.chars().skip(at).take(del).collect();
    // Each offset is into the text as the splices before it left it.
    if let Some(found) = removed.find(ins.as_str()) {
        let lead = removed.get(..found).map_or(0, chars);
        let trail = del.saturating_sub(lead).saturating_sub(chars(&ins));
        return [(at, lead), (at.saturating_add(chars(&ins)), trail)]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(from, count)| (from, count, String::new()))
            .collect();
    }
    if let Some(found) = ins.find(removed.as_str()) {
        let lead = ins.get(..found).unwrap_or_default();
        let trail = ins
            .get(found.saturating_add(removed.len())..)
            .unwrap_or_default();
        let after_kept = at.saturating_add(chars(lead)).saturating_add(del);
        return [(at, lead), (after_kept, trail)]
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
            .map(|(from, text)| (from, 0, text.to_owned()))
            .collect();
    }
    vec![(at, del, ins)]
}

/// Where `before`'s marks are once its text became `after` through the
/// splices the commit writes ([`char_splices`]), each carrying the marks as
/// the document does ([`marks::carry_through_edit`]) and as the editor's
/// own lines do: an input that only typed or deleted leaves them there, and
/// any other difference is formatting the person changed.
fn carried_marks(before: &BlockContent, after: &str) -> Vec<MarkSpan> {
    let byte_at = |text: &str, at: usize| {
        text.char_indices()
            .nth(at)
            .map_or(text.len(), |(byte, _)| byte)
    };
    let mut text = before.text.clone();
    let mut spans = before.marks.clone();
    for (at, del, ins) in char_splices(&before.text, after) {
        let start = byte_at(&text, at);
        let end = byte_at(&text, at.saturating_add(del));
        marks::carry_through_edit(&mut spans, start, ins.len(), end.saturating_sub(start));
        text.replace_range(start..end, &ins);
    }
    spans
}

fn same_shape(a: &BlockContent, b: &BlockContent) -> bool {
    a.kind == b.kind && a.nesting == b.nesting && a.code_group == b.code_group
}

/// The kind a split's new paragraph is created as; the rest of its style
/// follows as a style edit.
fn split_kind(kind: &BlockKind) -> (&'static str, Option<bool>) {
    match kind {
        BlockKind::Bullet { .. } | BlockKind::Checklist { .. } => ("list_item", Some(false)),
        BlockKind::Numbered { .. } => ("list_item", Some(true)),
        _ => ("paragraph", None),
    }
}

fn body_neighbour(state: &RichTextState, id: &str, step: isize) -> Option<String> {
    let at = state.body().position(id)?;
    let index = at.checked_add_signed(step)?;
    state.body().id_at(index).map(str::to_owned)
}

struct Commit<'a, H: Host> {
    host: &'a mut H,
    doc: &'a mut H::Doc,
    /// The document id each editor block ended up with.
    resolved: HashMap<String, String>,
    /// Editor blocks whose text is already in the document.
    written: HashSet<String>,
    /// Editor blocks whose marks the input changed beyond carrying them.
    remark: HashSet<String>,
    /// Editor blocks now stored in a paragraph created for them, whose kind
    /// is to be brought to what the editor shows.
    reshape: HashSet<String>,
}

impl<H: Host> Commit<'_, H> {
    /// Apply `action`; the document attributes the text it writes to
    /// whoever invoked it.
    fn apply(&mut self, action: &RichTextAction) -> Result<(), ActionError> {
        self.host.apply(self.doc, action)
    }

    /// The paragraph right after `id` in the document now.
    fn after(&mut self, id: &str) -> Option<String> {
        self.host
            .read(self.doc, |state| body_neighbour(state, id, 1))
            .flatten()
    }

    /// The body's last paragraph now.
    fn last(&mut self) -> Option<String> {
        self.host
            .read(self.doc, |state| {
                let body = state.body();
                body.id_at(body.len().checked_sub(1)?).map(str::to_owned)
            })
            .flatten()
    }

    fn view(&mut self, id: &str) -> Option<BlockView> {
        self.host
            .read(self.doc, |state| {
                state.block(id).map(|view| (**view).clone())
            })
            .flatten()
    }

    fn doc_id<'b>(&'b self, editor_id: &'b str) -> &'b str {
        self.resolved
            .get(editor_id)
            .map_or(editor_id, String::as_str)
    }

    /// Enter: a new block right after a presented one, holding the text the
    /// presented one lost from its end.
    fn splits(
        &mut self,
        presented: &HashMap<&str, &BlockContent>,
        presented_next: &HashMap<&str, &str>,
        edited: &[BlockContent],
    ) -> Result<(), ActionError> {
        for pair in edited.windows(2) {
            let [before, new] = pair else {
                continue;
            };
            if presented.contains_key(new.id.as_str()) || self.written.contains(&before.id) {
                continue;
            }
            let Some(was) = presented.get(before.id.as_str()) else {
                continue;
            };
            let Some(tail) = was.text.strip_prefix(before.text.as_str()) else {
                continue;
            };
            if tail != new.text {
                continue;
            }
            let (kind, ordered) = split_kind(&new.kind);
            self.apply(&RichTextAction::SplitBlock {
                region: RegionKind::Body,
                block: before.id.clone(),
                at: chars(&before.text),
                kind: kind.to_owned(),
                ordered,
                next: presented_next
                    .get(before.id.as_str())
                    .map(|next| (*next).to_owned()),
            })?;
            if let Some(minted) = self.after(&before.id) {
                self.resolved.insert(new.id.clone(), minted);
            }
            self.written.insert(before.id.clone());
            self.written.insert(new.id.clone());
            self.remark.insert(new.id.clone());
            if !was.marks.is_empty() {
                self.remark.insert(before.id.clone());
            }
        }
        Ok(())
    }

    /// Backspace at a line's start, or a selection deleted across blocks:
    /// runs of presented blocks that are gone, merged into the block before
    /// them.
    fn merges(
        &mut self,
        presented: &[BlockContent],
        edited: &HashMap<&str, &BlockContent>,
    ) -> Result<(), ActionError> {
        let mut index = 0;
        while index < presented.len() {
            let Some(block) = presented.get(index) else {
                break;
            };
            if edited.contains_key(block.id.as_str()) {
                index = index.saturating_add(1);
                continue;
            }
            let start = index;
            while presented
                .get(index)
                .is_some_and(|gone| !edited.contains_key(gone.id.as_str()))
            {
                index = index.saturating_add(1);
            }
            let run = presented.get(start..index).unwrap_or(&[]);
            let into = start
                .checked_sub(1)
                .and_then(|at| presented.get(at))
                .filter(|keep| !self.written.contains(&keep.id))
                .and_then(|keep| Some((keep, *edited.get(keep.id.as_str())?)));
            match (into, run.last()) {
                (Some((was, now)), Some(last)) => {
                    let (at, _, _) = char_splice(&was.text, &now.text);
                    let at = at.min(chars(&was.text));
                    let rest: String = now.text.chars().skip(at).collect();
                    let keep = rest
                        .chars()
                        .rev()
                        .zip(last.text.chars().rev())
                        .take_while(|(a, b)| a == b)
                        .count();
                    let inserted: String = rest
                        .chars()
                        .take(chars(&rest).saturating_sub(keep))
                        .collect();
                    self.apply(&RichTextAction::ReplaceSelection {
                        region: RegionKind::Body,
                        start_block: was.id.clone(),
                        start: at,
                        end_block: last.id.clone(),
                        end: chars(&last.text).saturating_sub(keep),
                        ins: inserted,
                    })?;
                    self.written.insert(was.id.clone());
                    if run.iter().any(|gone| !gone.marks.is_empty()) || !was.marks.is_empty() {
                        self.remark.insert(was.id.clone());
                    }
                }
                _ => self.apply(&RichTextAction::RemoveBlocks {
                    blocks: run.iter().map(|gone| gone.id.clone()).collect(),
                })?,
            }
        }
        Ok(())
    }

    /// Typing and deleting inside a block.
    fn splices(
        &mut self,
        presented: &HashMap<&str, &BlockContent>,
        edited: &[BlockContent],
    ) -> Result<(), ActionError> {
        for block in edited {
            let Some(was) = presented.get(block.id.as_str()) else {
                continue;
            };
            if self.written.contains(&block.id) {
                continue;
            }
            if was.text != block.text {
                for (at, del, ins) in char_splices(&was.text, &block.text) {
                    self.apply(&RichTextAction::SpliceText {
                        region: RegionKind::Body,
                        block: block.id.clone(),
                        at,
                        del,
                        ins,
                    })?;
                }
            }
            if carried_marks(was, &block.text) != block.marks {
                self.remark.insert(block.id.clone());
            }
        }
        Ok(())
    }

    /// New blocks no split explains: a paste, a block inserted from a menu.
    fn inserts(
        &mut self,
        presented: &HashMap<&str, &BlockContent>,
        edited: &[BlockContent],
    ) -> Result<(), ActionError> {
        for (index, block) in edited.iter().enumerate() {
            if presented.contains_key(block.id.as_str()) || self.resolved.contains_key(&block.id) {
                continue;
            }
            let previous = index
                .checked_sub(1)
                .and_then(|at| edited.get(at))
                .map(|before| self.doc_id(&before.id).to_owned());
            if let Some(after) = previous {
                self.apply(&RichTextAction::InsertParagraph {
                    after: after.clone(),
                    text: block.text.clone(),
                })?;
                if let Some(minted) = self.after(&after) {
                    self.resolved.insert(block.id.clone(), minted);
                }
            } else {
                // Before the first paragraph: split the first one at its
                // start, and the paragraph that keeps its id is the new one.
                let Some(first) = edited
                    .iter()
                    .find(|next| presented.contains_key(next.id.as_str()))
                else {
                    // Nothing to split: the text is the whole body.
                    if !block.text.is_empty() {
                        self.apply(&RichTextAction::AppendParagraph {
                            text: block.text.clone(),
                        })?;
                        if let Some(minted) = self.last() {
                            self.resolved.insert(block.id.clone(), minted);
                        }
                        self.remark.insert(block.id.clone());
                    }
                    continue;
                };
                let first = self.doc_id(&first.id).to_owned();
                self.apply(&RichTextAction::SplitBlock {
                    region: RegionKind::Body,
                    block: first.clone(),
                    at: 0,
                    kind: "paragraph".to_owned(),
                    ordered: None,
                    next: None,
                })?;
                let Some(minted) = self.after(&first) else {
                    continue;
                };
                for resolved in self.resolved.values_mut() {
                    if *resolved == first {
                        resolved.clone_from(&minted);
                    }
                }
                let moved = edited
                    .iter()
                    .find(|next| self.doc_id(&next.id) == first)
                    .map(|next| next.id.clone());
                if let Some(moved) = moved {
                    self.resolved.insert(moved.clone(), minted);
                    self.remark.insert(moved.clone());
                    self.reshape.insert(moved);
                }
                self.resolved.insert(block.id.clone(), first.clone());
                if !block.text.is_empty() {
                    self.apply(&RichTextAction::SpliceText {
                        region: RegionKind::Body,
                        block: first,
                        at: 0,
                        del: 0,
                        ins: block.text.clone(),
                    })?;
                }
            }
            self.remark.insert(block.id.clone());
        }
        Ok(())
    }

    /// Bring each block the input touched to the kind and marks it shows.
    fn styles(
        &mut self,
        presented: &HashMap<&str, &BlockContent>,
        edited: &[BlockContent],
    ) -> Result<(), ActionError> {
        for block in edited {
            let reshaped = self.reshape.contains(&block.id)
                || presented
                    .get(block.id.as_str())
                    .is_none_or(|was| !same_shape(was, block));
            let remark = self.remark.contains(&block.id);
            if !reshaped && !remark {
                continue;
            }
            let id = self.doc_id(&block.id).to_owned();
            let Some(view) = self.view(&id) else {
                continue;
            };
            if reshaped {
                self.restyle(&id, block, &view)?;
            }
            if remark && view.text == block.text {
                self.remark_block(&id, block, &view)?;
            }
        }
        Ok(())
    }

    fn restyle(
        &mut self,
        id: &str,
        block: &BlockContent,
        view: &BlockView,
    ) -> Result<(), ActionError> {
        let spec = KindSpec::of(block);
        if !spec.same_style(view) {
            self.apply(&RichTextAction::SetParagraphStyle {
                region: RegionKind::Body,
                blocks: vec![id.to_owned()],
                kind: spec.kind.to_owned(),
                level: spec.level,
                ordered: spec.ordered,
            })?;
        }
        for (key, value) in spec.attr_changes(view) {
            let blocks = vec![id.to_owned()];
            self.apply(&match value {
                Some(value) => RichTextAction::SetBlockAttr {
                    region: RegionKind::Body,
                    blocks,
                    key: key.to_owned(),
                    value,
                },
                None => RichTextAction::ClearBlockAttr {
                    region: RegionKind::Body,
                    blocks,
                    key: key.to_owned(),
                },
            })?;
        }
        Ok(())
    }

    fn remark_block(
        &mut self,
        id: &str,
        block: &BlockContent,
        view: &BlockView,
    ) -> Result<(), ActionError> {
        let want = convert::editor_runs(block);
        let have = convert::document_runs(view);
        for (name, value, ranges) in convert::mark_runs(&want, &have) {
            self.apply(&RichTextAction::SetMark {
                region: RegionKind::Body,
                spans: ranges
                    .into_iter()
                    .map(|(start, end)| Span {
                        block: id.to_owned(),
                        start,
                        end,
                    })
                    .collect(),
                name: name.to_owned(),
                value,
                also_clear: Vec::new(),
            })?;
        }
        Ok(())
    }
}

/// For each position, whether it belongs to a longest increasing run of
/// `positions`: the blocks that stay put while the others move around them.
fn longest_increasing(positions: &[usize]) -> Vec<bool> {
    // Patience sorting: `tails[k]` is the index ending the best run of
    // length `k + 1`; `back` links each index to the one before it.
    let mut tails: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; positions.len()];
    for (index, position) in positions.iter().enumerate() {
        let at = tails.partition_point(|&tail| positions.get(tail).is_some_and(|p| p < position));
        if let Some(slot) = back.get_mut(index) {
            *slot = at
                .checked_sub(1)
                .and_then(|before| tails.get(before).copied());
        }
        if let Some(tail) = tails.get_mut(at) {
            *tail = index;
        } else {
            tails.push(index);
        }
    }
    let mut keep = vec![false; positions.len()];
    let mut next = tails.last().copied();
    while let Some(index) = next {
        if let Some(slot) = keep.get_mut(index) {
            *slot = true;
        }
        next = back.get(index).copied().flatten();
    }
    keep
}

impl<H: Host> Commit<'_, H> {
    /// Blocks the person moved: the fewest moves that bring the document's
    /// order to the editor's. `before` is the paragraph before the first
    /// edited one, `None` at the start of the body.
    fn reorders(
        &mut self,
        edited: &[BlockContent],
        before: Option<String>,
    ) -> Result<(), ActionError> {
        let ids: Vec<String> = edited
            .iter()
            .map(|block| self.doc_id(&block.id).to_owned())
            .collect();
        let positions = self
            .host
            .read(self.doc, |state| {
                ids.iter()
                    .map(|id| state.body().position(id))
                    .collect::<Option<Vec<usize>>>()
            })
            .flatten();
        let Some(positions) = positions else {
            return Ok(());
        };
        if positions.windows(2).all(|pair| pair.first() < pair.last()) {
            return Ok(());
        }
        let keep = longest_increasing(&positions);
        let mut previous = before;
        let mut index = 0;
        while let Some(id) = ids.get(index) {
            if keep.get(index).copied().unwrap_or(true) {
                previous = Some(id.clone());
                index = index.saturating_add(1);
                continue;
            }
            let start = index;
            while keep.get(index).is_some_and(|kept| !kept) {
                index = index.saturating_add(1);
            }
            let run = ids.get(start..index).unwrap_or(&[]).to_vec();
            let last = run.last().cloned();
            self.apply(&RichTextAction::MoveBlocks {
                blocks: run,
                after: previous.clone(),
            })?;
            previous = last;
        }
        Ok(())
    }
}

/// Apply what the person did — `edited` where the editor was presented
/// `presented` — as rich-text actions through `host`. Returns the editor
/// blocks the document named otherwise (`(editor id, document id)`), for
/// the editor to rename.
///
/// # Errors
/// Returns the first refusal. Edits applied before it stay applied; the
/// caller presents the document as it now is.
pub(crate) fn commit<H: Host>(
    host: &mut H,
    doc: &mut H::Doc,
    presented: &[BlockContent],
    edited: &[BlockContent],
) -> Result<Vec<(String, String)>, ActionError> {
    let by_id: HashMap<&str, &BlockContent> = presented
        .iter()
        .map(|block| (block.id.as_str(), block))
        .collect();
    let next: HashMap<&str, &str> = presented
        .windows(2)
        .filter_map(|pair| match pair {
            [a, b] => Some((a.id.as_str(), b.id.as_str())),
            _ => None,
        })
        .collect();
    let edited_by_id: HashMap<&str, &BlockContent> = edited
        .iter()
        .map(|block| (block.id.as_str(), block))
        .collect();
    let before = presented.first().and_then(|first| {
        host.read(doc, |state| body_neighbour(state, &first.id, -1))
            .flatten()
    });
    let mut commit = Commit {
        host,
        doc,
        resolved: HashMap::new(),
        written: HashSet::new(),
        remark: HashSet::new(),
        reshape: HashSet::new(),
    };
    commit.splits(&by_id, &next, edited)?;
    commit.merges(presented, &edited_by_id)?;
    commit.splices(&by_id, edited)?;
    commit.inserts(&by_id, edited)?;
    commit.styles(&by_id, edited)?;
    commit.reorders(edited, before)?;
    let mut renames: Vec<(String, String)> = commit
        .resolved
        .into_iter()
        .filter(|(from, to)| from != to)
        .collect();
    renames.sort();
    Ok(renames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_document::RichText;
    use ui::tilo_actions::Action;
    use ui::tilo_resource::Doc;
    use ui::tilo_resource::{Demand, Reader, Resource};
    use ui::widget::marks::Mark;

    /// A document and the actions applied to it, read back like the
    /// framework reads it.
    struct Replica {
        reader: Reader<RichText>,
        applied: Vec<RichTextAction>,
    }

    impl Host for Replica {
        type Doc = Doc;

        fn apply(&mut self, doc: &mut Doc, action: &RichTextAction) -> Result<(), ActionError> {
            action.apply(doc)?;
            doc.commit();
            self.applied.push(action.clone());
            Ok(())
        }

        fn read<T>(&mut self, doc: &mut Doc, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
            self.reader.sync(doc).unwrap();
            Some(read(self.reader.state()))
        }
    }

    fn document(markdown: &str) -> (Doc, Replica) {
        let mut doc = Doc::new();
        RichText::initialize(&mut doc).unwrap();
        if !markdown.is_empty() {
            RichTextAction::ReplaceBody {
                markdown: markdown.to_owned(),
            }
            .apply(&mut doc)
            .unwrap();
        }
        doc.commit();
        let mut reader = Reader::<RichText>::open(&mut doc).unwrap();
        reader.demand(&doc, Demand::range(0..usize::MAX)).unwrap();
        (
            doc,
            Replica {
                reader,
                applied: Vec::new(),
            },
        )
    }

    fn shown(replica: &mut Replica, doc: &mut Doc) -> Vec<BlockContent> {
        replica
            .read(doc, |state| {
                state
                    .body()
                    .all()
                    .filter_map(|id| state.block(id))
                    .map(|view| convert::content_of(view))
                    .collect()
            })
            .unwrap()
    }

    fn texts(replica: &mut Replica, doc: &mut Doc) -> Vec<String> {
        shown(replica, doc)
            .into_iter()
            .map(|block| block.text)
            .collect()
    }

    #[test]
    fn typing_is_one_splice() {
        let (mut doc, mut replica) = document("hello");
        let presented = shown(&mut replica, &mut doc);
        let mut edited = presented.clone();
        edited[0].text = "hello there".into();
        let renames = commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        assert!(renames.is_empty());
        assert_eq!(texts(&mut replica, &mut doc), ["hello there"]);
        assert!(matches!(
            replica.applied.as_slice(),
            [RichTextAction::SpliceText { at: 5, del: 0, ins, .. }] if ins == " there"
        ));
    }

    #[test]
    fn enter_is_a_split_and_the_new_paragraph_takes_the_documents_id() {
        let (mut doc, mut replica) = document("**bold** tail");
        let presented = shown(&mut replica, &mut doc);
        let mut edited = presented.clone();
        let split = BlockContent::paragraph("editor-made", " tail");
        edited[0].text = "bold".into();
        edited[0].marks = vec![MarkSpan::new(0..4, Mark::Bold)];
        edited.push(split);
        let renames = commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        assert_eq!(texts(&mut replica, &mut doc), ["bold", " tail"]);
        let after = shown(&mut replica, &mut doc);
        assert_eq!(renames, [("editor-made".to_owned(), after[1].id.clone())]);
        assert!(matches!(
            replica.applied.first(),
            Some(RichTextAction::SplitBlock { at: 4, .. })
        ));
        assert_eq!(after[0].marks, [MarkSpan::new(0..4, Mark::Bold)]);
    }

    #[test]
    fn backspace_at_a_start_merges_and_keeps_the_marks_it_carries() {
        let (mut doc, mut replica) = document("one\n\ntwo *it*");
        let presented = shown(&mut replica, &mut doc);
        assert_eq!(presented.len(), 2);
        let mut merged = presented[0].clone();
        merged.text = "onetwo it".into();
        merged.marks = vec![MarkSpan::new(7..9, Mark::Italic)];
        commit(&mut replica, &mut doc, &presented, &[merged]).unwrap();
        let after = shown(&mut replica, &mut doc);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].text, "onetwo it");
        assert_eq!(after[0].marks, [MarkSpan::new(7..9, Mark::Italic)]);
        assert!(matches!(
            replica.applied.first(),
            Some(RichTextAction::ReplaceSelection {
                start: 3,
                end: 0,
                ..
            })
        ));
    }

    #[test]
    fn a_selection_deleted_across_paragraphs_is_one_replacement() {
        let (mut doc, mut replica) = document("alpha\n\nbeta\n\ngamma");
        let presented = shown(&mut replica, &mut doc);
        let mut kept = presented[0].clone();
        kept.text = "alXmma".into();
        commit(&mut replica, &mut doc, &presented, &[kept]).unwrap();
        assert_eq!(texts(&mut replica, &mut doc), ["alXmma"]);
        assert!(matches!(
            replica.applied.as_slice(),
            [RichTextAction::ReplaceSelection { start: 2, end: 2, ins, .. }] if ins == "X"
        ));
    }

    #[test]
    fn a_new_kind_and_list_depth_are_style_edits() {
        let (mut doc, mut replica) = document("item");
        let presented = shown(&mut replica, &mut doc);
        let mut edited = presented.clone();
        edited[0].kind = BlockKind::Checklist {
            indent: 1,
            done: true,
        };
        commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        assert_eq!(
            shown(&mut replica, &mut doc)[0].kind,
            BlockKind::Checklist {
                indent: 1,
                done: true
            }
        );
        edited = shown(&mut replica, &mut doc);
        let presented = edited.clone();
        edited[0].kind = BlockKind::Bullet { indent: 1 };
        commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        assert_eq!(
            shown(&mut replica, &mut doc)[0].kind,
            BlockKind::Bullet { indent: 1 }
        );
    }

    #[test]
    fn a_table_keeps_its_source_and_how_it_shows() {
        let (mut doc, mut replica) = document("| a | b |");
        for visual in [true, false] {
            let presented = shown(&mut replica, &mut doc);
            let mut edited = presented.clone();
            edited[0].kind = BlockKind::Table { visual };
            commit(&mut replica, &mut doc, &presented, &edited).unwrap();
            let after = shown(&mut replica, &mut doc);
            assert_eq!(after[0].kind, BlockKind::Table { visual });
            assert_eq!(after[0].text, "| a | b |");
        }
        let presented = shown(&mut replica, &mut doc);
        let mut edited = presented.clone();
        edited[0].kind = BlockKind::Paragraph;
        commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        assert_eq!(shown(&mut replica, &mut doc)[0].kind, BlockKind::Paragraph);
    }

    #[test]
    fn marking_a_range_sets_only_that_range() {
        let (mut doc, mut replica) = document("make this bold");
        let presented = shown(&mut replica, &mut doc);
        let mut edited = presented.clone();
        edited[0].marks = vec![MarkSpan::new(10..14, Mark::Bold)];
        commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        assert!(matches!(
            replica.applied.as_slice(),
            [RichTextAction::SetMark { spans, name, value: Some(_), .. }]
                if name == "strong" && spans[0].start == 10 && spans[0].end == 14
        ));
    }

    #[test]
    fn typing_at_the_end_of_a_bold_word_leaves_the_mark_to_the_document() {
        // The editor carries the bold over what is typed at its end, as the
        // document does: only the text is written.
        let (mut doc, mut replica) = document("**bold**");
        let presented = shown(&mut replica, &mut doc);
        let mut edited = presented.clone();
        edited[0].text = "bolder".into();
        edited[0].marks = vec![MarkSpan::new(0..6, Mark::Bold)];
        commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        assert!(
            matches!(
                replica.applied.as_slice(),
                [RichTextAction::SpliceText { .. }]
            ),
            "{:?}",
            replica.applied
        );

        // An editor that ended the bold there (a typed closing `**`) has
        // the typed text plain in the document too.
        let (mut doc, mut replica) = document("**bold**");
        let presented = shown(&mut replica, &mut doc);
        let mut edited = presented.clone();
        edited[0].text = "bold!".into();
        commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        let now = shown(&mut replica, &mut doc);
        assert_eq!(now[0].text, "bold!");
        assert_eq!(now[0].marks, [MarkSpan::new(0..4, Mark::Bold)]);
    }

    #[test]
    fn markdown_put_back_around_a_bold_word_is_plain() {
        let (mut doc, mut replica) = document("a **b**");
        let presented = shown(&mut replica, &mut doc);
        let mut edited = presented.clone();
        edited[0].text = "a **b**".into();
        edited[0].marks = Vec::new();
        commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        let now = shown(&mut replica, &mut doc);
        assert_eq!(now[0].text, "a **b**");
        assert!(now[0].marks.is_empty(), "{:?}", now[0].marks);
    }

    #[test]
    fn a_paste_of_paragraphs_inserts_them_in_order() {
        let (mut doc, mut replica) = document("first");
        let presented = shown(&mut replica, &mut doc);
        let mut edited = presented.clone();
        edited.push(BlockContent::paragraph("p1", "second"));
        edited.push(BlockContent::paragraph("p2", "third"));
        let renames = commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        assert_eq!(texts(&mut replica, &mut doc), ["first", "second", "third"]);
        assert_eq!(renames.len(), 2);
    }

    #[test]
    fn a_block_before_the_first_takes_its_place() {
        let (mut doc, mut replica) = document("first");
        let presented = shown(&mut replica, &mut doc);
        let mut edited = vec![BlockContent::paragraph("top", "zero")];
        edited.extend(presented.clone());
        let renames = commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        let after = shown(&mut replica, &mut doc);
        assert_eq!(
            after.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(),
            ["zero", "first"]
        );
        let by_editor_id: HashMap<_, _> = renames.into_iter().collect();
        assert_eq!(by_editor_id.get("top"), Some(&after[0].id));
        assert_eq!(
            by_editor_id.get(presented[0].id.as_str()),
            Some(&after[1].id)
        );
    }

    #[test]
    fn a_dragged_paragraph_is_one_move() {
        let (mut doc, mut replica) = document("a\n\nb\n\nc\n\nd");
        let presented = shown(&mut replica, &mut doc);
        let ids: Vec<String> = presented.iter().map(|block| block.id.clone()).collect();
        // `d` dragged to the top, then `a` dragged below `c`.
        for (order, moved) in [([3, 0, 1, 2], &ids[3]), ([0, 2, 3, 1], &ids[0])] {
            let presented = shown(&mut replica, &mut doc);
            let edited: Vec<BlockContent> = order
                .iter()
                .map(|&index| presented[index].clone())
                .collect();
            replica.applied.clear();
            let renames = commit(&mut replica, &mut doc, &presented, &edited).unwrap();
            assert!(renames.is_empty(), "a move keeps every id");
            assert!(
                matches!(
                    replica.applied.as_slice(),
                    [RichTextAction::MoveBlocks { blocks, .. }] if blocks.as_slice() == std::slice::from_ref(moved)
                ),
                "{:?}",
                replica.applied
            );
            let now: Vec<String> = shown(&mut replica, &mut doc)
                .into_iter()
                .map(|block| block.id)
                .collect();
            let want: Vec<String> = edited.into_iter().map(|block| block.id).collect();
            assert_eq!(now, want);
        }
        assert_eq!(texts(&mut replica, &mut doc), ["d", "b", "c", "a"]);
    }

    #[test]
    fn a_window_after_the_start_moves_within_it() {
        let (mut doc, mut replica) = document("a\n\nb\n\nc\n\nd");
        let all = shown(&mut replica, &mut doc);
        // The editor presents a window from `b`: `d` moves up above `c`.
        let presented = all[1..].to_vec();
        let edited = vec![all[1].clone(), all[3].clone(), all[2].clone()];
        commit(&mut replica, &mut doc, &presented, &edited).unwrap();
        assert_eq!(texts(&mut replica, &mut doc), ["a", "b", "d", "c"]);
    }

    #[test]
    fn longest_increasing_keeps_the_most_in_place() {
        assert_eq!(longest_increasing(&[0, 1, 2]), [true, true, true]);
        assert_eq!(longest_increasing(&[3, 0, 1, 2]), [false, true, true, true]);
        assert_eq!(longest_increasing(&[1, 2, 3, 0]), [true, true, true, false]);
        assert_eq!(longest_increasing(&[]), Vec::<bool>::new());
    }

    #[test]
    fn char_splices_count_characters() {
        assert_eq!(char_splice("héllo", "héllo!"), (5, 0, "!".into()));
        assert_eq!(char_splice("aaa", "aa"), (2, 1, String::new()));
        assert_eq!(char_splice("wörld", "wrld"), (1, 1, String::new()));
    }

    #[test]
    fn a_kept_run_is_never_written_again() {
        for (before, after, splices) in [
            (
                "a **bö*",
                "a bö",
                vec![(2, 2, String::new()), (4, 1, String::new())],
            ),
            ("*hi", "hi", vec![(0, 1, String::new())]),
            (
                "a bö",
                "a **bö**",
                vec![(2, 0, "**".into()), (6, 0, "**".into())],
            ),
            // An autocorrection keeps the letters it did not change.
            (
                "doesnt",
                "doesn't ",
                vec![(5, 0, "'".into()), (7, 0, " ".into())],
            ),
            // Anything else is one splice.
            ("abc", "axc", vec![(1, 1, "x".into())]),
            ("teh", "the ", vec![(1, 2, "he ".into())]),
        ] {
            let spliced = char_splices(before, after);
            assert_eq!(spliced, splices, "{before:?} -> {after:?}");
            let mut text: Vec<char> = before.chars().collect();
            for (at, del, ins) in spliced {
                text.splice(at..at.saturating_add(del), ins.chars());
            }
            assert_eq!(text.into_iter().collect::<String>(), after, "{before:?}");
        }
    }
}
