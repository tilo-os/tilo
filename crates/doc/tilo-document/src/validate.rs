//! What an action must be to apply: the checks every edit passes before it writes.
//!
//! An action arrives after the document may have changed, or from an untrusted automation, so
//! every target it names is read against one immutable view of the document first, and every
//! payload it carries is held to its limit. A refusal says which of the two it was: a text longer
//! than one text may hold ([`MAX_ACTION_TEXT_BYTES`]), or a target that is not as the action read
//! it.

use automerge::{AutoCommit, ReadDoc};
use std::collections::HashSet;

use tilo_doc::Containers;

use crate::ClipBlock;
use crate::action::{
    ActionDomain, AttrVal, MAX_ACTION_BLOB_BYTES, MAX_ACTION_TEXT_BYTES, MAX_IMAGE_ALT_CHARS,
    MAX_IMPORT_BYTES, MAX_IMPORTED_CONTENT, MAX_TABLE_CELLS, MAX_TABLE_DIMENSION, MarkVal,
    RegionKind, RichTextAction, Span, TableEdit, body_end, clip_attr, num, valid_hyperlink_target,
};

/// Read only the addressed block while matching tilo-doc's snapshot membership:
/// a region block must be a map with a byte-valued order key. Missing or
/// non-Text text projects to the empty string, just as `materialize_block` does.
/// Characters of region block `id`, `None` when it is not a block of that
/// region: the validator's one-block membership and length reading.
pub fn splice_target_len(
    doc: &AutoCommit,
    c: &Containers,
    region: RegionKind,
    id: &str,
) -> Option<usize> {
    target_block(doc, c, region, id).map(|(len, _)| len)
}

/// The addressed block's text length and order key, reading only that block.
fn target_block(
    doc: &AutoCommit,
    c: &Containers,
    region: RegionKind,
    id: &str,
) -> Option<(usize, Vec<u8>)> {
    use automerge::{ObjType, ScalarValue, Value};
    use tilo_doc::schema::{BLOCK_ORDER, BLOCK_TEXT};

    let container = match region {
        RegionKind::Body => &c.blocks,
        RegionKind::Header => &c.header,
        RegionKind::Footer => &c.footer,
    };
    let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(container, id) else {
        return None;
    };
    let key = match doc.get(&block, BLOCK_ORDER) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Bytes(key) => key.clone(),
            _ => return None,
        },
        _ => return None,
    };
    let len = match doc.get(&block, BLOCK_TEXT) {
        // Character offsets: the text object's own length when it counts
        // code points, as documents do by default.
        Ok(Some((Value::Object(ObjType::Text), text))) => {
            if doc.text_encoding() == automerge::TextEncoding::UnicodeCodePoint {
                doc.length(&text)
            } else {
                doc.text(&text).unwrap_or_default().chars().count()
            }
        }
        _ => 0,
    };
    Some((len, key))
}

/// Validate an action against one immutable point-in-time view of `doc`.
///
/// This is intentionally separate from the write helpers: an invalid action
/// must make **no mutation**, never a partly-applied collection of valid
/// targets followed by a stale one. It also bounds hostile payloads before an
/// action allocates a large table/blob or asks automerge to index impossible
/// offsets.  The caller maps `false` to an explicit [`ActionError::Refused`],
/// preserving a useful stale-target result without leaving a partial edit.
pub fn action_is_valid(action: &RichTextAction, doc: &AutoCommit, c: &Containers) -> bool {
    refusal(action, doc, c).is_ok()
}

/// Why `action` does not apply to `doc` as it is, for a reason a window tells
/// someone: the text it carries is longer than one text may hold, or what it
/// names is not as it read it (the document moved on since, or the arguments
/// name nothing there).
///
/// # Errors
///
/// Everything [`action_is_valid`] refuses, as the reason to tell.
pub fn refusal(
    action: &RichTextAction,
    doc: &AutoCommit,
    c: &Containers,
) -> Result<(), tilo_actions::Declined> {
    // An edit of a paragraph reads only the paragraphs it names; the whole
    // document is read only by the few checks that need it.
    let validator = ActionValidator::new(doc, c);
    if validator.validate(action) {
        return Ok(());
    }
    // The size of what it carries is the edit's own fault, not the document's: someone can
    // paste less, where a stale target is nothing they did.
    if validator.over.get() {
        Err(tilo_actions::Declined::TooLong)
    } else {
        Err(tilo_actions::Declined::Changed)
    }
}

