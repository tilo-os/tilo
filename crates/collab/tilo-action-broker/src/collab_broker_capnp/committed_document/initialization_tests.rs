//! Tests the actual receipt/context gate; durable owner publication is tested
//! separately against signed empty-room enrollment in tilo-joined-room.
use super::{
    CreationReceipt, CreationWorkspace, Initialization, NativeDocumentRead,
    NativeDocumentReadContext, NativeDocumentReads, Reply, Slot,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Service {
    created: AtomicBool,
    commits: AtomicUsize,
    refuse: AtomicBool,
}
impl NativeDocumentReads for Service {
    fn requires_room(&self, _: &str) -> bool {
        true
    }
    fn prepare_application(
        &self,
        _: &NativeDocumentReadContext,
    ) -> Result<Option<CreationWorkspace>, String> {
        Ok(self.created.swap(false, Ordering::AcqRel).then(workspace))
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()
    }
    fn capture(
        &self,
        _: &NativeDocumentReadContext,
        _: Option<[u8; 32]>,
        _: usize,
    ) -> Result<NativeDocumentRead, String> {
        Err("capture not exercised by this receipt unit test".into())
    }
    fn initialize_application(
        &self,
        context: &NativeDocumentReadContext,
        _: [u8; 32],
        _: &str,
        _: &[u8],
    ) -> Result<[u8; 32], String> {
        context.revalidate()?;
        if self.refuse.load(Ordering::Acquire) {
            return Err("durable publication refused".into());
        }
        self.commits.fetch_add(1, Ordering::AcqRel);
        Ok([9; 32])
    }
}
fn workspace() -> CreationWorkspace {
    CreationWorkspace {
        id: "7851d1ef-c4fa-4d23-aa65-5ad85f80a6af".into(),
        name: "Verified room".into(),
    }
}
fn fixture() -> (Initialization, Arc<Service>, Arc<AtomicBool>) {
    let service = Arc::new(Service {
        created: AtomicBool::new(true),
        commits: AtomicUsize::new(0),
        refuse: AtomicBool::new(false),
    });
    let live = Arc::new(AtomicBool::new(true));
    let checked = live.clone();
    let root = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[22; 32]);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Creator".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let context = NativeDocumentReadContext {
        route: crate::collab_broker::CrdtRoute::record("so.tilo.chat", workspace().id, ""),
        certificate,
        development_source: None,
        record_kind: Some("chat.workspace".into()),
        creation: Some(crate::services::ApplicationCreationEvidence {
            package_hash: "a1".repeat(32),
            declared_kinds: vec!["chat.workspace".into()],
        }),
        revalidate: Arc::new(move || {
            if checked.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("original launch custody ended".into())
            }
        }),
    };
    (
        Initialization {
            receipt: CreationReceipt {
                token: [7; 16],
                workspace: workspace(),
            },
            revision: [3; 32],
            context,
            service: service.clone(),
            deadline: std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(30))
                .unwrap(),
            completed: None,
            acknowledged: true,
            capture_id: 1,
        },
        service,
        live,
    )
}
#[test]
fn exact_receipt_retry_is_idempotent_changed_payload_and_scope_are_refused() {
    let (mut initialization, service, live) = fixture();
    for (token, revision, kind) in [
        ([8; 16], [3; 32], "chat.workspace"),
        ([7; 16], [4; 32], "chat.workspace"),
        ([7; 16], [3; 32], "chat.person"),
    ] {
        assert!(
            initialization
                .commit(token, revision, kind, &[1, 2])
                .is_err()
        );
    }
    assert_eq!(service.commits.load(Ordering::Acquire), 0);
    let reply = initialization
        .commit([7; 16], [3; 32], "chat.workspace", &[1, 2])
        .unwrap();
    assert_eq!(reply, Reply::Initialized { revision: [9; 32] });
    assert_eq!(
        initialization
            .commit([7; 16], [3; 32], "chat.workspace", &[1, 2])
            .unwrap(),
        reply
    );
    assert_eq!(service.commits.load(Ordering::Acquire), 1);
    assert!(
        initialization
            .commit([7; 16], [3; 32], "chat.workspace", &[3, 4])
            .is_err()
    );
    live.store(false, Ordering::Release);
    assert!(
        initialization
            .commit([7; 16], [3; 32], "chat.workspace", &[1, 2])
            .is_err()
    );
}
#[test]
fn first_materialization_only_and_refused_publication_do_not_consume_receipt() {
    let (mut initialization, service, _) = fixture();
    assert_eq!(
        initialization
            .context
            .prepare_application(service.as_ref())
            .unwrap(),
        Some(workspace())
    );
    assert_eq!(
        initialization
            .context
            .prepare_application(service.as_ref())
            .unwrap(),
        None
    );
    let mut restore = initialization.context.clone();
    restore.creation = None;
    assert_eq!(restore.prepare_application(service.as_ref()).unwrap(), None);
    initialization.acknowledged = false;
    assert!(
        initialization
            .commit([7; 16], [3; 32], "chat.workspace", &[1, 2])
            .is_err()
    );
    initialization.acknowledged = true;
    service.refuse.store(true, Ordering::Release);
    assert!(
        initialization
            .commit([7; 16], [3; 32], "chat.workspace", &[1, 2])
            .is_err()
    );
    assert!(initialization.completed.is_none());
    assert_eq!(service.commits.load(Ordering::Acquire), 0);
    service.refuse.store(false, Ordering::Release);
    assert!(
        initialization
            .commit([7; 16], [3; 32], "chat.workspace", &[1, 2])
            .is_ok()
    );
    let (mut expired, service, _) = fixture();
    expired.deadline = std::time::Instant::now();
    assert!(
        expired
            .commit([7; 16], [3; 32], "chat.workspace", &[1, 2])
            .is_err()
    );
    assert_eq!(service.commits.load(Ordering::Acquire), 0);
}

#[test]
fn first_capture_only_carries_source_scoped_creation_not_existing_empty() {
    let (initialization, service, _) = fixture();
    let service: Arc<dyn NativeDocumentReads> = service;
    let chunk = || Reply::Chunk {
        capture: 1,
        revision: [3; 32],
        total: 1,
        offset: 0,
        bytes: vec![1],
        token: [4; 16],
    };
    let mut existing = Slot::default();
    assert_eq!(
        existing
            .creation_reply(chunk(), None, &initialization.context, &service)
            .unwrap(),
        chunk()
    );
    assert!(
        existing.initialization.is_none(),
        "empty restored bytes are not creation evidence"
    );
    let mut fresh = Slot::default();
    assert!(matches!(
        fresh
            .creation_reply(
                chunk(),
                Some(workspace()),
                &initialization.context,
                &service
            )
            .unwrap(),
        Reply::Created { .. }
    ));
    assert!(
        fresh
            .creation_reply(
                chunk(),
                Some(workspace()),
                &initialization.context,
                &service
            )
            .is_err()
    );
    let mut wrong_room = workspace();
    wrong_room.id = "9a317772-6945-4ec7-aac0-6d1e7b3287cd".into();
    assert!(
        existing
            .creation_reply(chunk(), Some(wrong_room), &initialization.context, &service)
            .is_err()
    );
    assert!(existing.initialization.is_none());
}
