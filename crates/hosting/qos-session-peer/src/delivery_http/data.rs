//! Bounded inert route preparation. Native authority and commits remain owner scheduled.
use tilo_relay::delivery_v3::{
    commit::{DeliveryId, Digest},
    cursor::CursorToken,
    framing::{self, FrameError, MessageType},
    http_proof::{Method, Request},
    state::{self, CursorAck, StateItemChunk},
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DataError {
    InvalidFrame,
    Unsupported,
    QuotaExceeded,
}
#[derive(Debug)]
pub(super) struct PreparedItem<'a> {
    pub(super) delivery: DeliveryId,
    pub(super) digest: Digest,
    pub(super) canonical: &'a [u8],
}

fn frame_error(error: FrameError) -> DataError {
    match error {
        FrameError::UnsupportedVersion(_) => DataError::Unsupported,
        FrameError::Allocation => DataError::QuotaExceeded,
        _ => DataError::InvalidFrame,
    }
}
pub(super) fn prepare_state<'a>(
    request: &'a Request<'_>,
) -> Result<Vec<PreparedItem<'a>>, DataError> {
    if request.method() != Method::Post
        || request.target() != "/peer/v3/state"
        || request.body().is_empty()
        || request.body().len() > 1024 * 1024
    {
        return Err(DataError::InvalidFrame);
    }
    let mut remaining = request.body();
    let mut items = Vec::new();
    while !remaining.is_empty() {
        if items.len() == 64 {
            return Err(DataError::QuotaExceeded);
        }
        let (kind, payload, rest) = framing::decode_frame_prefix(remaining).map_err(frame_error)?;
        if kind != MessageType::StateItemChunk {
            return Err(DataError::InvalidFrame);
        }
        let item = StateItemChunk::decode(payload).map_err(|_| DataError::InvalidFrame)?;
        if item.offset() != 0
            || item.canonical_size() != item.bytes().len() as u64
            || item.input_digest() != *blake3::hash(item.bytes()).as_bytes()
        {
            return Err(DataError::InvalidFrame);
        }
        items.try_reserve(1).map_err(|_| DataError::QuotaExceeded)?;
        items.push(PreparedItem {
            delivery: item.delivery(),
            digest: item.input_digest(),
            canonical: item.bytes(),
        });
        remaining = rest;
    }
    Ok(items)
}
pub(super) fn prepare_ack(request: &Request<'_>) -> Result<CursorAck, DataError> {
    if request.method() != Method::Post
        || request.target() != "/peer/v3/acks"
        || request.body().len() > 8 * 1024
    {
        return Err(DataError::InvalidFrame);
    }
    let (kind, payload, rest) =
        framing::decode_frame_prefix(request.body()).map_err(frame_error)?;
    if kind != MessageType::CursorAck || !rest.is_empty() {
        return Err(DataError::InvalidFrame);
    }
    CursorAck::decode(payload).map_err(|_| DataError::InvalidFrame)
}
pub(super) fn prepare_poll(request: &Request<'_>) -> Result<Option<CursorToken>, DataError> {
    if request.method() != Method::Get || !request.body().is_empty() {
        return Err(DataError::InvalidFrame);
    }
    if request.target() == "/peer/v3/events?mode=poll" {
        return Ok(None);
    }
    let cursor = request
        .target()
        .strip_prefix("/peer/v3/events?cursor=")
        .and_then(|v| v.strip_suffix("&mode=poll"))
        .ok_or(DataError::InvalidFrame)?;
    state::decode_cursor_url(cursor)
        .map(Some)
        .map_err(|_| DataError::InvalidFrame)
}

#[cfg(test)]
mod tests;

// Owner cores, dispatched by the installed data routes in `serve`.
pub(super) mod owner;
