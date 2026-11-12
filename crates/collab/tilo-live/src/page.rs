//! The transport-independent tilo-page/1 object profile: one window's page as
//! a paint stream — what the page looks like, then the changes since.
//!
//! A web page is foreign content: what it draws is decided by strangers'
//! code, and the device that runs the engine keeps the JS heap, the cookies
//! and the storage. What leaves it is this: the page as a list of paint-ordered
//! items, then patches to it, with the images those items reference sent once
//! by content hash. A viewer draws the list with its own text and vector
//! stack, the same natively and in a page. No pixels and no video travel.
//!
//! # Tracks
//!
//! One track per window: `page/<tab as 16 hex digits>` ([`track`]). It is
//! sealed for the scope's members as video is, so the relay copies what it
//! cannot read, and it is published only while someone shows it: the plane
//! accepts the track when a member asks for it and ends it when the last one
//! stops ([`crate::plane::Plane::wanted_pages`]).
//!
//! # Groups and objects
//!
//! A group starts with a whole frame, so a member that joins late — or whose
//! patches stopped applying — starts at the next group with everything it
//! needs. Each object is one paint payload (the bytes of
//! `tilo_paint_stream::wire::Payload`) with this fixed header:
//!
//! | Bytes | Field |
//! | --- | --- |
//! | 1 | version, `1` |
//! | 1 | flags: bit 0 whole frame |
//! | 2 | reserved, zero |
//! | 8 | sequence (LE), counting objects of the track |
//! | 8 | capture time in microseconds (LE), on the publisher's clock |
//! | 4 | payload length (LE) |
//!
//! followed by the payload. The flags byte says whether an object starts a
//! group, so a relay never decodes a payload to find out.

pub const HEADER_BYTES: usize = 24;
/// Largest paint payload an object carries, leaving the header room on top.
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
/// The largest object: one whole payload.
pub const MAX_OBJECT_BYTES: usize = MAX_PAYLOAD_BYTES + HEADER_BYTES;
/// Objects one group holds at most: a whole frame at least every so many
/// patches, so a viewer that fell behind starts again from a frame it can use.
pub const MAX_GROUP_OBJECTS: u64 = 240;
/// Page tracks one publisher may have forwarded at once: every window of it
/// that someone views.
pub const MAX_TRACKS: usize = 32;

/// The track window `tab` publishes its page on: `page/` and the tab as 16
/// lowercase hex digits.
#[must_use]
pub fn track(tab: u64) -> String {
    super::video::tab_track("page", tab)
}

/// The tab of a page track name, or `None` for any other name.
#[must_use]
pub fn parse_track(name: &str) -> Option<u64> {
    super::video::tab_of("page", name)
}

/// One object of a window's page: a whole frame or the patches since one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageObject {
    /// Whether this object is a whole frame: it starts its group.
    pub whole: bool,
    pub sequence: u64,
    /// Capture time in microseconds on the publisher's clock.
    pub timestamp_us: u64,
    /// One `tilo_paint_stream::wire::Payload`, as its publisher encoded it.
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid tilo-page/1 object")]
pub struct InvalidPageObject;

impl PageObject {
    /// Encode this object in the tilo-page/1 wire layout.
    ///
    /// # Errors
    ///
    /// [`InvalidPageObject`] for an empty or oversized payload.
    pub fn encode(&self) -> Result<Vec<u8>, InvalidPageObject> {
        if self.payload.is_empty() || self.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(InvalidPageObject);
        }
        let length = u32::try_from(self.payload.len()).map_err(|_| InvalidPageObject)?;
        let mut bytes = Vec::with_capacity(HEADER_BYTES.saturating_add(self.payload.len()));
        bytes.extend_from_slice(&[1, u8::from(self.whole), 0, 0]);
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&self.timestamp_us.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
        debug_assert_eq!(bytes.len(), HEADER_BYTES);
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    /// Decode one complete, valid tilo-page/1 object.
    ///
    /// # Errors
    ///
    /// [`InvalidPageObject`] for bytes that are not one.
    pub fn decode(bytes: &[u8]) -> Result<Self, InvalidPageObject> {
        let header = validate(bytes)?;
        Ok(Self {
            whole: header.whole,
            sequence: header.sequence,
            timestamp_us: u64::from_le_bytes(field(bytes, 12)?),
            payload: bytes.get(HEADER_BYTES..).ok_or(InvalidPageObject)?.to_vec(),
        })
    }
}

/// What a relay checks of an object without copying its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Whether this object is a whole frame.
    pub whole: bool,
    pub sequence: u64,
}

/// Check one object's header and length.
///
/// # Errors
///
/// [`InvalidPageObject`] for bytes that are not one.
pub fn validate(bytes: &[u8]) -> Result<Header, InvalidPageObject> {
    if bytes.len() <= HEADER_BYTES || bytes.len() > MAX_OBJECT_BYTES {
        return Err(InvalidPageObject);
    }
    let flags = byte(bytes, 1)?;
    if byte(bytes, 0)? != 1 || flags & !1 != 0 || field::<2>(bytes, 2)? != [0, 0] {
        return Err(InvalidPageObject);
    }
    let length = u32::from_le_bytes(field(bytes, 20)?);
    if usize::try_from(length).ok() != bytes.len().checked_sub(HEADER_BYTES) {
        return Err(InvalidPageObject);
    }
    Ok(Header {
        whole: flags & 1 == 1,
        sequence: u64::from_le_bytes(field(bytes, 4)?),
    })
}

fn byte(bytes: &[u8], at: usize) -> Result<u8, InvalidPageObject> {
    bytes.get(at).copied().ok_or(InvalidPageObject)
}

/// Read the fixed-width header field at `at`, refusing a short buffer.
fn field<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], InvalidPageObject> {
    let end = at.checked_add(N).ok_or(InvalidPageObject)?;
    bytes
        .get(at..end)
        .and_then(|field| field.try_into().ok())
        .ok_or(InvalidPageObject)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(whole: bool) -> PageObject {
        PageObject {
            whole,
            sequence: 9,
            timestamp_us: 1_000_000,
            payload: vec![0, 1, 2, 3],
        }
    }

    #[test]
    fn a_page_object_round_trips_and_a_malformed_one_is_refused() {
        let whole = object(true);
        let bytes = whole.encode().unwrap();
        assert_eq!(PageObject::decode(&bytes).unwrap(), whole);
        let header = validate(&bytes).unwrap();
        assert!(header.whole);
        assert_eq!(header.sequence, 9);

        let patch = object(false);
        assert!(!validate(&patch.encode().unwrap()).unwrap().whole);

        let mut versioned = bytes.clone();
        versioned[0] = 2;
        assert!(validate(&versioned).is_err());
        let mut flagged = bytes.clone();
        flagged[1] = 0b10;
        assert!(validate(&flagged).is_err());
        let mut reserved = bytes.clone();
        reserved[2] = 1;
        assert!(validate(&reserved).is_err());
        let mut truncated = bytes;
        truncated.pop();
        assert!(validate(&truncated).is_err(), "the length says the payload");
        assert!(
            PageObject {
                payload: Vec::new(),
                ..object(true)
            }
            .encode()
            .is_err()
        );
    }

    #[test]
    fn a_page_track_names_its_window_and_nothing_else_parses_as_one() {
        assert_eq!(track(7), "page/0000000000000007");
        assert_eq!(parse_track("page/0000000000000007"), Some(7));
        for name in [
            "page/",
            "page/000000000000007",
            "page/5EED000000000007",
            "window/5eed000000000007",
            "pages/5eed000000000007",
        ] {
            assert_eq!(parse_track(name), None, "{name}");
        }
    }
}
