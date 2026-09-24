//! Retained native mechanics only; current owner/disclosure qualification is external.
use super::*;
use automerge::resource_limits::{NativeReadSession, NativeReservation};
use qdoc::semantic_read::source::Object;
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
    qdoc::semantic_read::validate_limits(limits, 32)?;
    // Fixed vector capacities. Each message owns <=128/256/128 metadata
    // bytes. Streamed String capacities plus moving-realloc overlap need at
    // most3*the aggregate JSON budget plus8 bytes per tiny String. Four cursor
    // copies cover previous/current/temporary overlap; channel <=128 bytes.
    let terms = [
        64usize.checked_mul(size_of::<String>() + 64),
        (limits.items + 1).checked_mul(size_of::<(usize, Object<'_>)>()),
        limits.items.checked_mul(size_of::<Message>() + 512 + 8),
        limits.json_bytes.checked_mul(3),
        4usize.checked_mul(size_of::<Cursor>() + 128),
        Some(128 + 1024),
    ];
    terms.into_iter().try_fold(0usize, |sum, term| {
        sum.checked_add(term.ok_or(ReadError::Limit)?)
            .ok_or(ReadError::Limit)
    })
}
pub fn read_messages_native(
    read: &NativeReadSession<'_, '_>,
    channel: &str,
    cursor: Option<&Cursor>,
    limits: Limits,
) -> Result<NativePage, ReadError> {
    let credit = read.reserve_output(output_bound(limits)?)?;
    let page = read_source(&Source::native(read), channel, cursor, limits)?;
    Ok(NativePage {
        page,
        _credit: credit,
    })
}