struct ActionValidator<'a> {
    doc: &'a AutoCommit,
    containers: &'a Containers,
    /// The whole document, read only by the rare checks that need it (style
    /// instances, tracked changes, tables), never for an edit of a paragraph.
    snapshot: std::cell::OnceCell<tilo_doc::snapshot::DocSnapshot>,
    /// Whether a text the action carries was longer than one text may hold while
    /// validating it: [`refusal`] tells that apart from a target that moved.
    over: std::cell::Cell<bool>,
}

impl<'a> ActionValidator<'a> {
    fn new(doc: &'a AutoCommit, containers: &'a Containers) -> Self {
        Self {
            doc,
            containers,
            snapshot: std::cell::OnceCell::new(),
            over: std::cell::Cell::new(false),
        }
    }

    fn snapshot(&self) -> &tilo_doc::snapshot::DocSnapshot {
        self.snapshot
            .get_or_init(|| tilo_doc::model::materialize(self.doc, self.containers))
    }

    fn block_len(&self, region: RegionKind, id: &str) -> Option<usize> {
        splice_target_len(self.doc, self.containers, region, id)
    }

    /// Whether `[start_block, start)` comes no later than `[end_block, end)`
    /// in `region`, and both offsets are within their blocks: compared by
    /// the blocks' order keys, without reading the region.
    fn ordered_range(
        &self,
        region: RegionKind,
        (start_block, start): (&str, usize),
        (end_block, end): (&str, usize),
    ) -> bool {
        let first = target_block(self.doc, self.containers, region, start_block);
        let last = target_block(self.doc, self.containers, region, end_block);
        let (Some((start_len, start_key)), Some((end_len, end_key))) = (first, last) else {
            return false;
        };
        let order = tilo_resource::order::bytes::cmp(&start_key, &end_key)
            .then_with(|| start_block.cmp(end_block));
        let before = order.is_lt() || (order.is_eq() && start <= end);
        before && start <= start_len && end <= end_len
    }

    fn unique_existing(&self, region: RegionKind, ids: &[String]) -> bool {
        !ids.is_empty()
            && ids.iter().all(|id| self.block_len(region, id).is_some())
            && ids.iter().collect::<HashSet<_>>().len() == ids.len()
    }

