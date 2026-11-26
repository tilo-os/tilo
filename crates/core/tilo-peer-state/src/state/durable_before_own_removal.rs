//! Durable layouts 10 and 11, and a store's frames before version 5, brought up to the layout
//! after them: the one [`super::durable_before_call`] reads, which brings them the rest of the way.
//!
//! Each kept with every document, and with every document write waiting to be sent or kept as a
//! frame, the member's own record it was. Nothing reads that any more, so it is dropped; every
//! other byte is kept as it was written, a write's envelope as its author signed it included.
//! Layout 10 was written twice: in the later one a waiting document write ends with the reader set
//! it is sealed for, as in layout 11, and in the earlier one it names none, which it comes up as.
//! A store's frames name none at version 1 and do from version 2.
//!
//! The released build keeps each workspace's state at layout 10 and its frames at version 1, one
//! file each, so these are what a device updating from it holds: its workspaces, and the writes
//! it is the only one to hold.

use std::collections::BTreeMap;

use collab_types::before_call::SignedOpBeforeCall;

use super::{DeviceCert, DurableDocument, Route, err};
use crate::NeighbourMsg;

/// The durable layouts brought up here.
pub(super) const BEFORE_OWN_REMOVAL: std::ops::RangeInclusive<u32> = 10..=11;

/// The layout written once only the earlier of layout 10's two layouts was: its document writes
/// name no reader set.
const BEFORE_READER_SETS: u32 = 10;

/// A member's own record a document or a write was, as these layouts kept it: its kind and its
/// person. Read only to step past it.
type Own = Option<(String, [u8; 32])>;

/// A document write's fields up to and including its envelope, as these layouts and the one
/// after them write them alike.
type WriteHead = (
    neighbour_wire::PeerId,
    String,
    String,
    String,
    SignedOpBeforeCall,
);

/// The fields of a state after its documents and before the writes waiting to be sent, then
/// their count.
type BeforeWaiting = (
    BTreeMap<String, Vec<u8>>,
    Vec<DeviceCert>,
    Vec<tilo_user_identity::RevocationEntry>,
    usize,
);

/// Whether a state's waiting document writes end with a reader set: every one of layout 11 does,
/// and those of layout 10 do in the later of its two layouts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReaderSets {
    Named,
    NotYet,
}

impl ReaderSets {
    /// The layouts a state of durable `version` may be in, the one to read first first: a state of
    /// layout 10 that does not read as the later layout is read as the earlier.
    fn of(version: u32) -> &'static [Self] {
        if version == BEFORE_READER_SETS {
            &[Self::Named, Self::NotYet]
        } else {
            &[Self::Named]
        }
    }

    /// What `read` makes of a state of durable `version` in the first of its layouts it reads
    /// in ([`Self::of`]), or why it reads in none of them.
    pub(super) fn first_read<T>(
        version: u32,
        read: impl Fn(Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut errors = Vec::new();
        for reader_sets in Self::of(version) {
            match read(*reader_sets) {
                Ok(read) => return Ok(read),
                Err(error) => errors.push(match reader_sets {
                    Self::Named => error,
                    Self::NotYet => format!("written before reader sets, {error}"),
                }),
            }
        }
        Err(errors.join("; "))
    }
}

/// The bytes of `from` before `rest`, the part of it a read took.
fn taken<'a>(from: &'a [u8], rest: &[u8]) -> &'a [u8] {
    from.get(..from.len().saturating_sub(rest.len()))
        .unwrap_or_default()
}

/// `bytes`, a durable state of layout 10 or 11, in this build's layout `version`: written as
/// layout 12 and brought up from there ([`super::durable_before_call::brought_up`]).
pub(super) fn brought_up(bytes: &[u8], version: u32) -> Result<Vec<u8>, String> {
    let (kept, _) = postcard::take_from_bytes::<u32>(bytes).map_err(err)?;
    ReaderSets::first_read(kept, |reader_sets| {
        at_12(bytes, reader_sets)
            .and_then(|at_12| super::durable_before_call::brought_up(&at_12, version))
    })
}

