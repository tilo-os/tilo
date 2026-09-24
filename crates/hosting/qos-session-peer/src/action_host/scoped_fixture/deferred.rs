//! Real certified RPC admission and mounted final-owner Deferred minting.
//! The native final publication fence is still closed; these tests observe the
//! actual owner document, never invent a durable success/receipt.
use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::automerge::{AutoCommit, ObjId, ROOT, ReadDoc, transaction::Transactable};
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

const TYPE: &str = "probe.deferred-document";
const OWNER: &str = "actual-admitted-deferred-owner";
#[derive(Clone, Default)]
struct Probe {
    mints: Arc<AtomicUsize>,
    authorized: Arc<AtomicUsize>,
    applied: Arc<AtomicUsize>,
    minted: Arc<Mutex<Option<uuid::Uuid>>>,
}
thread_local! { static FACTORY: RefCell<Option<Probe>> = const { RefCell::new(None) }; }
struct Registration;
impl Registration {
    fn install(probe: &Probe) -> Self {
        FACTORY.with(|slot| assert!(slot.replace(Some(probe.clone())).is_none()));
        Self
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        FACTORY.with(|slot| slot.replace(None));
    }
}
pub(super) fn definition(app: &str) -> Option<&'static ApplicationDefinition> {
    if app != "dev.qos.notes" || !FACTORY.with(|slot| slot.borrow().is_some()) {
        return None;
    }
    static DEFINITION: std::sync::OnceLock<ApplicationDefinition> = std::sync::OnceLock::new();
    Some(DEFINITION.get_or_init(|| {
        let original = ::notes::definition();
        ApplicationDefinition {
            app_id: original.app_id,
            title: "Deferred probe",
            icon: IconDescriptor::Glyph("D"),
            record_kind: original.record_kind.clone(),
            required: original.required.clone(),
            optional: original.optional.clone(),
            create: factory,
        }
    }))
}
pub(super) fn observe_authorizer(
    app: &str,
    actual: ui::invocation_scope::PublishedInvocationAuthorizer,
) -> ui::invocation_scope::PublishedInvocationAuthorizer {
    let probe = FACTORY
        .with(|slot| slot.borrow().clone())
        .filter(|_| app == "dev.qos.notes");
    let Some(probe) = probe else {
        return actual;
    };
    Arc::new(move |request| {
        // Observe only AFTER genuine authorization, returning the same lease.
        let lease = actual(request)?;
        probe.authorized.fetch_add(1, Ordering::SeqCst);
        Ok(lease)
    })
}
fn factory(
    context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    let probe = FACTORY
        .with(|slot| slot.borrow().clone())
        .expect("registered real factory");
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<MintedDocument>(TYPE);
    Ok(AppComponent::new(context, Box::new(probe), registry))
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct MintedDocument {
    id: uuid::Uuid,
}
impl tilo_actions::Action for MintedDocument {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        if doc
            .get(ROOT, "deferred-owner")?
            .and_then(|(v, _)| v.to_str().map(str::to_owned))
            .as_deref()
            != Some(OWNER)
        {
            return Err(tilo_actions::ActionError::Refused(
                "wrong actual owner document".into(),
            ));
        }
        let count = doc
            .get(ROOT, "deferred-applied")?
            .and_then(|(v, _)| v.to_u64())
            .unwrap_or(0);
        doc.put(ROOT, "deferred-minted-id", self.id.to_string())?;
        doc.put(ROOT, "deferred-applied", count + 1)?;
        Ok(())
    }
    fn locus(&self) -> tilo_actions::ExecutionLocus {
        tilo_actions::ExecutionLocus::ServerPreferred
    }
}
impl Widget for Probe {
    fn action_directory_invocation_domain(
        &self,
        _: &AutoCommit,
    ) -> ui::widget::ActionDirectoryInvocationDomain {
        // Lookup, pages and published resources are exactly this finite universe.
        ui::widget::ActionDirectoryInvocationDomain::MaterializedOnly
    }
    fn action_directory_revision(&self, _: &AutoCommit) -> Option<String> {
        Some("deferred-probe-v1".into())
    }
    fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::Resource> {
        use tilo_actions::{
            ActionSpec, ContinuationEvidence, ContinuationKey, ContinuationQuery, ExecutionLocus,
        };
        let mut resource = tilo_actions::Resource::new("deferred:probe", "probe", "Deferred probe");
        for (action, confirmation) in [("create", false), ("create-confirmed", true)] {
            let probe = self.clone();
            let mut spec = ActionSpec::deferred(
                action,
                "Create",
                ExecutionLocus::ServerPreferred,
                TYPE,
                move || {
                    probe.mints.fetch_add(1, Ordering::SeqCst);
                    let id = uuid::Uuid::new_v4();
                    *probe.minted.lock().unwrap() = Some(id);
                    tilo_actions::EncodedAction::encode(TYPE, &MintedDocument { id }).unwrap()
                },
            )
            .requiring(tilo_actions::PermissionRequirement::Write)
            .with_continuation_identity(|q| {
                matches!(q, ContinuationQuery::Draft).then(|| ContinuationEvidence::Draft {
                    resolution_domain: ContinuationKey::new("deferred-probe", [b"owner-v1"]),
                    handler: ContinuationKey::new("deferred-probe", [b"handler-v1"]),
                })
            });
            if confirmation {
                spec.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
            }
            resource = resource.with_action(spec);
        }
        vec![resource]
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        self.resources(doc)
            .into_iter()
            .find(|r| r.id.as_str() == id)
    }
    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        Some(ui::widget::ActionDirectoryResourcePage {
            resources: if cursor.is_empty() && max_results > 0 {
                self.resources(doc)
            } else {
                vec![]
            },
            next_cursor: vec![],
            exhausted: true,
        })
    }
    fn document_action_applied(
        &mut self,
        action: &tilo_actions::EncodedAction,
        _: &[ui::automerge::ChangeHash],
        doc: &AutoCommit,
        _: &ObjId,
    ) {
        assert_eq!(action.type_id, TYPE);
        assert_eq!(
            doc.get(ROOT, "deferred-owner").unwrap().unwrap().0.to_str(),
            Some(OWNER)
        );
        self.applied.fetch_add(1, Ordering::SeqCst);
    }
    fn sync(&mut self, _: &mut AutoCommit, _: &ObjId) {}
    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        _: &ui::Theme,
        nodes: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = tree.new_leaf(ui::taffy::Style::default()).unwrap();
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(
        &mut self,
        _: &ui::taffy::TaffyTree<()>,
        _: ui::taffy::NodeId,
        _: ui::kurbo::Point,
    ) {
    }
    fn draw(
        &mut self,
        _: &mut ui::vello::Scene,
        _: ui::kurbo::Affine,
        _: &mut ui::TextRenderer,
        _: &ui::Theme,
        _: &ui::PresenceCtx,
    ) {
    }
    fn handle_event(
        &mut self,
        _: &ui::winit::event::WindowEvent,
        _: f64,
        _: &ui::winit::event::Modifiers,
        _: &mut AutoCommit,
        _: &ObjId,
        _: &mut bool,
        _: &mut Option<ui::taffy::NodeId>,
        _: ui::taffy::NodeId,
        _: &ui::taffy::TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &'static str {
        "DeferredProbe"
    }
    fn get_rect(&self) -> ui::kurbo::Rect {
        ui::kurbo::Rect::ZERO
    }
}
impl AppWidget for Probe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Deferred probe"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("P")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

