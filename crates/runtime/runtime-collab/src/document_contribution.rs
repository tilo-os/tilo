//! Bounded opaque native contribution transport, not an action authority API.
#[cfg(feature = "client")]
use crate::{ChannelHandle, NotAvailable, collab_capnp};
#[cfg(feature = "client")]
use std::rc::Rc;

/// Matches the existing complete document reassembly budget. This upload may
/// carry inert typed metadata; native signed documents retain their own limit.
pub const MAX_UPLOAD_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_CHUNK_BYTES: usize = 1024 * 1024;
pub const MAX_REFERENCES: usize = 16;
pub const MAX_SOURCE_SPEC_BYTES: usize = 256 * 1024;
/// Existing native complete signed-document frame ceiling.
pub const MAX_SIGNED_OP_BYTES: usize = 3 * 1024 * 1024;

/// Inert uploaded bytes. The broker independently binds the actual executed
/// envelope and committed range to its original source and exact signed op.
/// This structure cannot mint source custody or owner admission.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentContributionBody {
    /// JSON preserves the executor envelope's omitted optional fields.
    pub envelope: Vec<u8>,
    pub changes: Vec<[u8; 32]>,
    pub signed_op: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DocumentContributionSource {
    token: [u8; 32],
    spec_json: String,
}

impl std::fmt::Debug for DocumentContributionSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DocumentContributionSource(..)")
    }
}

impl DocumentContributionSource {
    pub fn token(&self) -> &[u8; 32] {
        &self.token
    }

    pub fn spec_json(&self) -> &str {
        &self.spec_json
    }

    #[cfg(feature = "client")]
    pub(crate) fn decode(
        token: &[u8],
        spec: capnp::text::Reader<'_>,
    ) -> Result<Option<Self>, capnp::Error> {
        if token.is_empty() && spec.is_empty() {
            return Ok(None);
        }
        if token.len() != 32 || spec.is_empty() || spec.len() > MAX_SOURCE_SPEC_BYTES {
            return Err(capnp::Error::failed(
                "invalid native document source".into(),
            ));
        }
        Ok(Some(Self {
            token: token
                .try_into()
                .map_err(|_| capnp::Error::failed("invalid source token".into()))?,
            spec_json: spec.to_str()?.to_owned(),
        }))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DocumentContributionReference([u8; 32]);

impl std::fmt::Debug for DocumentContributionReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DocumentContributionReference(..)")
    }
}

/// One actual writer capability; incomplete uploads cannot supply a reference.
#[cfg(feature = "client")]
pub struct DocumentContributionUpload {
    writer: Rc<collab_capnp::document_contribution_writer::Client>,
    reference: DocumentContributionReference,
    total: usize,
    // None means a sent chunk has no definite successful response. Cancellation
    // or a lost response must never permit retrying an uncertain offset.
    offset: Option<usize>,
}

#[cfg(feature = "client")]
impl DocumentContributionUpload {
    /// Append one contiguous bounded chunk. A sent failure poisons this writer.
    ///
    /// # Errors
    /// Refuses empty/oversized/overflowing chunks and propagates broker refusal.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<(), NotAvailable> {
        let offset = self.offset.ok_or_else(|| {
            capnp::Error::failed("contribution writer outcome is uncertain".into())
        })?;
        let next = offset
            .checked_add(bytes.len())
            .filter(|next| *next <= self.total)
            .ok_or_else(|| capnp::Error::failed("contribution exceeds declared total".into()))?;
        if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
            return Err(capnp::Error::failed("invalid contribution chunk".into()).into());
        }
        let mut request = self.writer.write_request();
        request.get().set_offset(
            u64::try_from(offset)
                .map_err(|_| capnp::Error::failed("contribution offset overflow".into()))?,
        );
        request.get().set_chunk(bytes);
        self.offset = None;
        request.send().promise.await?;
        self.offset = Some(next);
        Ok(())
    }

    /// Retain completed upload custody for exact signed-op publication.
    ///
    /// # Errors
    /// An incomplete upload never exposes its reference.
    pub fn reference(&self) -> Result<DocumentContributionReference, NotAvailable> {
        if self.offset != Some(self.total) {
            return Err(capnp::Error::failed("contribution is incomplete".into()).into());
        }
        Ok(self.reference.clone())
    }
}

#[cfg(feature = "client")]
impl ChannelHandle {
    /// Reserve one bounded upload from this exact stream's opaque source.
    ///
    /// # Errors
    /// Refuses an invalid bound, missing old-peer method, or broker retirement.
    pub async fn open_document_contribution(
        &self,
        source: &DocumentContributionSource,
        total: usize,
    ) -> Result<DocumentContributionUpload, NotAvailable> {
        if total == 0 || total > MAX_UPLOAD_BYTES {
            return Err(capnp::Error::failed("invalid contribution total".into()).into());
        }
        let mut request = self.inner.open_document_contribution_request();
        request.get().set_source_token(source.token());
        request.get().set_total_bytes(
            u64::try_from(total)
                .map_err(|_| capnp::Error::failed("contribution total overflow".into()))?,
        );
        let reply = request.send().promise.await?;
        let reply = reply.get()?;
        let reference = reply
            .get_reference()?
            .try_into()
            .map_err(|_| capnp::Error::failed("invalid contribution reference".into()))?;
        Ok(DocumentContributionUpload {
            writer: Rc::new(reply.get_writer()?),
            reference: DocumentContributionReference(reference),
            total,
            offset: Some(0),
        })
    }