/// `bytes`, a durable state of layout 10 or 11, as layout 12 writes it: its version 12, and each
/// document and each waiting document write without the member's own record it was, a write of
/// the earlier layout 10 naming no reader set. Every other byte is the state's own: the bytes the
/// waiting writes take are counted again where layout 12 is brought up.
fn at_12(bytes: &[u8], reader_sets: ReaderSets) -> Result<Vec<u8>, String> {
    type Head<'a> = (String, [u8; 32], Option<&'a [u8]>, usize);
    let (_, mut rest) = postcard::take_from_bytes::<u32>(bytes).map_err(err)?;
    let mut out =
        postcard::to_allocvec(super::durable_before_call::BEFORE_CALL.start()).map_err(err)?;
    let ((_, _, _, count), after) = postcard::take_from_bytes::<Head<'_>>(rest)
        .map_err(|error| format!("its head: {error}"))?;
    out.extend_from_slice(taken(rest, after));
    rest = after;
    for at in 0..count {
        (_, rest) =
            take_document(rest, &mut out).map_err(|error| format!("document {at}: {error}"))?;
    }
    let ((_, _, _, waiting), after) = postcard::take_from_bytes::<BeforeWaiting>(rest)
        .map_err(|error| format!("its receipts and certificates: {error}"))?;
    out.extend_from_slice(taken(rest, after));
    rest = after;
    for at in 0..waiting {
        let waiting_write = |error: String| format!("waiting write {at}: {error}");
        let (_, after) = postcard::take_from_bytes::<[u8; 32]>(rest)
            .map_err(err)
            .map_err(waiting_write)?;
        out.extend_from_slice(taken(rest, after));
        let (message, after) = take_message(after, reader_sets).map_err(waiting_write)?;
        out.extend_from_slice(&message);
        let (_, sent) = postcard::take_from_bytes::<bool>(after)
            .map_err(err)
            .map_err(waiting_write)?;
        out.extend_from_slice(taken(after, sent));
        rest = sent;
    }
    // The bytes the waiting writes took, the exact index and the sealed writes follow as layout 12
    // writes them.
    out.extend_from_slice(rest);
    Ok(out)
}

/// The document at the start of `bytes`, as layouts 10 and 11 kept it, appended to `out` without
/// the member's own record it was: its route, and the bytes after it.
fn take_document<'a>(bytes: &'a [u8], out: &mut Vec<u8>) -> Result<(Route, &'a [u8]), String> {
    let (document, after) = postcard::take_from_bytes::<DurableDocument>(bytes).map_err(err)?;
    out.extend_from_slice(taken(bytes, after));
    let (_, rest) = postcard::take_from_bytes::<Own>(after).map_err(err)?;
    Ok((document.route, rest))
}

/// The document at the start of `bytes` as layouts 10 and 11 kept it: its route, and the bytes
/// after it.
pub(super) fn document(bytes: &[u8]) -> Result<(Route, &[u8]), String> {
    take_document(bytes, &mut Vec::new())
}

/// The bytes after the fields of a layout 10 or 11 state that follow its documents and come
/// before its sealed writes, at the start of `bytes`: its receipts, certificates, revocations,
/// waiting writes, the bytes they take and its exact index.
pub(super) fn after_waiting(bytes: &[u8], reader_sets: ReaderSets) -> Result<&[u8], String> {
    let ((_, _, _, waiting), mut rest) =
        postcard::take_from_bytes::<BeforeWaiting>(bytes).map_err(err)?;
    for _ in 0..waiting {
        let (_, after) = postcard::take_from_bytes::<[u8; 32]>(rest).map_err(err)?;
        let (_, after) = take_message(after, reader_sets)?;
        (_, rest) = postcard::take_from_bytes::<bool>(after).map_err(err)?;
    }
    let (_, rest) = postcard::take_from_bytes::<(usize, Option<&[u8]>)>(rest).map_err(err)?;
    Ok(rest)
}

/// The one message at the start of `bytes` as these layouts kept it, as layout 12 writes it, and
/// the bytes after it: a document write without its own record, ending with the reader set it
/// names or with none, any other message as it is.
fn take_message(bytes: &[u8], reader_sets: ReaderSets) -> Result<(Vec<u8>, &[u8]), String> {
    let Some(fields) = bytes.strip_prefix(&[NeighbourMsg::APP_DOCUMENT_TAG]) else {
        let (_, rest) = postcard::take_from_bytes::<NeighbourMsg>(bytes).map_err(err)?;
        return Ok((taken(bytes, rest).to_vec(), rest));
    };
    let (_, after_envelope) = postcard::take_from_bytes::<WriteHead>(fields).map_err(err)?;
    let mut message = taken(bytes, after_envelope).to_vec();
    let (_, rest) = postcard::take_from_bytes::<Own>(after_envelope).map_err(err)?;
    let rest = match reader_sets {
        ReaderSets::Named => {
            let (_, after) = postcard::take_from_bytes::<Option<String>>(rest).map_err(err)?;
            message.extend_from_slice(taken(rest, after));
            after
        }
        ReaderSets::NotYet => {
            message = postcard::to_extend(&None::<String>, message).map_err(err)?;
            rest
        }
    };
    Ok((message, rest))
}

/// A frame a store kept before version 5, as this build's message: every byte of `bytes` read. A
/// document write of version 1 ends with its own record, one of versions 2 to 4 with its reader
/// set after it, and either comes up as the frame version 5 kept
/// ([`super::durable_before_call::frame_before_call`]).
pub(crate) fn frame_before_own_removal(bytes: &[u8]) -> Result<NeighbourMsg, String> {
    let at_5 = match take_message(bytes, ReaderSets::NotYet) {
        Ok((message, [])) => message,
        _ => match take_message(bytes, ReaderSets::Named)? {
            (message, []) => message,
            _ => return Err("trailing bytes after the frame".into()),
        },
    };
    super::durable_before_call::frame_before_call(&at_5)
}
