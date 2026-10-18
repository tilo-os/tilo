//! Explicit carrier attribution; all actual admission rules stay in `StatePeer`.
use super::{
    AdmissionError, AdmissionReport, NeighbourMsg, ReadDoc, StatePeer, SystemTime, TransportOrigin,
};

/// A streaming admission transaction for one authenticated native publication cut.
///
/// This legacy semantic transaction does not qualify native allocation bounds.
/// The caller must authenticate the carrier/cut and fence its live state across
/// asynchronous reads. An error consumes the candidate; only `finish` exports it.
pub struct NativePublicationCut {
    candidate: StatePeer,
    prior_pending: [[u8; 32]; 64],
    prior_pending_len: usize,
}
impl NativePublicationCut {
    /// Admit one publication into this cut.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] if the message is refused.
    pub fn push(self, message: NeighbourMsg, now: SystemTime) -> Result<Self, AdmissionError> {
        self.push_report(message, now, false).map(|(cut, _)| cut)
    }

    fn push_report(
        mut self,
        message: NeighbourMsg,
        now: SystemTime,
        strict: bool,
    ) -> Result<(Self, AdmissionReport), AdmissionError> {
        let publisher = self
            .candidate
            .core
            .native_cut_publisher
            .ok_or_else(|| AdmissionError::from("native cut context missing"))?;
        let (source, replayed) = self.candidate.resolve_origin(
            TransportOrigin::RetainedNativePublication { publisher },
            &message,
            now,
        )?;
        let report = self
            .candidate
            .receive_with_context(source, message, now, replayed, strict)?;
        // An IdentityHello or governance snapshot can revoke the publisher while
        // admitting independently signed metadata. Such a cut cannot be exported.
        self.candidate.validate_native_publisher(publisher, now)?;
        Ok((self, report))
    }

    /// Finish the cut, returning the candidate peer.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] if the cut is incomplete.
    pub fn finish(mut self, now: SystemTime) -> Result<StatePeer, AdmissionError> {
        let publisher = self
            .candidate
            .core
            .native_cut_publisher
            .ok_or_else(|| AdmissionError::from("native cut context missing"))?;
        self.candidate.validate_native_publisher(publisher, now)?;
        if self
            .candidate
            .core
            .room_doc
            .as_deref()
            .is_some_and(|room| !room.get_missing_deps(&[]).is_empty())
        {
            return Err("native publication cut has unresolved room history".into());
        }
        if self.candidate.core.pending.len() > 64 {
            return Err("native cut pending queue exceeds entry limit".into());
        }
        // Preserve unresolved relay work that existed before this cut. No new
        // native item may lose publisher provenance in the frozen v4/v5 queue.
        let mut matched = [false; 64];
        for pending in &self.candidate.core.pending {
            let stamp = pending_stamp(pending)?;
            let prior = self
                .prior_pending
                .get(..self.prior_pending_len)
                .unwrap_or_default();
            let Some(slot) = prior
                .iter()
                .zip(matched.iter_mut())
                .find_map(|(prior, matched)| (!*matched && prior == &stamp).then_some(matched))
            else {
                return Err("native publication cut has unresolved input".into());
            };
            *slot = true;
        }
        self.candidate.core.native_cut_publisher = None;
        Ok(self.candidate)
    }
}

// The known tuple serializer writes directly into a fixed-size hash state; it
// neither materializes the wire bytes nor changes the durable queue format.
fn pending_stamp(pending: &([u8; 32], NeighbourMsg, bool)) -> Result<[u8; 32], AdmissionError> {
    let mut hasher = blake3::Hasher::new();
    crate::postcard_sink::hash_into(pending, &mut hasher, usize::MAX)
        .map_err(|_| AdmissionError::from("native cut pending identity could not be encoded"))?;
    Ok(*hasher.finalize().as_bytes())
}

impl StatePeer {
    /// Begin a native publication cut for `publisher`.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] if `publisher` may not publish at `now`.
    pub fn begin_native_publication_cut(
        &self,
        publisher: [u8; 32],
        now: SystemTime,
    ) -> Result<NativePublicationCut, AdmissionError> {
        self.validate_native_publisher(publisher, now)?;
        if self.core.native_cut_publisher.is_some() || self.core.pending.len() > 64 {
            return Err("invalid native cut baseline".into());
        }
        let mut prior_pending = [[0; 32]; 64];
        for (slot, pending) in prior_pending.iter_mut().zip(&self.core.pending) {
            *slot = pending_stamp(pending)?;
        }
        let mut candidate = self.clone();
        candidate.core.native_cut_publisher = Some(publisher);
        Ok(NativePublicationCut {
            candidate,
            prior_pending,
            prior_pending_len: self.core.pending.len(),
        })
    }

    pub(super) fn receive_native_publication(
        &mut self,
        publisher: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        strict: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        let (cut, report) = self
            .begin_native_publication_cut(publisher, now)?
            .push_report(message, now, strict)?;
        *self = cut.finish(now)?;
        Ok(report)
    }

    pub(super) fn validate_native_publisher(
        &self,
        publisher: [u8; 32],
        now: SystemTime,
    ) -> Result<(), AdmissionError> {
        if publisher != self.core.authority.device_public_key {
            return Err("native publication governor is not pinned authority".into());
        }
        self.certificate(&publisher, now)?;
        Ok(())
    }

    pub(super) fn resolve_origin(
        &self,
        origin: TransportOrigin,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Result<([u8; 32], bool), AdmissionError> {
        match origin {
            TransportOrigin::RelayDelta(source) => Ok((source, false)),
            TransportOrigin::RetainedRelayDelta(source) => Ok((source, true)),
            TransportOrigin::RetainedNativePublication { publisher } => {
                // The authenticated adapter supplies the admitted canonical
                // governor, NOT the separate TLS/delivery-session host. A tag or
                // untrusted DTO field alone cannot establish this carrier.
                self.validate_native_publisher(publisher, now)?;
                let source = match message {
                    NeighbourMsg::SyncResp { .. } | NeighbourMsg::SanitizedRoomBaseline { .. } => {
                        publisher
                    }
                    // Existing admission independently verifies this original
                    // signer, route, peer handle and current policy. Hosting a
                    // retained packet does not change its signed authorship.
                    NeighbourMsg::AppDocument { envelope, .. } => *envelope.author.as_bytes(),
                    NeighbourMsg::IdentityHello { cert, .. } => cert.device_public_key,
                    _ => return Err("unsupported native publication family".into()),
                };
                Ok((source, true))
            }
            TransportOrigin::UntrustedPresence(source) => {
                if matches!(
                    message,
                    NeighbourMsg::SyncResp { .. }
                        | NeighbourMsg::AppDocument { .. }
                        | NeighbourMsg::SanitizedRoomBaseline { .. }
                ) {
                    return Err("state requires relay-authenticated Delta carrier".into());
                }
                Ok((source, false))
            }
        }
    }
}
#[cfg(test)]
mod tests;
