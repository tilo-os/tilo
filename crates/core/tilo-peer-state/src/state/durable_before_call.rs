//! Durable layouts 12 and 13, brought up to this build's once.
//!
//! Both kept the writes a state holds for a prerequisite and each encrypted record's sealed
//! writes as signed ops whose claims name no call ([`collab_types::before_call`]); 13 differs
//! from 12 only in the definitions a certificate's capabilities are pinned by, not in a byte it
//! writes. Each such op comes up as this build's op with the signature its author made, which
//! still verifies, so a device that is the only one holding a workspace keeps every write of it.
//!
//! A write's admission is named by the hash of its bytes ([`crate::DocumentAdmission`]). Written
//! again as this build writes it, a sealed write's bytes change, so its entry in the exact index
//! moves with it: from the hash of the bytes it was kept as to the hash of today's.

use std::collections::{BTreeMap, VecDeque};

use collab_types::before_call::SignedOpBeforeCall;
use serde::Deserialize;

use super::durable::Durable;
use super::{DeviceCert, DurableDocument, ExactAdmissions, Route, err};
use crate::NeighbourMsg;

/// The durable layouts brought up here.
pub(super) const BEFORE_CALL: std::ops::RangeInclusive<u32> = 12..=13;

/// One document write held for a prerequisite, as layouts 12 and 13 kept it.
#[derive(Deserialize)]
struct DocumentWriteBeforeCall {
    peer: neighbour_wire::PeerId,
    app_id: String,
    cluster_id: String,
    document_id: String,
    envelope: SignedOpBeforeCall,
    readers: Option<String>,
}

/// `bytes`, a durable state of layout 12 or 13, in this build's layout `version`.
pub(super) fn brought_up(bytes: &[u8], version: u32) -> Result<Vec<u8>, String> {
    type Head = (u32, String, [u8; 32], Option<Vec<u8>>, usize);
    let ((_, workspace, owner, workspace_bytes, count), mut rest) =
        postcard::take_from_bytes::<Head>(bytes).map_err(|error| format!("its head: {error}"))?;
    let mut documents = Vec::new();
    for at in 0..count {
        let (document, after) = postcard::take_from_bytes::<DurableDocument>(rest)
            .map_err(|error| format!("document {at}: {error}"))?;
        documents.push(document);
        rest = after;
    }
    let ((receipts, certificates, revocations, held), mut rest) = postcard::take_from_bytes::<(
        BTreeMap<String, Vec<u8>>,
        Vec<DeviceCert>,
        Vec<tilo_user_identity::RevocationEntry>,
        usize,
    )>(rest)
    .map_err(|error| format!("its receipts and certificates: {error}"))?;
    let mut pending = VecDeque::new();
    for at in 0..held {
        let pending_write = |error: String| format!("held write {at}: {error}");
        let (source, after) = postcard::take_from_bytes::<[u8; 32]>(rest)
            .map_err(err)
            .map_err(pending_write)?;
        let (message, after) = message_before_call(after).map_err(pending_write)?;
        let (replayed, after) = postcard::take_from_bytes::<bool>(after)
            .map_err(err)
            .map_err(pending_write)?;
        pending.push_back((source, message, replayed));
        rest = after;
    }
    let ((_, exact), mut rest) = postcard::take_from_bytes::<(usize, Option<&[u8]>)>(rest)
        .map_err(|error| format!("its exact admissions: {error}"))?;
    let (routes, after) = postcard::take_from_bytes::<usize>(rest).map_err(err)?;
    rest = after;
    let mut kept = Vec::with_capacity(routes.min(rest.len()));
    for _ in 0..routes {
        let ((route, count), after) = postcard::take_from_bytes::<(Route, usize)>(rest)
            .map_err(|error| format!("a sealed history's record: {error}"))?;
        rest = after;
        let mut ops = Vec::with_capacity(count.min(rest.len()));
        for at in 0..count {
            let (op, after) = postcard::take_from_bytes::<SignedOpBeforeCall>(rest)
                .map_err(|error| format!("sealed write {at} of {route:?}: {error}"))?;
            ops.push(op);
            rest = after;
        }
        kept.push((route, ops));
    }
    if !rest.is_empty() {
        return Err("trailing bytes after durable state".into());
    }
    let mut index = exact
        .map(ExactAdmissions::decode)
        .transpose()
        .map_err(|error| format!("exact admissions: {error}"))?;
    let mut sealed = Vec::with_capacity(kept.len());
    for (route, ops) in kept {
        let mut writes = Vec::with_capacity(ops.len());
        for (at, op) in ops.into_iter().enumerate() {
            let was = *blake3::hash(&op.kept_bytes().map_err(err)?).as_bytes();
            let op = op
                .into_op()
                .map_err(|error| format!("sealed write {at} of {route:?}: {error}"))?;
            if let Some(index) = index.as_mut() {
                let now =
                    crate::DocumentAdmission::from_op(&workspace, &route.app, &route.record, &op)
                        .map_err(err)?;
                index.moved(&now.key, &was, now.operation_id);
            }
            writes.push(op);
        }
        sealed.push((route, writes));
    }
    let mut pending_bytes = 0usize;
    for (_, message, _) in &pending {
        pending_bytes = pending_bytes
            .saturating_add(crate::postcard_sink::encoded_len(message, usize::MAX).map_err(err)?);
    }
    let exact = index
        .map(|index| index.encode())
        .transpose()
        .map_err(|error| format!("exact admissions: {error}"))?;
    postcard::to_allocvec(&Durable {
        version,
        workspace,
        owner,
        workspace_bytes,
        documents,
        receipts,
        certificates,
        revocations,
        pending,
        pending_bytes,
        exact: exact.as_deref(),
        sealed,
    })
    .map_err(err)
}

/// The one message at the start of `bytes` as layouts 12 and 13 kept it, as this build's, and
/// the bytes after it: a document write read in that layout's claims, any other message as it
/// is.
fn message_before_call(bytes: &[u8]) -> Result<(NeighbourMsg, &[u8]), String> {
    let Some(fields) = bytes.strip_prefix(&[NeighbourMsg::APP_DOCUMENT_TAG]) else {
        return postcard::take_from_bytes::<NeighbourMsg>(bytes).map_err(err);
    };
    let (write, rest) =
        postcard::take_from_bytes::<DocumentWriteBeforeCall>(fields).map_err(err)?;
    Ok((
        NeighbourMsg::AppDocument {
            peer: write.peer,
            app_id: write.app_id,
            cluster_id: write.cluster_id,
            document_id: write.document_id,
            envelope: Box::new(write.envelope.into_op()?),
            readers: write.readers,
        },
        rest,
    ))
}

/// A frame a store kept before claims named a call, as this build's message: every byte of
/// `bytes` read.
pub(crate) fn frame_before_call(bytes: &[u8]) -> Result<NeighbourMsg, String> {
    let (message, rest) = message_before_call(bytes)?;
    if !rest.is_empty() {
        return Err("trailing bytes after the frame".into());
    }
    Ok(message)
}