    fn safe_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }

    /// Whether `bytes` bytes of text are as much as one text may hold: the one limit an
    /// action's text is held to ([`MAX_ACTION_TEXT_BYTES`]), kept for [`refusal`] to tell a
    /// text too long from a target that moved.
    fn text_fits(&self, bytes: usize) -> bool {
        let fits = bytes <= MAX_ACTION_TEXT_BYTES;
        if !fits {
            self.over.set(true);
        }
        fits
    }

    fn safe_text(&self, text: &str) -> bool {
        self.text_fits(text.len())
    }

    fn valid_attr(key: &str, value: &AttrVal) -> bool {
        match key {
            "align" => {
                matches!(value, AttrVal::Str(v) if matches!(v.as_str(), "left" | "center" | "right" | "justify"))
            }
            "page_break_before" | "flip_h" | "flip_v" | "checked" | "code" | "table_source" => {
                matches!(value, AttrVal::Bool(_))
            }
            "indent" | "line_spacing" => {
                matches!(value, AttrVal::Float(v) if v.is_finite() && *v >= 0.0 && *v <= 10_000.0)
            }
            "first_line_indent" => {
                matches!(value, AttrVal::Float(v) if v.is_finite() && (-10_000.0..=10_000.0).contains(v))
            }
            "tab_stops" | "style" => matches!(value, AttrVal::Str(v) if v.len() <= 4_096),
            "width" | "height" => matches!(value, AttrVal::Int(v) if *v > 0 && *v <= 100_000),
            "lang" => {
                matches!(value, AttrVal::Str(v) if v.len() <= 64 && !v.chars().any(char::is_control))
            }
            "code_group" => matches!(value, AttrVal::Str(v) if Self::safe_name(v)),
            "nesting" => matches!(value, AttrVal::Int(v) if (0..=9).contains(v)),
            "alt" => matches!(value, AttrVal::Str(v) if Self::valid_alt(v)),
            // A callout: the GitHub alert a quote is set apart as.
            "alert" => {
                matches!(value, AttrVal::Str(v) if crate::block_markdown::CALLOUTS.contains(&v.as_str()))
            }
            _ => false,
        }
    }

    /// An image description a screen reader can speak: non-blank, bounded,
    /// and free of control characters that would corrupt exports.
    fn valid_alt(alt: &str) -> bool {
        !alt.trim().is_empty()
            && alt.chars().count() <= MAX_IMAGE_ALT_CHARS
            && !alt.chars().any(char::is_control)
    }

    /// Whether every one of `blocks` is a body image: only an image has an
    /// accessible description to set.
    fn all_images(&self, region: RegionKind, blocks: &[String]) -> bool {
        region == RegionKind::Body
            && blocks.iter().all(|block| {
                self.snapshot()
                    .blocks
                    .iter()
                    .any(|b| b.id == *block && b.kind == "image")
            })
    }

    fn valid_clip_attrs(block: &crate::ClipBlock) -> bool {
        block.attrs.len() <= 16
            && block.attrs.iter().all(|(key, value)| {
                let value = clip_attr(value);
                Self::valid_attr(key, &value)
                    || (block.kind == "cell" && Self::valid_cell_attr(key, &value))
            })
    }

    /// A pasted cell's place: the table it belongs to (renamed on apply) and
    /// its row and column.
    fn valid_cell_attr(key: &str, value: &AttrVal) -> bool {
        let dimension = i64::try_from(MAX_TABLE_DIMENSION).unwrap_or(i64::MAX);
        match key {
            "table" => matches!(value, AttrVal::Str(v) if Self::safe_name(v)),
            "row" | "col" => matches!(value, AttrVal::Int(v) if (0..dimension).contains(v)),
            _ => false,
        }
    }

    /// Attributes whose absence is their default, so an edit may remove them.
    fn clearable_attr(key: &str) -> bool {
        matches!(
            key,
            "checked"
                | "code"
                | "lang"
                | "code_group"
                | "nesting"
                | "table_source"
                | "alt"
                | "alert"
        )
    }

    fn valid_style_property(key: &str, value: &AttrVal) -> bool {
        match key {
            tilo_doc::schema::STYLE_NAME => {
                matches!(value, AttrVal::Str(v) if v.chars().count() <= 256)
            }
            tilo_doc::schema::STYLE_FONT => {
                matches!(value, AttrVal::Str(v) if matches!(v.as_str(), "sans" | "serif" | "mono"))
            }
            tilo_doc::schema::STYLE_SIZE => {
                matches!(value, AttrVal::Float(v) if v.is_finite() && (6.0..=200.0).contains(v))
            }
            tilo_doc::schema::STYLE_BOLD | tilo_doc::schema::STYLE_ITALIC => {
                matches!(value, AttrVal::Bool(_))
            }
            tilo_doc::schema::STYLE_COLOR => {
                matches!(value, AttrVal::Str(v) if v.len() == 7 && v.starts_with('#'))
            }
            tilo_doc::schema::STYLE_ALIGN => {
                matches!(value, AttrVal::Str(v) if matches!(v.as_str(), "left" | "center" | "right" | "justify"))
            }
            tilo_doc::schema::STYLE_SPACE_BEFORE
            | tilo_doc::schema::STYLE_SPACE_AFTER
            | tilo_doc::schema::STYLE_INDENT => {
                matches!(value, AttrVal::Float(v) if v.is_finite() && (0.0..=10_000.0).contains(v))
            }
            _ => false,
        }
    }

    fn validate(&self, action: &RichTextAction) -> bool {
        match action.domain() {
            ActionDomain::Text => {
                self.validate_text_edit(action) || self.validate_paste_and_body(action)
            }
            ActionDomain::Formatting => self.validate_formatting(action),
            ActionDomain::Media => self.validate_media(action),
            ActionDomain::Table => self.validate_tables(action),
            ActionDomain::Structure => self.validate_structure(action),
            ActionDomain::Review => self.validate_review(action),
            ActionDomain::Blocks => {
                crate::block_edit::check(action, self.doc, self.containers).is_ok()
            }
        }
    }

    fn valid_replacements(&self, region: RegionKind, spans: &[Span], ins: &str) -> bool {
        if spans.is_empty() || spans.len() > 10_000 {
            return false;
        }
        // Every replacement carries the same text: what the action would write, held to the
        // limit in bytes, saturating rather than overflowing.
        let written = ins.len().saturating_mul(spans.len());
        if !self.text_fits(written) {
            return false;
        }
        let mut sorted: Vec<_> = spans.iter().collect();
        sorted.sort_by(|a, b| a.block.cmp(&b.block).then_with(|| a.start.cmp(&b.start)));
        if sorted.windows(2).any(|pair| match pair {
            [a, b] if a.block == b.block => a.end > b.start,
            _ => false,
        }) {
            return false;
        }
        spans.iter().all(|span| {
            self.block_len(region, &span.block)
                .is_some_and(|len| span.start < span.end && span.end <= len)
        })
    }

    fn validate_text_edit(&self, action: &RichTextAction) -> bool {
        match action {
            RichTextAction::ReplaceMatches { region, spans, ins } => {
                self.valid_replacements(*region, spans, ins)
            }
            RichTextAction::SpliceText {
                region,
                block,
                at,
                del,
                ins,
            } => {
                self.block_len(*region, block)
                    .is_some_and(|len| *at <= len && *del <= len.saturating_sub(*at))
                    && self.safe_text(ins)
            }
            RichTextAction::ReplaceSelection {
                region,
                start_block,
                start,
                end_block,
                end,
                ins,
            } => {
                self.ordered_range(*region, (start_block, *start), (end_block, *end))
                    && self.safe_text(ins)
            }
            RichTextAction::SetMark {
                region,
                spans,
                name,
                value,
                also_clear,
            } => {
                Self::safe_name(name)
                    && also_clear.iter().all(|sibling| Self::safe_name(sibling))
                    && spans.len() <= 10_000
                    && !spans.is_empty()
                    && spans.iter().all(|span| {
                        self.block_len(*region, &span.block)
                            .is_some_and(|len| span.start < span.end && span.end <= len)
                    })
                    && match (name.as_str(), value) {
                        ("link", Some(MarkVal::Str(target))) => valid_hyperlink_target(target),
                        ("link", Some(_)) => false,
                        (_, Some(MarkVal::Str(text))) => self.safe_text(text),
                        _ => true,
                    }
            }
            _ => false,
        }
    }

    /// A pasted or written paragraph the document can hold.
    fn valid_clip(&self, block: &ClipBlock) -> bool {
        self.safe_text(&block.text)
            && Self::safe_name(&block.kind)
            && block.level.is_none_or(|level| (0..=9).contains(&level))
            && Self::valid_clip_attrs(block)
            && block.marks.iter().all(|mark| {
                Self::safe_name(&mark.name)
                    && mark.start < mark.end
                    && mark.end <= block.text.chars().count()
            })
    }

    fn validate_paste_and_body(&self, action: &RichTextAction) -> bool {
        match action {
            RichTextAction::PasteFragment {
                region,
                start_block,
                start,
                end_block,
                end,
                blocks,
                ..
            } => {
                let valid_range =
                    self.ordered_range(*region, (start_block, *start), (end_block, *end));
                valid_range
                    && !blocks.is_empty()
                    && blocks.len() <= 10_000
                    && blocks.iter().all(|block| self.valid_clip(block))
            }
            RichTextAction::InsertParagraph { after, text } => {
                self.block_len(RegionKind::Body, after).is_some() && self.safe_text(text)
            }
            RichTextAction::AppendParagraph { text } => !text.is_empty() && self.safe_text(text),
            RichTextAction::ContinueParagraph { text } => {
                !text.is_empty()
                    && self.safe_text(text)
                    && body_end(self.doc, self.containers).is_some_and(|end| end.text_block)
            }
            RichTextAction::RemoveBlocks { blocks } => {
                self.unique_existing(RegionKind::Body, blocks)
            }
            RichTextAction::RemoveRegionBlocks { region, blocks } => {
                matches!(region, RegionKind::Header | RegionKind::Footer)
                    && self.unique_existing(*region, blocks)
            }
            RichTextAction::EnsureRegionBlock { .. } => true,
            RichTextAction::ReplaceBody { markdown } => self.safe_text(markdown),
            RichTextAction::ReplaceClips { blocks } => {
                blocks.len() <= 10_000 && blocks.iter().all(|block| self.valid_clip(block))
            }
            RichTextAction::SyncClips {
                blocks, replace, ..
            } => {
                blocks.len() <= 10_000
                    && replace.as_ref().is_none_or(|ids| ids.len() <= 10_000)
                    && blocks.iter().all(|block| self.valid_clip(block))
            }
            RichTextAction::ReplaceFormatted { bytes, .. } => bytes.len() <= MAX_IMPORT_BYTES,
            RichTextAction::ReplaceImported {
                document,
                content,
                file_digest,
            } => {
                document.len() <= MAX_IMPORT_BYTES
                    && tilo_content::valid_hash(file_digest)
                    && content.len() <= MAX_IMPORTED_CONTENT
                    && content.iter().all(|(address, reference)| {
                        tilo_content::valid_hash(address)
                            && tilo_content::valid_reference(reference)
                    })
            }
            _ => false,
        }
    }

    fn validate_formatting(&self, action: &RichTextAction) -> bool {
        match action {
        RichTextAction::SetParagraphStyle {
            region,
            blocks,
            kind,
            level,
            ..
        } => {
            self.unique_existing(*region, blocks)
                && Self::safe_name(kind)
                && level.is_none_or(|level| (0..=9).contains(&level))
        }
        RichTextAction::ApplyNamedStyle {
            region,
            blocks,
            style,
            kind,
            level,
            ..
        } => {
            self.unique_existing(*region, blocks)
                && !style.is_empty()
                && style.len() <= 128
                && kind.as_deref().is_none_or(Self::safe_name)
                && level.is_none_or(|level| (0..=9).contains(&level))
        }
        RichTextAction::SetBlockAttr {
            region,
            blocks,
            key,
            value,
        } => {
            self.unique_existing(*region, blocks)
                && Self::valid_attr(key, value)
                && (key != "alt" || self.all_images(*region, blocks))
        }
        RichTextAction::ClearBlockAttr {
            region,
            blocks,
            key,
        } => self.unique_existing(*region, blocks) && Self::clearable_attr(key),
        RichTextAction::NoteFileWritten { digest } => {
            !digest.is_empty()
                && digest.len() <= 128
                && digest.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }
        RichTextAction::SetPageMargins { top, right, bottom, left } => [top, right, bottom, left]
            .into_iter()
            .all(|value| value.is_finite() && (4.0..=10_000.0).contains(value)),
        RichTextAction::SetStyleProperty { style, key, value } => {
            Self::safe_name(style)
                && !tilo_doc::model::style_is_tombstoned(self.doc, self.containers, style)
                && Self::valid_style_property(key, value)
        }
        RichTextAction::DeleteStyle {
            style,
            fallback_blocks,
        } => {
            Self::safe_name(style)
                && !crate::BUILTIN_STYLES.contains(&style.as_str())
                && tilo_doc::model::style_ids(self.doc, self.containers).contains(style)
                && fallback_blocks.iter().all(|block| {
                    self.snapshot().blocks.iter().any(|entry| {
                        entry.id == *block
                            && matches!(entry.attrs.get("style"), Some(tilo_doc::AttrVal::Str(id)) if id == style)
                    })
                })
        }
            _ => false,
        }
    }

    fn validate_media(&self, action: &RichTextAction) -> bool {
        match action {
            RichTextAction::SetImageCrop {
                block,
                left,
                top,
                right,
                bottom,
            } => {
                self.block_len(RegionKind::Body, block).is_some()
                && self.snapshot().blocks.iter().any(|b| b.id == *block && b.kind == "image")
                && [left, top, right, bottom]
                    .iter()
                    .all(|edge| (0_i64..=9_500).contains(*edge))
                // Always retain at least five percent of the source in both
                // dimensions. This prevents a hostile crop from creating an
                // infinite scale factor in the renderer.
                && (*left).saturating_add(*right) <= 9_500
                && (*top).saturating_add(*bottom) <= 9_500
            }
            RichTextAction::SetImageSize {
                block,
                width,
                height,
            } => {
                self.block_len(RegionKind::Body, block).is_some()
                    && self
                        .snapshot()
                        .blocks
                        .iter()
                        .any(|b| b.id == *block && b.kind == "image")
                    && (16..=100_000).contains(width)
                    && (16..=100_000).contains(height)
            }
            RichTextAction::SetImagePlacement {
                block,
                anchor,
                wrap,
                x,
                y,
                page,
            } => {
                self.block_len(RegionKind::Body, block).is_some()
                    && self
                        .snapshot()
                        .blocks
                        .iter()
                        .any(|b| b.id == *block && b.kind == "image")
                    && matches!(anchor.as_str(), "inline" | "paragraph" | "page")
                    && matches!(wrap.as_str(), "none" | "left" | "right" | "both")
                    && (-100_000..=100_000).contains(x)
                    && (-100_000..=100_000).contains(y)
                    && (0..=10_000).contains(page)
            }
            RichTextAction::InsertImage {
                block,
                at,
                content,
                len,
                width,
                height,
                ..
            } => {
                self.block_len(RegionKind::Body, block)
                    .is_some_and(|len| *at <= len)
                    && tilo_content::valid_reference(content)
                    && usize::try_from(*len).is_ok_and(|len| len <= MAX_ACTION_BLOB_BYTES)
                    && width.is_none_or(|value| (1..=100_000).contains(&value))
                    && height.is_none_or(|value| (1..=100_000).contains(&value))
            }
            _ => false,
        }
    }

    fn validate_tables(&self, action: &RichTextAction) -> bool {
        match action {
            RichTextAction::InsertTable { after, rows, cols } => {
                self.block_len(RegionKind::Body, after).is_some()
                    && *rows > 0
                    && *cols > 0
                    && *rows <= MAX_TABLE_DIMENSION
                    && *cols <= MAX_TABLE_DIMENSION
                    && rows.saturating_mul(*cols) <= MAX_TABLE_CELLS
            }
            RichTextAction::InsertFilledTable { after, cells } => {
                let rows = cells.len();
                let cols = cells.first().map_or(0, Vec::len);
                self.block_len(RegionKind::Body, after).is_some()
                    && rows > 0
                    && cols > 0
                    && rows <= MAX_TABLE_DIMENSION
                    && cols <= MAX_TABLE_DIMENSION
                    && rows.saturating_mul(cols) <= MAX_TABLE_CELLS
                    && cells.iter().all(|row| row.len() == cols)
                    // The whole table's text is held to what one text action
                    // may carry, counted without overflowing and stopping at
                    // the first cell past the limit.
                    && cells
                        .iter()
                        .flatten()
                        .try_fold(0_usize, |total, text| {
                            let total = total.saturating_add(text.len());
                            self.text_fits(total).then_some(total)
                        })
                        .is_some()
            }
            RichTextAction::EditTable { table, edit } => {
                let cells: Vec<_> = self
                    .snapshot()
                    .blocks
                    .iter()
                    .filter(|block| {
                        block.kind == "cell"
                            && matches!(block.attrs.get("table"), Some(tilo_doc::AttrVal::Str(id)) if id == table)
                    })
                    .collect();
                let coord = |block: &tilo_doc::snapshot::BlockView, key: &str| match block
                    .attrs
                    .get(key)
                {
                    Some(tilo_doc::AttrVal::Int(value)) if *value >= 0 => num::usize_of_i64(*value),
                    _ => 0,
                };
                let rows = cells
                    .iter()
                    .map(|block| coord(block, "row"))
                    .max()
                    .map_or(0, |value| value.saturating_add(1));
                let cols = cells
                    .iter()
                    .map(|block| coord(block, "col"))
                    .max()
                    .map_or(0, |value| value.saturating_add(1));
                !table.is_empty()
                    && !cells.is_empty()
                    && cells.len() == rows.saturating_mul(cols)
                    && match edit {
                        TableEdit::InsertRow { at } => *at <= rows && rows < MAX_TABLE_DIMENSION,
                        TableEdit::InsertColumn { at } => *at <= cols && cols < MAX_TABLE_DIMENSION,
                        TableEdit::DeleteRow { row } => *row < rows,
                        TableEdit::DeleteColumn { column } => *column < cols,
                        TableEdit::Delete => true,
                    }
            }
            _ => false,
        }
    }

    fn validate_structure(&self, action: &RichTextAction) -> bool {
        match action {
            RichTextAction::InsertPageBreak { region, block, at } => {
                self.block_len(*region, block).is_some_and(|len| *at <= len)
            }
            RichTextAction::SplitBlock {
                region,
                block,
                at,
                kind,
                ..
            } => {
                self.block_len(*region, block).is_some_and(|len| *at <= len)
                    && matches!(kind.as_str(), "paragraph" | "list_item")
            }
            RichTextAction::InsertField {
                region,
                block,
                at,
                field,
            } => {
                self.block_len(*region, block).is_some_and(|len| *at <= len)
                    && field == tilo_doc::schema::FIELD_PAGE_NUMBER
            }
            RichTextAction::MoveBlocks { blocks, after } => {
                self.unique_existing(RegionKind::Body, blocks)
                    && after.as_ref().is_none_or(|after| {
                        self.block_len(RegionKind::Body, after).is_some() && !blocks.contains(after)
                    })
            }
            RichTextAction::GenerateToc { entries } => {
                !entries.is_empty()
                    && entries.len() <= 10_000
                    && entries
                        .iter()
                        .map(|entry| entry.heading.as_str())
                        .collect::<HashSet<_>>()
                        .len()
                        == entries.len()
                    && entries.iter().all(|entry| {
                        (1..=9).contains(&entry.level)
                            && self.safe_text(&entry.text)
                            && entry.page.is_none_or(|page| page <= 100_000)
                            && self
                                .snapshot()
                                .blocks
                                .iter()
                                .any(|block| block.id == entry.heading && block.kind == "heading")
                    })
            }
            _ => false,
        }
    }

    fn validate_review(&self, action: &RichTextAction) -> bool {
        match action {
            RichTextAction::AcceptChanges { id } | RichTextAction::RejectChanges { id } => {
                id.as_ref().is_none_or(|id| {
                    self.snapshot()
                        .changes
                        .iter()
                        .any(|change| &change.id == id)
                })
            }
            RichTextAction::ResolveComment { id } => {
                !id.is_empty()
                    && self
                        .snapshot()
                        .comments
                        .iter()
                        .any(|comment| comment.id == *id)
            }
            RichTextAction::TrackInsert {
                block,
                at,
                text,
                author,
                date,
            } => {
                self.block_len(RegionKind::Body, block)
                    .is_some_and(|len| *at <= len)
                    && !text.is_empty()
                    && self.safe_text(text)
                    && !author.trim().is_empty()
                    && author.chars().count() <= 256
                    && date.chars().count() <= 128
            }
            RichTextAction::AddComment {
                block,
                start,
                end,
                author,
                date,
                text,
            } => {
                self.block_len(RegionKind::Body, block)
                    .is_some_and(|len| *start < *end && *end <= len)
                    && !author.trim().is_empty()
                    && author.chars().count() <= 256
                    && date.chars().count() <= 128
                    && self.safe_text(text)
            }
            RichTextAction::ReplyToComment {
                comment,
                author,
                date,
                text,
                body,
            } => {
                self.snapshot()
                    .comments
                    .iter()
                    .any(|entry| entry.id == *comment)
                    && !author.trim().is_empty()
                    && author.chars().count() <= 256
                    && date.chars().count() <= 128
                    && self.safe_text(text)
                    && body.len() <= 1_000
                    && body.iter().all(|block| {
                        Self::safe_name(&block.kind)
                            && self.safe_text(&block.text)
                            && block.level.is_none_or(|level| (0..=9).contains(&level))
                            && Self::valid_clip_attrs(block)
                            && block.marks.iter().all(|mark| {
                                Self::safe_name(&mark.name)
                                    && mark.start < mark.end
                                    && mark.end <= block.text.chars().count()
                            })
                    })
            }
            _ => false,
        }
    }
}
