//! Pre-operation body target from Writer's actual participant-aware encoder.
//! Does not read the window caret or participant outcome queue, mutate a view,
//! apply the action, or claim that any corresponding pixels were presented.
use crate::actions::{RegionKind, RichTextAction, WORD_PROCESSOR_ACTION_TYPE};
use automerge::{AutoCommit, ReadDoc};
use tilo_actions::{
    ActionAddress, EncodedAction, InteractionAnchor, InteractionTextRange, MAX_ID_BYTES,
};
use ui::invocation_anchor::{MAX_PROJECTION_BYTES, RegisteredInvocationAnchor};

pub static PROJECTORS: [RegisteredInvocationAnchor; 1] =
    [
        RegisteredInvocationAnchor::new(WORD_PROCESSOR_ACTION_TYPE, project)
            .with_applied(project_applied),
    ];

pub(crate) fn project(
    doc: &AutoCommit,
    address: &ActionAddress,
    encoded: &EncodedAction,
) -> Option<InteractionAnchor> {
    if !matches!(
        address.resource.as_str(),
        "word-processor.document" | "word-processor.caret"
    ) || address.action != "type-text"
        || encoded.type_id != WORD_PROCESSOR_ACTION_TYPE
        || encoded.payload.len() > MAX_PROJECTION_BYTES
    {
        return None;
    }
    let action: RichTextAction = serde_json::from_slice(&encoded.payload).ok()?;
    let (region, block, start, end) = match action {
        RichTextAction::SpliceText {
            region,
            block,
            at,
            del,
            ..
        } => (region, block, at, at.checked_add(del)?),
        RichTextAction::ReplaceSelection {
            region,
            start_block,
            start,
            end_block,
            end,
            ..
        } if start_block == end_block => (region, start_block, start, end),
        // Cross-block order needs its own bounded proof; unsupported targets
        // remain explicit gaps rather than a guessed caret or truncated range.
        _ => return None,
    };
    if region != RegionKind::Body
        || block.is_empty()
        || block.len() > MAX_ID_BYTES
        || block.chars().any(char::is_control)
        || start > end
        || doc.text_encoding() != automerge::TextEncoding::UnicodeCodePoint
    {
        return None;
    }
    let containers = tilo_doc::model::open(doc, &automerge::ROOT).ok()?;
    // The action executor's own region-membership and code-point length check
    // reads one addressed block. Never materialize the entire document here.
    if end > crate::actions::splice_target_len(doc, &containers, region, &block)? {
        return None;
    }
    Some(InteractionAnchor::Text {
        region: "body".into(),
        ranges: vec![InteractionTextRange {
            block,
            start: u64::try_from(start).ok()?,
            end: u64::try_from(end).ok()?,
        }],
    })
}

/// Derive the resulting participant target from the action actually applied,
/// never the window caret or the participant outcome queue. SDK qualification
/// separately proves local application, exact custody and unchanged final heads.
pub(crate) fn project_applied(
    doc: &AutoCommit,
    address: &ActionAddress,
    encoded: &EncodedAction,
) -> Option<InteractionAnchor> {
    if !matches!(
        address.resource.as_str(),
        "word-processor.document" | "word-processor.caret"
    ) || address.action != "type-text"
        || encoded.type_id != WORD_PROCESSOR_ACTION_TYPE
        || encoded.payload.len() > MAX_PROJECTION_BYTES
        || doc.text_encoding() != automerge::TextEncoding::UnicodeCodePoint
    {
        return None;
    }
    let action: RichTextAction = serde_json::from_slice(&encoded.payload).ok()?;
    let (region, block, start, ins) = match action {
        RichTextAction::SpliceText {
            region,
            block,
            at,
            ins,
            ..
        } => (region, block, at, ins),
        RichTextAction::ReplaceSelection {
            region,
            start_block,
            start,
            end_block,
            ins,
            ..
        } if start_block == end_block => (region, start_block, start, ins),
        _ => return None,
    };
    if region != RegionKind::Body
        || block.is_empty()
        || block.len() > MAX_ID_BYTES
        || block.chars().any(char::is_control)
    {
        return None;
    }
    let caret = start.checked_add(ins.chars().count())?;
    let containers = tilo_doc::model::open(doc, &automerge::ROOT).ok()?;
    if caret > crate::actions::splice_target_len(doc, &containers, region, &block)? {
        return None;
    }
    let caret = u64::try_from(caret).ok()?;
    Some(InteractionAnchor::Text {
        region: "body".into(),
        ranges: vec![InteractionTextRange {
            block,
            start: caret,
            end: caret,
        }],
    })
}

#[cfg(test)]
#[path = "invocation_anchor_tests.rs"]
mod tests;