    /// Publish actual signed bytes with exact completed native contributions.
    ///
    /// # Errors
    /// Refuses invalid/duplicate reference lists and propagates owner refusal.
    pub async fn send_with_contributions(
        &self,
        op: &[u8],
        references: &[DocumentContributionReference],
    ) -> Result<(), NotAvailable> {
        if op.is_empty()
            || op.len() > MAX_SIGNED_OP_BYTES
            || references.is_empty()
            || references.len() > MAX_REFERENCES
            || references.iter().enumerate().any(|(index, reference)| {
                references
                    .iter()
                    .take(index)
                    .any(|prior| prior == reference)
            })
        {
            return Err(capnp::Error::failed("invalid contribution reference list".into()).into());
        }
        let mut request = self.inner.send_request();
        request.get().set_op(op);
        let mut refs =
            request
                .get()
                .init_contribution_refs(u32::try_from(references.len()).map_err(|_| {
                    capnp::Error::failed("contribution reference count overflow".into())
                })?);
        for (index, reference) in references.iter().enumerate() {
            refs.set(
                u32::try_from(index).map_err(|_| {
                    capnp::Error::failed("contribution reference index overflow".into())
                })?,
                &reference.0,
            );
        }
        request.send().promise.await?;
        Ok(())
    }
}

#[cfg(all(test, feature = "client"))]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Recipient(Rc<Cell<usize>>);
    impl collab_capnp::channel_handle::Server for Recipient {
        #[expect(
            clippy::unwrap_used,
            reason = "overflow in the controlled RPC fixture call counter must fail the test, never wrap"
        )]
        fn send(
            self: Rc<Self>,
            params: collab_capnp::channel_handle::SendParams,
            _results: collab_capnp::channel_handle::SendResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let result = (|| {
                let request = params.get()?;
                assert_eq!(request.get_op()?.len(), MAX_SIGNED_OP_BYTES);
                assert_eq!(request.get_contribution_refs()?.len(), 1);
                self.0.set(self.0.get().checked_add(1).unwrap());
                Ok(())
            })();
            capnp::capability::Promise::from(result)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_contribution_signed_op_and_reference_bounds_precede_rpc() {
        let calls = Rc::new(Cell::new(0));
        let channel = ChannelHandle {
            inner: Rc::new(capnp_rpc::new_client(Recipient(calls.clone()))),
        };
        let reference = DocumentContributionReference([7; 32]);
        let op = vec![0; MAX_SIGNED_OP_BYTES];
        channel
            .send_with_contributions(&op, std::slice::from_ref(&reference))
            .await
            .unwrap();
        for (bytes, refs) in [
            (Vec::new(), vec![reference.clone()]),
            (vec![0; MAX_SIGNED_OP_BYTES + 1], vec![reference.clone()]),
            (op.clone(), Vec::new()),
            (op.clone(), vec![reference.clone(); 2]),
            (op, vec![reference; MAX_REFERENCES + 1]),
        ] {
            assert!(
                channel
                    .send_with_contributions(&bytes, &refs)
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            calls.get(),
            1,
            "invalid bytes/references must not reach the peer"
        );
    }

    struct FailedWriter(Rc<Cell<usize>>);
    impl collab_capnp::document_contribution_writer::Server for FailedWriter {
        #[expect(
            clippy::unwrap_used,
            reason = "overflow in the controlled failed-write call counter must fail the test, never wrap"
        )]
        fn write(
            self: Rc<Self>,
            _params: collab_capnp::document_contribution_writer::WriteParams,
            _results: collab_capnp::document_contribution_writer::WriteResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            self.0.set(self.0.get().checked_add(1).unwrap());
            capnp::capability::Promise::err(capnp::Error::disconnected("response lost".into()))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_contribution_lost_chunk_response_cannot_retry_or_expose_reference() {
        let calls = Rc::new(Cell::new(0));
        let mut upload = DocumentContributionUpload {
            writer: Rc::new(capnp_rpc::new_client(FailedWriter(calls.clone()))),
            reference: DocumentContributionReference([8; 32]),
            total: 1,
            offset: Some(0),
        };
        assert!(upload.write(&[]).await.is_err());
        assert_eq!(calls.get(), 0, "invalid chunk must not poison or send");
        assert_eq!(upload.offset, Some(0));
        assert!(upload.write(&[1]).await.is_err());
        assert_eq!(upload.offset, None);
        assert!(upload.write(&[1]).await.is_err());
        assert!(upload.reference().is_err());
        assert_eq!(calls.get(), 1, "uncertain offset cannot be sent twice");
    }

    struct SuspendedWriter;
    impl collab_capnp::document_contribution_writer::Server for SuspendedWriter {
        fn write(
            self: Rc<Self>,
            _params: collab_capnp::document_contribution_writer::WriteParams,
            _results: collab_capnp::document_contribution_writer::WriteResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            std::future::pending()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_contribution_cancelled_chunk_remains_uncertain() {
        let mut upload = DocumentContributionUpload {
            writer: Rc::new(capnp_rpc::new_client(SuspendedWriter)),
            reference: DocumentContributionReference([9; 32]),
            total: 1,
            offset: Some(0),
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), upload.write(&[1]))
                .await
                .is_err()
        );
        assert_eq!(upload.offset, None);
        assert!(upload.reference().is_err());
        assert!(upload.write(&[1]).await.is_err());
    }
}
