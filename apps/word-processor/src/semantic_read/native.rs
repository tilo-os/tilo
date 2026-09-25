//! Retained native mechanics only; current owner/disclosure qualification is external.
use super::*;
use automerge::resource_limits::{NativeObject, NativeReadSession, NativeReservation};
use std::mem::size_of;
/// Backing drops before the original current-turn credit. No extraction or Clone.
#[derive(Debug)]
pub struct NativePage {
    page: Page,
    _credit: NativeReservation,
}
impl NativePage {
    pub fn page(&self) -> &Page {
        &self.page
    }
}
fn output_bound(limits: Limits) -> Result<usize, ReadError> {
    qdoc::semantic_read::validate_limits(limits, 64)?;
    // Exact typed fixed-capacity vectors; candidate id/order <=256 each,
    // segment id/kind <=256/64. Strings grow only for streamed text: retained
    // capacities <=2*total bytes +8 per string; moving realloc adds <=one
    // text budget. Four cursor copies cover previous/current/temporary overlap.
    let terms = [
        64usize.checked_mul(size_of::<String>() + 64),
        (limits.items + 1).checked_mul(size_of::<Candidate<'_>>() + 512),
        limits.items.checked_mul(size_of::<Segment>() + 320 + 8),
        limits.json_bytes.checked_mul(3),
        4usize.checked_mul(size_of::<Cursor>() + 512),
        // Fixed serializer refusal/error bookkeeping, independent of content.
        Some(1024),
    ];
    terms.into_iter().try_fold(0usize, |sum, term| {
        sum.checked_add(term.ok_or(ReadError::Limit)?)
            .ok_or(ReadError::Limit)
    })
}
pub fn read_text_native<'doc>(
    read: &NativeReadSession<'doc, '_>,
    root: NativeObject<'doc>,
    cursor: Option<&Cursor>,
    limits: Limits,
) -> Result<NativePage, ReadError> {
    let credit = read.reserve_output(output_bound(limits)?)?;
    let source = Source::native(read);
    let root = source.native_object(root)?;
    let page = read_source(&source, &root, cursor, limits)?;
    Ok(NativePage {
        page,
        _credit: credit,
    })
}
