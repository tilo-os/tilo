//! SDK memory/correlation tests, not a replacement for native owner acceptance.
use super::*;
use crate::invocation_scope::{
    NativeMessageStreamLifetime,
    native_message::tests::{RecordFixture, delivered_source, source_channel},
};

struct CertifiedSigner {
    key: ed25519_dalek::SigningKey,
    refuse: bool,
}

impl super::super::SdkCertifiedIdentity for CertifiedSigner {
    fn channel_keys(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        Err(std::io::Error::other("unused in native Message signing test").into())
    }

    fn device_certificate(&self) -> Option<&[u8]> {
        Some(b"fixture")
    }

    fn signing_public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    fn sign_collaboration_join(&self, _: &str) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Err(std::io::Error::other("unused in native Message signing test").into())
    }

    fn sign_collaboration_op(
        &self,
        _: &str,
        _: &str,
        _: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        use ed25519_dalek::Signer as _;
        if self.refuse {
            return Err(std::io::Error::other("injected certified signing refusal").into());
        }
        let op: collab_types::SignedOp = postcard::from_bytes(unsigned_op).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        let signing_bytes = op.signing_bytes().map_err(std::io::Error::other)?;
        Ok(self.key.sign(&signing_bytes).to_bytes())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn certified_launcher_signs_complete_native_document_only_on_plain_committed_route() {
    let lifetime = NativeMessageStreamLifetime::new();
    let source = delivered_source(&lifetime).await;
    let route_spec = source.route().clone();
    let _fixture = RecordFixture::new(source);
    let budget = NativeDocumentBudget::default();
    let mut route = super::super::SdkRoute {
        _checks: None,
        spec: route_spec,
        channel: source_channel().await,
        presence: None,
        workspace_controls: Default::default(),
        committed: true,
        encryption: Arc::new(Mutex::new(
            crate::collaboration::route_keys::RouteKeys::plain(),
        )),
        native_documents: NativeDocumentQueue::new(budget),
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let mut signer = super::super::LocalSdkDocumentSigner::Inherited(
        super::super::SdkDocumentSigner::Certified(Arc::new(CertifiedSigner {
            key,
            refuse: false,
        })),
    );
    let mutation = collab_types::DocumentMutation::Incremental(vec![1, 2, 3]);
    let bytes = sign_complete_native_document(&route, &mut signer, &mutation)
        .await
        .unwrap();
    let op: collab_types::SignedOp = postcard::from_bytes(&bytes).unwrap();
    collab_types::verify_op(&op).unwrap();

    route.committed = false;
    assert!(
        sign_complete_native_document(&route, &mut signer, &mutation)
            .await
            .is_err(),
        "certified signer must not mint native custody without a committed owner"
    );
    route.committed = true;
    route.encryption = Arc::new(Mutex::new(
        crate::collaboration::route_keys::RouteKeys::requested(),
    ));
    assert!(
        sign_complete_native_document(&route, &mut signer, &mutation)
            .await
            .is_err(),
        "certified signer must not bypass waiting or encrypted route admission"
    );

    route.encryption = Arc::new(Mutex::new(
        crate::collaboration::route_keys::RouteKeys::plain(),
    ));
    let mut refused = super::super::LocalSdkDocumentSigner::Inherited(
        super::super::SdkDocumentSigner::Certified(Arc::new(CertifiedSigner {
            key: ed25519_dalek::SigningKey::from_bytes(&[43; 32]),
            refuse: true,
        })),
    );
    assert!(
        sign_complete_native_document(&route, &mut refused, &mutation)
            .await
            .is_err(),
        "certified signing denial must not fall back to another signer"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn staged_actual_executor_contribution_is_charged_and_duplicate_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let lifetime = NativeMessageStreamLifetime::new();
            let fixture = RecordFixture::new(delivered_source(&lifetime).await);
            fixture.apply(false).unwrap();
            let contribution = fixture.capture.finish().unwrap();
            let budget = NativeDocumentBudget::default();
            let route = super::super::SdkRoute {
                _checks: None,
                spec: contribution.source().route().clone(),
                channel: source_channel().await,
                presence: None,
                workspace_controls: Default::default(),
                committed: true,
                encryption: Arc::new(Mutex::new(
                    crate::collaboration::route_keys::RouteKeys::plain(),
                )),
                native_documents: NativeDocumentQueue::new(budget.clone()),
            };
            let mut queue = NativeDocumentQueue::new(budget.clone());
            queue.stage(&route, contribution.clone()).unwrap();
            let charged = *budget.0.lock().unwrap();
            assert!(charged >= contribution.retained_bytes().unwrap());
            assert!(queue.stage(&route, contribution.clone()).is_err());
            assert_eq!(*budget.0.lock().unwrap(), charged);
            queue.observed_terminal(7, Some(contribution.source().request().invocation));
            assert!(
                queue.claimed(),
                "unsaved input cannot be released by terminal acknowledgment"
            );
            assert_eq!(*budget.0.lock().unwrap(), charged);
            drop(queue);
            assert_eq!(*budget.0.lock().unwrap(), 0);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn full_shared_budget_refuses_before_staging_and_retirement_stops_fresh_stage() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let lifetime = NativeMessageStreamLifetime::new();
            let fixture = RecordFixture::new(delivered_source(&lifetime).await);
            fixture.apply(false).unwrap();
            let contribution = fixture.capture.finish().unwrap();
            let budget = NativeDocumentBudget::default();
            let route = super::super::SdkRoute {
                _checks: None,
                spec: contribution.source().route().clone(),
                channel: source_channel().await,
                presence: None,
                workspace_controls: Default::default(),
                committed: true,
                encryption: Arc::new(Mutex::new(
                    crate::collaboration::route_keys::RouteKeys::plain(),
                )),
                native_documents: NativeDocumentQueue::new(budget.clone()),
            };
            let mut other = NativeDocumentQueue::new(budget.clone());
            other.resize_charge(MAX_UPLOAD_BYTES).unwrap();
            let mut queue = NativeDocumentQueue::new(budget.clone());
            assert!(queue.stage(&route, contribution.clone()).is_err());
            assert!(!queue.claimed());
            assert_eq!(*budget.0.lock().unwrap(), MAX_UPLOAD_BYTES);
            drop(other);
            drop(lifetime);
            assert!(queue.stage(&route, contribution).is_err());
            assert!(!queue.claimed());
            assert_eq!(*budget.0.lock().unwrap(), 0);
        })
        .await;
}