fn fixture() -> ScopedAppFixture {
    ScopedAppFixture::with_record_seed("dev.qos.notes", |doc| {
        doc.put(ROOT, "deferred-owner", OWNER).unwrap();
        doc.commit();
    })
}
fn bytes(f: &mut ScopedAppFixture) -> Vec<u8> {
    let id = f.record_id();
    f.records().read(&id, |doc| doc.clone().save()).unwrap()
}
fn minted_count(f: &mut ScopedAppFixture) -> u64 {
    let id = f.record_id();
    f.records()
        .read(&id, |doc| {
            doc.get(ROOT, "deferred-applied")
                .unwrap()
                .and_then(|(v, _)| v.to_u64())
                .unwrap_or(0)
        })
        .unwrap()
}
async fn submit_without_owner_pump(
    f: &mut ScopedAppFixture,
    peer: u64,
    resource: &str,
    action: &str,
    confirmation: serde_json::Value,
) -> Result<u64, String> {
    f.request += 1;
    assert!(f.caller.send(CollabRequest::InvokePublishedAction {
        request_id: f.request,
        peer_id: peer,
        resource: resource.into(),
        action: action.into(),
        arguments: serde_json::Map::new(),
        confirmation
    }));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for event in f.caller.poll() {
            if let CollabEvent::PublishedInvocationResult { request_id, result } = event {
                if request_id == f.request {
                    return result.map(|id| id.0).map_err(|e| e.to_string());
                }
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "genuine signed RPC admission timed out"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}
fn assert_queued(f: &ScopedAppFixture, p: &Probe, id: u64) {
    assert!(matches!(
        f.invocation_status(id),
        Some(tilo_actions::InvocationUpdate::Accepted { .. })
    ));
    assert_eq!(
        p.mints.load(Ordering::SeqCst),
        0,
        "signed caller/broker must not mint owner's action"
    );
    assert_eq!(
        p.authorized.load(Ordering::SeqCst),
        0,
        "owner must remain unpumped"
    );
}
async fn owner_mutates_once(f: &mut ScopedAppFixture, p: &Probe, id: u64) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Err(error) = f.pump().await {
            assert_eq!(
                error,
                "startup replay cannot acknowledge invocation contributors"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "did not reach actual closed native publication fence"
        );
    }
    assert_eq!(p.authorized.load(Ordering::SeqCst), 1);
    assert_eq!(p.mints.load(Ordering::SeqCst), 1);
    assert_eq!(p.applied.load(Ordering::SeqCst), 1);
    assert_eq!(minted_count(f), 1);
    let expected = p.minted.lock().unwrap().unwrap().to_string();
    let record = f.record_id();
    assert_eq!(
        f.records()
            .read(&record, |doc| doc
                .get(ROOT, "deferred-minted-id")
                .unwrap()
                .unwrap()
                .0
                .to_str()
                .unwrap()
                .to_owned())
            .unwrap(),
        expected
    );
    assert!(
        !matches!(
            f.invocation_status(id),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ),
        "closed native publication must never be called a durable success"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn deferred_signed_admission_zero_then_exact_owner_mints_once() {
    let p = Probe::default();
    let _registration = Registration::install(&p);
    let mut f = fixture();
    let (peer, resource) = f.wait_action("create").await;
    let before = bytes(&mut f);
    for _ in 0..3 {
        f.pump().await.unwrap();
    }
    let page = f
        .query_directory(runtime_collab::ActionDirectoryQuery {
            peer_id: peer,
            revision: String::new(),
            cursor: vec![],
            needle: String::new(),
            parent_resource: resource.clone(),
            max_results: 8,
            max_bytes: 65536,
            max_work: 64,
            max_depth: 1,
        })
        .await
        .unwrap();
    assert!(
        page.rows
            .iter()
            .any(|r| r.action.as_deref() == Some("create"))
    );
    assert!(matches!(
        f.resolve_directory(peer, &page.revision, &resource, "create")
            .await
            .unwrap(),
        runtime_collab::ActionDirectoryResolution::Found(_)
    ));
    assert_eq!(
        p.mints.load(Ordering::SeqCst),
        0,
        "publication/discovery/resolve/no-op pumps are inert"
    );
    assert_eq!(bytes(&mut f), before);
    let id = submit_without_owner_pump(&mut f, peer, &resource, "create", serde_json::json!({}))
        .await
        .unwrap();
    assert_queued(&f, &p, id);
    assert_eq!(bytes(&mut f), before);
    owner_mutates_once(&mut f, &p, id).await;
    assert_ne!(bytes(&mut f), before);
}
#[tokio::test(flavor = "current_thread")]
async fn deferred_signed_confirmation_refuses_without_mint_then_confirmed_owner_mints_once() {
    let p = Probe::default();
    let _registration = Registration::install(&p);
    let mut f = fixture();
    let (peer, resource) = f.wait_action("create-confirmed").await;
    let before = bytes(&mut f);
    let error = submit_without_owner_pump(
        &mut f,
        peer,
        &resource,
        "create-confirmed",
        serde_json::json!({}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("confirm"), "actual broker refusal: {error}");
    for _ in 0..3 {
        f.pump().await.unwrap();
    }
    assert_eq!(p.mints.load(Ordering::SeqCst), 0);
    assert_eq!(p.authorized.load(Ordering::SeqCst), 0);
    assert_eq!(bytes(&mut f), before);
    let id = submit_without_owner_pump(
        &mut f,
        peer,
        &resource,
        "create-confirmed",
        serde_json::json!({"confirm":true}),
    )
    .await
    .unwrap();
    assert_queued(&f, &p, id);
    owner_mutates_once(&mut f, &p, id).await;
}
#[derive(Clone, Copy, Debug)]
enum Refusal {
    Unavailable,
    Retired,
    Expired,
}
async fn refused_after_admission(reason: Refusal) {
    let p = Probe::default();
    let _registration = Registration::install(&p);
    let mut f = fixture();
    let (peer, resource) = f.wait_action("create").await;
    let before = bytes(&mut f);
    let id = submit_without_owner_pump(&mut f, peer, &resource, "create", serde_json::json!({}))
        .await
        .unwrap();
    assert_queued(&f, &p, id);
    match reason {
        Refusal::Unavailable => f.host.authority.set_ready(false),
        Refusal::Retired => f.retire_policy_origin(),
        Refusal::Expired => f.expire_original_caller(),
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let result = f.pump().await;
        if matches!(reason, Refusal::Unavailable | Refusal::Retired) {
            let error = result.expect_err("actual authority must refuse before owner dispatch");
            assert_eq!(
                error,
                match reason {
                    Refusal::Unavailable => "action transport unavailable",
                    Refusal::Retired => "owner authority unavailable",
                    Refusal::Expired => unreachable!(),
                }
            );
            break;
        }
        result.unwrap();
        if let Some(update) = f.invocation_status(id) {
            if update.is_terminal() {
                let tilo_actions::InvocationUpdate::Failed { error, .. } = update else {
                    panic!("expired signed invocation returned {update:?}");
                };
                assert!(
                    error.message.contains("certificate expired"),
                    "actual expiry refusal: {error:?}"
                );
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expired signed invocation did not fail"
        );
    }
    assert_eq!(p.mints.load(Ordering::SeqCst), 0, "{reason:?} cannot mint");
    assert_eq!(p.authorized.load(Ordering::SeqCst), 0);
    assert_eq!(p.applied.load(Ordering::SeqCst), 0);
    assert_eq!(minted_count(&mut f), 0);
    assert_eq!(bytes(&mut f), before);
    assert!(!matches!(
        f.invocation_status(id),
        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
    ));
}
#[tokio::test(flavor = "current_thread")]
async fn deferred_signed_current_authority_refusal_mints_zero() {
    refused_after_admission(Refusal::Unavailable).await;
}
#[tokio::test(flavor = "current_thread")]
async fn deferred_signed_retired_origin_refusal_mints_zero() {
    refused_after_admission(Refusal::Retired).await;
}
#[tokio::test(flavor = "current_thread")]
async fn deferred_signed_expired_original_caller_mints_zero() {
    refused_after_admission(Refusal::Expired).await;
}
