//! Genuine mounted declaration changes between signed admission and delivery.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::automerge::{AutoCommit, ObjId};
use ui::widget::{AppKind, AppWidget, IconDescriptor, ResourceOperationStart, Widget};

#[derive(Clone, Default)]
struct Probe {
    changed: Arc<AtomicBool>,
    change_at_authorizer: Arc<AtomicBool>,
    authorized: Arc<AtomicUsize>,
    effects: Arc<AtomicUsize>,
}
thread_local! { static FACTORY: RefCell<Option<Probe>> = const { RefCell::new(None) }; }
pub(crate) fn definition(app: &str) -> Option<&'static ApplicationDefinition> {
    if app != "dev.qos.notes" || !FACTORY.with(|slot| slot.borrow().is_some()) {
        return None;
    }
    static DEFINITION: std::sync::OnceLock<ApplicationDefinition> = std::sync::OnceLock::new();
    Some(DEFINITION.get_or_init(|| {
        let original = ::notes::definition();
        ApplicationDefinition {
            app_id: original.app_id,
            title: "Dispatch spec probe",
            icon: IconDescriptor::Glyph("P"),
            record_kind: original.record_kind.clone(),
            required: original.required.clone(),
            optional: original.optional.clone(),
            create: factory,
        }
    }))
}
pub(crate) fn observe_authorizer(
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
        let lease = actual(request)?;
        // A deterministic test interleaving AFTER genuine original admission
        // and authorization, BEFORE SDK declaration resolution/handler. The
        // exact original lease is returned unchanged; no authority is minted.
        probe.authorized.fetch_add(1, Ordering::SeqCst);
        if probe.change_at_authorizer.load(Ordering::SeqCst) {
            probe.changed.store(true, Ordering::SeqCst);
        }
        Ok(lease)
    })
}
fn factory(
    context: HostContext,
    _target: LaunchTarget,
    _restore: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    let probe = FACTORY
        .with(|slot| slot.borrow().clone())
        .expect("actual test factory registration");
    Ok(AppComponent::new(
        context,
        Box::new(probe),
        tilo_actions::ActionRegistry::new(),
    ))
}
impl Widget for Probe {
    fn action_directory_invocation_domain(
        &self,
        _: &AutoCommit,
    ) -> ui::widget::ActionDirectoryInvocationDomain {
        // Every lookup/page result below is freshly materialized by resources().
        // There is no additional lazy target or hidden dispatch declaration.
        ui::widget::ActionDirectoryInvocationDomain::MaterializedOnly
    }
    fn action_directory_revision(&self, _: &AutoCommit) -> Option<String> {
        Some(format!("probe:{}", self.changed.load(Ordering::SeqCst)))
    }
    fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut contract = tilo_actions::ActionContract::view();
        contract.permission = if self.changed.load(Ordering::SeqCst) {
            tilo_actions::PermissionRequirement::Write
        } else {
            tilo_actions::PermissionRequirement::Read
        };
        vec![
            tilo_actions::Resource::new("spec:probe", "probe", "Dispatch spec probe").with_action(
                tilo_actions::ActionSpec::operation(
                    "run",
                    "Run",
                    tilo_actions::OperationKind::Effect,
                    "probe.effect",
                    &(),
                )
                .unwrap()
                .with_contract(contract),
            ),
        ]
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        self.resources(doc)
            .into_iter()
            .find(|resource| resource.id.as_str() == id)
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
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["probe.effect"]
    }
    fn start_resource_operation_scoped(
        &mut self,
        scope: &ui::invocation_scope::ResourceInvocationScope,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        _: &[u8],
        _: &mut AutoCommit,
        _: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect || type_id != "probe.effect" {
            return Ok(None);
        }
        scope.with_effect(|| {
            self.effects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })?;
        Ok(Some(ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
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
        "DispatchSpecProbe"
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
        "Dispatch spec probe"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("P")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}
async fn run(changed: bool) {
    let probe = Probe::default();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            FACTORY.with(|slot| slot.replace(None));
        }
    }
    FACTORY.with(|slot| slot.replace(Some(probe.clone())));
    let _restore = Restore;
    let mut fixture = ScopedAppFixture::new("dev.qos.notes");
    let (peer, resource) = fixture.wait_action("run").await;
    fixture.request += 1;
    assert!(fixture.caller.send(CollabRequest::InvokePublishedAction {
        request_id: fixture.request,
        peer_id: peer,
        resource,
        action: "run".into(),
        arguments: serde_json::Map::new(),
        confirmation: serde_json::json!({})
    }));
    // The broker has its own actual RPC executor. Deliberately do not pump the
    // mounted worker until signed admission has retained the original spec.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let id = loop {
        let mut admitted = None;
        for event in fixture.caller.poll() {
            if let CollabEvent::PublishedInvocationResult { request_id, result } = event {
                if request_id == fixture.request {
                    admitted = Some(result.expect("actual signed admission").0);
                }
            }
        }
        if let Some(id) = admitted {
            break id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "actual admission timed out"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert_eq!(
        probe.effects.load(Ordering::SeqCst),
        0,
        "handler must still be queued"
    );
    assert!(matches!(
        fixture.invocation_status(id),
        Some(tilo_actions::InvocationUpdate::Accepted { .. })
    ));
    probe.changed.store(changed, Ordering::SeqCst);
    let terminal = loop {
        fixture.pump().await.unwrap();
        if let Some(update) = fixture.invocation_status(id) {
            if matches!(
                update,
                tilo_actions::InvocationUpdate::Succeeded { .. }
                    | tilo_actions::InvocationUpdate::Failed { .. }
            ) {
                break update;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "actual worker did not complete"
        );
    };
    if changed {
        assert_eq!(
            probe.effects.load(Ordering::SeqCst),
            0,
            "changed declaration executed under original contract"
        );
        assert!(matches!(
            terminal,
            tilo_actions::InvocationUpdate::Failed { .. }
        ));
    } else {
        assert_eq!(
            probe.effects.load(Ordering::SeqCst),
            1,
            "unchanged genuine control must execute"
        );
        assert!(matches!(
            terminal,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));
    }
}
#[tokio::test(flavor = "current_thread")]
async fn original_dispatch_spec_unchanged_control() {
    run(false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn original_dispatch_spec_changed_before_delivery_refuses() {
    run(true).await;
}

async fn resolved_run(changed: bool) {
    let probe = Probe::default();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            FACTORY.with(|slot| slot.replace(None));
        }
    }
    FACTORY.with(|slot| slot.replace(Some(probe.clone())));
    let _restore = Restore;
    let mut fixture = ScopedAppFixture::new("dev.qos.notes");
    let (peer, resource) = fixture.wait_action("run").await;
    let page = fixture
        .query_directory(runtime_collab::ActionDirectoryQuery {
            peer_id: peer,
            revision: String::new(),
            cursor: vec![],
            needle: String::new(),
            parent_resource: resource.clone(),
            max_results: 8,
            max_bytes: 64 * 1024,
            max_work: 64,
            max_depth: 1,
        })
        .await
        .unwrap();
    assert!(
        page.rows
            .iter()
            .any(|row| row.resource == resource && row.action.as_deref() == Some("run"))
    );
    let resolved = fixture
        .resolve_directory(peer, &page.revision, &resource, "run")
        .await
        .unwrap();
    if !matches!(
        resolved,
        runtime_collab::ActionDirectoryResolution::Found(_)
    ) {
        let current = fixture
            .query_directory(runtime_collab::ActionDirectoryQuery {
                peer_id: peer,
                revision: String::new(),
                cursor: vec![],
                needle: String::new(),
                parent_resource: resource.clone(),
                max_results: 8,
                max_bytes: 64 * 1024,
                max_work: 64,
                max_depth: 1,
            })
            .await
            .unwrap();
        panic!(
            "actual resolve {resolved:?}; offered={} next={} same_rows={} probe_changed={}",
            page.revision,
            current.revision,
            page.rows == current.rows,
            probe.changed.load(Ordering::SeqCst)
        );
    }
    probe.change_at_authorizer.store(changed, Ordering::SeqCst);
    let id = fixture
        .invoke_resolved(
            peer,
            &page.revision,
            &resource,
            "run",
            serde_json::Map::new(),
        )
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let terminal = loop {
        fixture.pump().await.unwrap();
        if let Some(update) = fixture.invocation_status(id) {
            if matches!(
                update,
                tilo_actions::InvocationUpdate::Succeeded { .. }
                    | tilo_actions::InvocationUpdate::Failed { .. }
            ) {
                break update;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "actual resolved invocation did not finish"
        );
    };
    assert_eq!(
        probe.authorized.load(Ordering::SeqCst),
        1,
        "actual original authorizer must have returned a genuine lease"
    );
    if changed {
        assert_eq!(
            probe.effects.load(Ordering::SeqCst),
            0,
            "new RPC must preserve original contract at handler"
        );
        assert!(matches!(
            terminal,
            tilo_actions::InvocationUpdate::Failed { .. }
        ));
    } else {
        assert_eq!(
            probe.effects.load(Ordering::SeqCst),
            1,
            "valid resolved original must progress"
        );
        assert!(matches!(
            terminal,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));
    }
}
#[tokio::test(flavor = "current_thread")]
async fn resolved_dispatch_spec_unchanged_control() {
    resolved_run(false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn resolved_dispatch_spec_changed_after_authorizer_refuses() {
    resolved_run(true).await;
}
