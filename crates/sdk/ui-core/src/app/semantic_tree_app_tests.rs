include!("published_invocation_tests.rs");

include!("hosted_model_tests.rs");

include!("hosted_pump_dirty_tests.rs");

include!("deferred_remote_tests.rs");

use super::*;
use crate::a11y_tree::{SemanticActionKind, SemanticId};
use crate::state::State;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{Button, Widget};
use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use tilo_actions::{
    Action as QAction, ActionError, ActionSpec, Doc, EncodedAction, ExecutionLocus, OperationKind,
    PermissionRequirement, Resource,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
enum CounterAction {
    Bump,
    Reset,
    HostBump,
    SetLabel(String),
}
const COUNTER_TYPE: &str = "test.counter";
const ASYNC_COUNTER_TYPE: &str = "test.async-counter";

#[derive(Clone)]
struct LargeDirectoryProvider {
    catalog: crate::search::SearchCatalog,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    largest_budget: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl LargeDirectoryProvider {
    fn new(records: usize) -> Self {
        let catalog = crate::search::SearchCatalog::new("Large directory");
        // One bulk snapshot, as a provider rebuilding its whole corpus
        // publishes it; later `upsert`s keep their incremental semantics.
        catalog
            .replace_all((0..records).map(|index| {
                let resource = format!("test.item:{index}");
                let address = tilo_actions::ActionAddress::new(resource.clone(), "open");
                (
                    resource,
                    format!("record {index} needle-{index}"),
                    crate::search::SearchResult::for_action(
                        "Large directory",
                        index.to_string(),
                        address,
                    ),
                )
            }))
            .expect("canonical synthetic directory rows");
        Self {
            catalog,
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            largest_budget: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn resource_for(address: &tilo_actions::ActionAddress) -> Option<Resource> {
        address
            .resource
            .strip_prefix("test.item:")?
            .parse::<usize>()
            .ok()?;
        Some(
            Resource::new(address.resource.clone(), "test.item", "Synthetic record").with_action(
                ActionSpec::operation(
                    "open",
                    "Open",
                    OperationKind::View,
                    COUNTER_TYPE,
                    &CounterAction::Bump,
                )
                .ok()?
                .with_contract(tilo_actions::ActionContract::view()),
            ),
        )
    }
}

impl crate::search::SearchProvider for LargeDirectoryProvider {
    fn name(&self) -> &'static str {
        "Large directory"
    }

    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.catalog.stats().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &crate::search::SearchCursor) -> bool {
        self.catalog.cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
        self.catalog.search(query)
    }

    fn search_page(
        &self,
        query: &crate::search::SearchQuery,
        cursor: Option<&crate::search::SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> crate::search::SearchPage {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.largest_budget
            .fetch_max(work_budget, std::sync::atomic::Ordering::Relaxed);
        self.catalog
            .query_page(query.text(), cursor, work_budget, result_limit)
    }

    fn resolve_action_resource(&self, address: &tilo_actions::ActionAddress) -> Option<Resource> {
        Self::resource_for(address)
    }
}

impl QAction for CounterAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            CounterAction::Bump | CounterAction::HostBump => {
                let n = match doc.get(&ROOT, "n") {
                    Ok(Some((v, _))) => v.as_i64().unwrap_or(0),
                    _ => 0,
                };
                doc.put(&ROOT, "n", ScalarValue::Int(n.saturating_add(1)))?;
            }
            CounterAction::Reset => {
                doc.put(&ROOT, "n", ScalarValue::Int(0))?;
            }
            CounterAction::SetLabel(s) => {
                doc.put(&ROOT, "label", s.as_str())?;
            }
        }
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        if matches!(self, CounterAction::HostBump) {
            ExecutionLocus::ServerPreferred
        } else {
            ExecutionLocus::ClientLocal
        }
    }
}

/// One open resource exporting an immediate `bump` and a prompted
/// `set-label`.
struct CounterCard {
    fresh_semantic: bool,
    fresh_dynamic: bool,
    button: Button,
    destructive: bool,
    lazy_resource: Option<Resource>,
    directory_provider: Option<LargeDirectoryProvider>,
    directory_roots: usize,
    post_bump_reset: bool,
    async_bump: bool,
    pending_bump: Option<tilo_actions::InvocationId>,
    scoped_jobs: Vec<crate::invocation_scope::ResourceInvocationScope>,
    scoped_results: Vec<tilo_actions::InvocationUpdate>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    /// Bump the counter as part of every `ping`, as an effect's handler
    /// submits an edit.
    bump_after_ping: bool,
    follow_ups: Vec<crate::widget::controls::AttributedControlInvocation>,
    publication_record: Option<crate::widget::TileHost>,
}

fn counter_resource(destructive: bool) -> Resource {
    counter_resource_with_reset(destructive, false)
}

fn counter_resource_with_reset(destructive: bool, reset_visible: bool) -> Resource {
    let bump = ActionSpec::immediate("bump", "Bump", COUNTER_TYPE, &CounterAction::Bump)
        .unwrap()
        .requiring(PermissionRequirement::Write);
    let bump = if destructive {
        bump.destructive()
    } else {
        bump
    };
    let resource = Resource::new("test.counter", "test.counter", "Counter")
        .with_action(bump)
        .with_action(
            ActionSpec::immediate(
                "host-bump",
                "Host bump",
                COUNTER_TYPE,
                &CounterAction::HostBump,
            )
            .unwrap()
            .requiring(PermissionRequirement::Write),
        )
        .with_action(
            ActionSpec::operation(
                "inspect",
                "Inspect counter",
                OperationKind::View,
                COUNTER_TYPE,
                &CounterAction::Bump,
            )
            .unwrap()
            .with_contract(tilo_actions::ActionContract::view()),
        )
        .with_action(
            ActionSpec::operation(
                "ping",
                "Ping service",
                OperationKind::Effect,
                COUNTER_TYPE,
                &CounterAction::Bump,
            )
            .unwrap()
            .with_contract(tilo_actions::ActionContract::default()),
        )
        .with_action(ActionSpec::prompted(
            "set-label",
            "Set label",
            "Label",
            "name…",
            ExecutionLocus::ClientLocal,
            |s: &str| {
                EncodedAction::encode(COUNTER_TYPE, &CounterAction::SetLabel(s.to_string()))
                    .unwrap()
            },
        ));
    if reset_visible {
        resource.with_action(
            ActionSpec::immediate("reset", "Reset", COUNTER_TYPE, &CounterAction::Reset)
                .unwrap()
                .requiring(PermissionRequirement::Write),
        )
    } else {
        resource
    }
}

impl CounterCard {
    fn new() -> Self {
        Self::with_destructive(false)
    }

    fn with_destructive(destructive: bool) -> Self {
        let resource = counter_resource(destructive);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "bump").unwrap();
        Self {
            button: Button::new("bump", "Bump").on_action(binding),
            fresh_semantic: false,
            fresh_dynamic: false,
            destructive,
            lazy_resource: None,
            directory_provider: None,
            directory_roots: 1,
            post_bump_reset: false,
            async_bump: false,
            pending_bump: None,
            scoped_jobs: vec![],
            scoped_results: vec![],
            resource_result: None,
            bump_after_ping: false,
            follow_ups: vec![],
            publication_record: None,
        }
    }

    fn with_post_bump_reset() -> Self {
        let mut card = Self::new();
        card.post_bump_reset = true;
        card
    }

    fn with_async_post_bump_reset() -> Self {
        let mut card = Self::with_post_bump_reset();
        card.async_bump = true;
        card
    }

    fn with_lazy_resource(resource: Resource) -> Self {
        Self {
            button: Button::new("lazy-resource", "Lazy resource"),
            fresh_semantic: false,
            fresh_dynamic: false,
            destructive: false,
            lazy_resource: Some(resource),
            directory_provider: None,
            directory_roots: 1,
            post_bump_reset: false,
            async_bump: false,
            pending_bump: None,
            scoped_jobs: vec![],
            scoped_results: vec![],
            resource_result: None,
            bump_after_ping: false,
            follow_ups: vec![],
            publication_record: None,
        }
    }

    fn with_directory_provider(provider: LargeDirectoryProvider) -> Self {
        let mut card = Self::new();
        card.directory_provider = Some(provider);
        card
    }

    fn with_directory_roots(count: usize) -> Self {
        let mut card = Self::new();
        card.directory_roots = count;
        card
    }

    fn current_resource(&self, doc: &automerge::AutoCommit) -> Resource {
        let mut resource = counter_resource_with_reset(
            self.destructive,
            self.post_bump_reset && counter_in_doc(doc) > 0,
        );
        if self.fresh_semantic {
            resource.semantic_identity = Some(tilo_actions::ResourceSemanticIdentity {
                kind: "test.counter".into(),
                key: "original-counter".into(),
            });
        }
        if self.fresh_dynamic {
            for action in &mut resource.actions {
                if action.id == "bump" {
                    *action = action
                        .clone()
                        .with_risk(|_| tilo_actions::RiskLevel::Destructive);
                }
            }
        }
        if self.bump_after_ping {
            resource = resource.with_action(
                ActionSpec::operation(
                    "peek",
                    "Peek at the counter",
                    OperationKind::Effect,
                    COUNTER_TYPE,
                    &CounterAction::Bump,
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract {
                    permission: PermissionRequirement::Read,
                    ..tilo_actions::ActionContract::default()
                }),
            );
        }
        if self.async_bump {
            resource.with_action(
                ActionSpec::operation(
                    "async-bump",
                    "Asynchronous bump",
                    OperationKind::Effect,
                    ASYNC_COUNTER_TYPE,
                    &(),
                )
                .unwrap()
                .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
                .returns(tilo_actions::ResultSpec::Job),
            )
        } else {
            resource
        }
    }
}
impl Widget for CounterCard {
    crate::provides!(
        dyn crate::resources::PublishesResources,
        dyn crate::search::capability::SearchSource,
        dyn crate::widget::controls::ActionControls
    );

    fn record_owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
        self.publication_record
            .as_ref()
            .and_then(Widget::history_owner)
            .into_iter()
            .collect()
    }

    fn sync(&mut self, s: &AppModel, p: &ModelPath) {
        let (s, p) = crate::doc::document_ref(s, p);
        self.button.sync(s, p);
    }
    fn settle(&mut self, edits: &mut crate::widget::Edits<'_>, p: &automerge::ObjId) {
        self.button.settle(edits, p);
        let mut actions = crate::action_host::ActionHost::new();
        for scope in std::mem::take(&mut self.scoped_jobs) {
            let invocation = scope.invocation();
            let result = scope.with_document_mutation(edits.document_mut(), |doc| {
                actions
                    .try_dispatch(doc, COUNTER_TYPE, &CounterAction::Bump)
                    .map_err(|error| error.to_string())
            });
            self.scoped_results.push(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "expired".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
        if let Some(invocation) = self.pending_bump.take() {
            actions
                .try_dispatch(edits.document_mut(), COUNTER_TYPE, &CounterAction::Bump)
                .unwrap();
            self.resource_result = Some(tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            });
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let kid = self.button.build_layout(taffy, theme, node_map);
        let node = taffy
            .new_with_children(taffy::Style::default(), &[kid])
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.button]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.button]
    }
    fn name(&self) -> &'static str {
        "CounterCard"
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[COUNTER_TYPE, ASYNC_COUNTER_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let (request, scope) = op.with_custody();
        if scope.is_none()
            && let Some(started) = crate::widget::start_in_children(
                self,
                op,
                &mut crate::widget::Edits::lend(doc.document_mut()),
                path,
            )?
        {
            return Ok(Some(started));
        }
        op.check()?;
        if self.async_bump && request.type_id == ASYNC_COUNTER_TYPE {
            match scope {
                Some(scope) => self.scoped_jobs.push(scope.clone()),
                None => self.pending_bump = Some(request.invocation),
            }
            return Ok(Some(crate::widget::ResourceOperationStart::Accepted));
        }
        if self.bump_after_ping && request.kind == tilo_actions::OperationKind::Effect {
            self.follow_ups.push((
                tilo_actions::ControlInvocation {
                    address: tilo_actions::ActionAddress::new("test.counter", "bump"),
                    arguments: serde_json::Map::new(),
                },
                Some(request.invocation),
            ));
        }
        Ok(
            (request.kind == tilo_actions::OperationKind::Effect
                && request.type_id == COUNTER_TYPE)
                .then(|| {
                    crate::widget::ResourceOperationStart::Completed(
                        tilo_actions::ActionOutput::default(),
                    )
                }),
        )
    }
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if let Some(index) = self
            .scoped_results
            .iter()
            .position(|update| update.invocation() == invocation)
        {
            return Some(self.scoped_results.remove(index));
        }
        if self
            .resource_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            None
        }
    }
}

impl crate::widget::controls::ActionControls for CounterCard {
    fn take_attributed_control_invocation(
        &mut self,
    ) -> Option<crate::widget::controls::AttributedControlInvocation> {
        if self.follow_ups.is_empty() {
            self.take_control_invocation()
                .map(|invocation| (invocation, None))
        } else {
            Some(self.follow_ups.remove(0))
        }
    }
}

impl crate::search::capability::SearchSource for CounterCard {
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        self.directory_provider
            .clone()
            .map(|provider| vec![Box::new(provider) as Box<dyn crate::search::SearchProvider>])
            .unwrap_or_default()
    }
}

impl crate::resources::PublishesResources for CounterCard {
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<Resource> {
        self.lazy_resource
            .is_none()
            .then(|| self.current_resource(doc))
            .into_iter()
            .collect()
    }
    fn action_directory_resource_page(
        &self,
        doc: &automerge::AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<crate::widget::ActionDirectoryResourcePage> {
        let offset = std::str::from_utf8(cursor)
            .ok()
            .and_then(|cursor| cursor.parse::<usize>().ok())
            .unwrap_or(0);
        let end = offset.saturating_add(max_results).min(self.directory_roots);
        let resources = (offset..end)
            .map(|index| {
                if index == 0 {
                    self.current_resource(doc)
                } else {
                    Resource::new(
                        format!("test.counter:{index}"),
                        "test.counter",
                        format!("Counter {index}"),
                    )
                    .with_action(
                        ActionSpec::operation(
                            "inspect",
                            "Inspect counter",
                            OperationKind::View,
                            COUNTER_TYPE,
                            &CounterAction::Bump,
                        )
                        .unwrap()
                        .with_contract(tilo_actions::ActionContract::view()),
                    )
                }
            })
            .collect();
        Some(crate::widget::ActionDirectoryResourcePage {
            resources,
            next_cursor: end.to_string().into_bytes(),
            exhausted: end >= self.directory_roots,
        })
    }
    fn resource(&self, doc: &automerge::AutoCommit, id: &str) -> Option<Resource> {
        if let Some(resource) = self
            .lazy_resource
            .as_ref()
            .filter(|resource| find_action_resource(std::slice::from_ref(resource), id).is_some())
        {
            return Some(resource.clone());
        }
        if id == "test.counter" {
            return Some(self.current_resource(doc));
        }
        id.strip_prefix("test.counter:")
            .and_then(|suffix| suffix.parse::<usize>().ok())
            .filter(|index| *index < self.directory_roots)
            .map(|index| {
                Resource::new(
                    format!("test.counter:{index}"),
                    "test.counter",
                    format!("Counter {index}"),
                )
                .with_action(
                    ActionSpec::operation(
                        "inspect",
                        "Inspect counter",
                        OperationKind::View,
                        COUNTER_TYPE,
                        &CounterAction::Bump,
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
                )
            })
    }
}

// These fixtures inject already admitted deliveries through the public handler.
// The legacy peer models that transport; it claims no native history capture.
pub(super) fn admitted_delivery_manager() -> CollaborationManager {
    CollaborationManager::with_peer_transport(HostedModelTransport(Rc::new(RefCell::new(vec![]))))
}

// A legacy owner may receive an already admitted delivery after its outbound
// channel disconnects. It has no native history/read protocol, and cannot
// publish a terminal result or admit a new outbound action.
struct DisconnectedDeliveryPeer;
impl crate::collaboration::PeerTransport for DisconnectedDeliveryPeer {
    fn is_connected(&self) -> bool {
        false
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Err("fixture outbound channel disconnected".into())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        vec![]
    }
    fn join_route(&mut self, _: crate::collaboration::RouteSpec) -> Result<(), String> {
        Ok(())
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}
fn disconnected_delivery_manager() -> CollaborationManager {
    CollaborationManager::with_peer_transport(DisconnectedDeliveryPeer)
}

fn app() -> App {
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(CounterCard::new()));
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.init_layout();
    app
}

#[test]
fn published_delivery_on_disabled_transport_refuses_before_document_handler() {
    let mut app = app();
    app.collaboration.manager = CollaborationManager::disabled();
    let before = app.state.doc.clone().save();
    app.invoke_published_action(
        81,
        tilo_actions::InvocationId::new(),
        "test.counter".into(),
        "bump".into(),
        None,
        "remote:original",
        None,
    );
    assert_eq!(counter(&app), 0);
    assert_eq!(app.state.doc.clone().save(), before);
    assert!(app.actions.directed_invocations.is_empty());
}

fn lazy_resource(id: &str) -> Resource {
    Resource::new(id, id, "Lazy counter").with_action(
        ActionSpec::immediate("bump", "Bump", COUNTER_TYPE, &CounterAction::Bump)
            .unwrap()
            .requiring(PermissionRequirement::Write),
    )
}

fn lazy_app(resource: Resource) -> App {
    let mut app = App::new(
        State::new(),
        Box::new(CounterCard::with_lazy_resource(resource)),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.init_layout();
    app
}

#[test]
fn background_document_job_from_an_unbound_resource_waits_for_broker_admission() {
    let address = tilo_actions::ActionAddress::new("test.lazy-document-job", "bump");
    let spec = ActionSpec::immediate("bump", "Bump", COUNTER_TYPE, &CounterAction::Bump)
        .unwrap()
        .execution(tilo_actions::ExecutionMode::Background { cancellable: true })
        .returns(tilo_actions::ResultSpec::Job);
    let mut app =
        lazy_app(Resource::new(&address.resource, "test.counter", "Counter").with_action(spec));
    let sent = Rc::new(RefCell::new(Vec::new()));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
    let actor = app.collaboration.local_author.0.clone();
    let started = app
        .invoke_resource_action(ResourceInvocation::new(&address, &actor))
        .unwrap();
    assert!(matches!(
        started,
        crate::widget::ResourceOperationStart::Accepted
    ));
    assert_eq!(counter(&app), 0, "no edit before native broker admission");
    let request = sent
        .borrow()
        .iter()
        .find_map(|request| match request {
            CollabRequest::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
            } => Some(invoke.clone()),
            _ => None,
        })
        .expect("the real action path must submit a broker request");
    assert_eq!(request.target.address, address);
    app.invoke_published_action(
        53,
        request.invocation,
        address.resource,
        address.action,
        None,
        &actor,
        None,
    );
    assert_eq!(
        counter(&app),
        1,
        "the admitted delivery applies exactly once"
    );
    assert_eq!(
        sent.borrow()
            .iter()
            .filter(|request| matches!(
                request,
                CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Invoke(_)
                }
            ))
            .count(),
        1,
        "admitted document delivery must not route in a loop"
    );
}

fn counter(app: &App) -> i64 {
    counter_in_doc(&app.state.doc)
}

fn counter_in_doc(doc: &automerge::AutoCommit) -> i64 {
    match doc.get(&ROOT, "n") {
        Ok(Some((v, _))) => v.as_i64().unwrap_or(0),
        _ => 0,
    }
}

fn activate_bound_button(app: &mut App) {
    let node = app.focusable_order[0];
    let pointer = app.layout.node_map[&node];
    assert!(unsafe {
        (*pointer).perform_action(
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut app.state.doc),
            &app.state.root,
        )
    });
}

#[test]
fn lazy_resources_populate_the_tree_with_stable_identity_and_permissions() {
    // Case `lazy_resource_fallback_invokes_after_retained_cache_snapshot`.
    {
        let resource = lazy_resource("test.lazy");
        let mut app = lazy_app(resource);
        let address = tilo_actions::ActionAddress::new("test.lazy", "bump");
        let snapshot = app.collect_resources();
        assert!(
            tilo_actions::door::resolve(&snapshot, &address).is_none(),
            "the retained snapshot must omit the lazy action"
        );
        assert!(
            app.actions.resource_cache.borrow().is_some(),
            "the snapshot is retained before fallback"
        );

        let actor = app.collaboration.local_author.0.clone();
        let result = app.invoke_resource_action(ResourceInvocation::new(&address, &actor));

        assert!(
            result.is_ok(),
            "the lazy resource should resolve: {result:?}"
        );
        assert_eq!(counter(&app), 1);
    }

    // Case `lazy_resource_fallback_respects_local_permissions`.
    {
        let mut app = lazy_app(lazy_resource("test.lazy"));
        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));

        let address = tilo_actions::ActionAddress::new("test.lazy", "bump");
        let actor = app.collaboration.local_author.0.clone();
        let denied = app.invoke_resource_action(ResourceInvocation::new(&address, &actor));
        assert!(matches!(denied, Err(tilo_actions::DoorError::Unknown(_))));
        assert_eq!(counter(&app), 0, "the denied fallback must not run");

        app.set_permission_ctx(PermissionContext::permissive());
        let allowed = app.invoke_resource_action(ResourceInvocation::new(&address, &actor));
        assert!(
            allowed.is_ok(),
            "restoring write permission should admit it"
        );
        assert_eq!(counter(&app), 1);
    }

    // Case `recursive_lazy_resource_remains_discoverable_and_invocable`.
    {
        let child = lazy_resource("test.lazy.child");
        let tree =
            Resource::new("test.lazy.parent", "test.lazy.parent", "Parent").with_child(child);
        let mut app = lazy_app(tree);
        let resolved = crate::widget::resolve_widget_resource(
            app.root_widget.as_ref(),
            &app.state.doc,
            "test.lazy.child",
        )
        .expect("the recursive lazy child is discoverable");
        assert_eq!(
            find_action_resource(&[resolved], "test.lazy.child")
                .map(|resource| resource.id.as_str()),
            Some("test.lazy.child")
        );

        let address = tilo_actions::ActionAddress::new("test.lazy.child", "bump");
        let actor = app.collaboration.local_author.0.clone();
        let result = app.invoke_resource_action(ResourceInvocation::new(&address, &actor));
        assert!(
            result.is_ok(),
            "the recursive lazy action should resolve: {result:?}"
        );
        assert_eq!(counter(&app), 1);
    }

    // Case `refresh_populates_the_tree_from_app_resources`.
    {
        let mut app = app();
        app.refresh_semantic_tree();
        let tree = app.semantic_tree();
        let node = tree
            .node(&SemanticId("r:test.counter".into()))
            .expect("the app's open resource is a semantic node");
        assert_eq!(node.name.as_deref(), Some("Counter"));
        assert!(
            node.action("bump").is_some(),
            "the resource's action is an affordance"
        );
        assert!(node.action("set-label").is_some());
        // The root owns it (resources hang off the window root).
        let root = tree.node(tree.root()).unwrap();
        assert!(root.children.contains(&SemanticId("r:test.counter".into())));
    }

    // Case `identity_is_stable_across_refreshes`.
    {
        let mut app = app();
        app.refresh_semantic_tree();
        let ids_before: Vec<SemanticId> =
            app.semantic_tree().iter().map(|n| n.id.clone()).collect();
        let delta = app.refresh_semantic_tree();
        assert!(
            delta.is_empty(),
            "a stable frame yields no delta, got {delta:?}"
        );
        let ids_after: Vec<SemanticId> = app.semantic_tree().iter().map(|n| n.id.clone()).collect();
        assert_eq!(ids_before, ids_after, "node identity survives a refresh");
    }
}

#[test]
fn action_bound_controls_use_the_fresh_door_and_never_auto_confirm() {
    // Case `action_bound_control_uses_fresh_door_and_records_history`.
    {
        let mut app = app();
        activate_bound_button(&mut app);
        assert_eq!(counter(&app), 0, "control only queued an intent");
        assert_eq!(app.dispatch_control_invocations(), 1);
        assert_eq!(counter(&app), 1);
        assert!(app.state.peek_last_undo().is_some());
        assert_eq!(
            app.control_action_coverage()[0].status,
            ControlBindingStatus::Resolved
        );
    }

    // Case `a11y_activation_drains_an_action_bound_control_before_returning`.
    {
        let mut app = app();
        let button = app.focusable_order[0];
        // Retained widget ids have the same stable AccessKit mapping used by
        // the native provider. This exercises the public AT-SPI action path,
        // rather than manually draining the widget's queued intent.
        let accesskit_button = accesskit::NodeId(u64::from(button) + 1);

        assert_eq!(
            app.dispatch_a11y_action(accesskit_button, &crate::a11y::Action::Activate),
            None
        );
        assert_eq!(
            counter(&app),
            1,
            "AT-SPI activation must apply the bound action before success is returned"
        );
        assert!(app.state.peek_last_undo().is_some());
    }

    // Case `permission_revocation_and_restore_refresh_bound_control_presentation`.
    {
        let mut app = app();
        let node = app.focusable_order[0];
        let pointer = app.layout.node_map[&node];
        assert!(unsafe { (*pointer).is_enabled() });

        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));
        assert!(!unsafe { (*pointer).is_enabled() });
        assert!(!unsafe {
            (*pointer).perform_action(
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut app.state.doc),
                &app.state.root,
            )
        });

        app.set_permission_ctx(PermissionContext::permissive());
        assert!(unsafe { (*pointer).is_enabled() });
    }

    // Case `action_bound_control_never_auto_confirms_destructive_action`.
    {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_destructive(true)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        activate_bound_button(&mut app);
        assert_eq!(app.dispatch_control_invocations(), 1);
        assert_eq!(counter(&app), 0);
        assert!(app.state.peek_last_undo().is_none());
        assert!(app.action_palette().is_some(), "confirmation UI is open");
    }

    // Case `confirmed_control_preserves_captured_arguments_through_palette_dispatch`.
    {
        let (mut probe, observed) = crate::widget::control_result_probe::Probe::new(false);
        probe.confirmation = true;
        probe.typed_text = true;
        observed.enqueue_text("run", "captured before confirmation");
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));

        assert_eq!(host.dispatch_control_invocations(), 1);
        assert!(
            host.action_palette()
                .is_some_and(crate::action_palette::ActionPalette::in_argument_mode),
            "captured arguments enter confirmation directly"
        );
        assert!(observed.payloads.borrow().is_empty());

        host.actions
            .action_palette
            .as_mut()
            .expect("confirmation palette is open")
            .set_query("CONFIRM");
        assert!(host.action_palette_handle_key(&action_palette_tests::synth_enter()));
        assert_eq!(
            observed.payloads.borrow().as_slice(),
            ["captured before confirmation"],
            "confirmation dispatches the original intent arguments"
        );
    }
}

#[test]
fn broker_and_directed_document_actions_preserve_pending_edits_and_ownership() {
    // Case `local_document_action_cannot_claim_an_admitted_source_invocation`.
    {
        let mut app = app();
        let claimed = tilo_actions::InvocationId::new();
        let ambient = tilo_actions::ActionContext::default().with_source_invocation(Some(claimed));
        let address = tilo_actions::ActionAddress::new("test.counter", "bump");
        let actor = app.collaboration.local_author.0.clone();
        app.invoke_resource_action(
            ResourceInvocation::new(&address, &actor)
                .invocation(Some(claimed))
                .ambient_context(Some(&ambient)),
        )
        .unwrap();
        let context = app
            .state
            .doc
            .get_changes(&[])
            .last()
            .and_then(|change| change.message())
            .and_then(tilo_actions::ActionContext::from_change_message)
            .expect("the local action retained its ordinary history context");
        assert_eq!(context.source_invocation, None);
    }

    // Case `directed_task_context_survives_into_action_history`.
    {
        let mut app = app();
        let resources = app.collect_resources();
        assert!(!resources.is_empty(), "test action surface was rejected");
        let forged_invocation = tilo_actions::InvocationId::new();
        let ambient = tilo_actions::ActionContext::default()
            .with_app("org.tilo.Agent", "Agent")
            .with_purpose("Make the opening concrete")
            .with_group("task:opening", "Rewrite the opening", "task")
            .with_source_invocation(Some(forged_invocation));

        let address = tilo_actions::ActionAddress::new("test.counter", "bump");
        let admitted_invocation = tilo_actions::InvocationId::new();
        let request = ResourceInvocation::new(&address, "agent:Scribe")
            .confirmed(true)
            .broker_admitted(true)
            .invocation(Some(admitted_invocation))
            .ambient_context(Some(&ambient));
        let result = app.invoke_resource_action(request);

        assert!(result.is_ok(), "the directed action failed: {result:?}");
        assert_eq!(counter(&app), 1, "the directed action was applied");
        let changes = app.state.doc.get_changes(&[]);
        let committed = changes.last().expect("the directed action committed");
        assert_eq!(committed.actor_id().to_bytes(), b"agent:Scribe");
        let context = committed
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .expect("the action commit retained semantic provenance");
        assert_eq!(context.action_label, "Bump");
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("test.counter"),
            "the concrete addressed resource stays authoritative"
        );
        assert_eq!(context.purpose, ambient.purpose);
        assert_eq!(context.group, ambient.group);
        assert_eq!(context.app, ambient.app);
        assert_eq!(context.source_invocation, Some(admitted_invocation));
        assert_eq!(
            app.state
                .peek_last_undo()
                .and_then(|entry| entry.author.as_ref()),
            Some(&crate::permissions::UserId("agent:Scribe".into())),
            "undo attribution and durable history use the same actor"
        );
    }

    // Case `broker_invocation_republishes_a_document_dependent_action_surface`.
    {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_post_bump_reset()),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        app.collaboration.manager = admitted_delivery_manager();
        assert!(
            tilo_actions::door::survey(&app.collect_resources())
                .iter()
                .all(|descriptor| descriptor.address.action != "reset"),
            "reset is unavailable before the counter changes"
        );
        let publication_attempts = app.actions.action_surface_publication_attempts;

        app.apply_collab_event_for_test(CollabEvent::Invocation {
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(42)),
            id: 42,
            resource: "test.counter".into(),
            action: "bump".into(),
            argument: None,
            actor: "agent:Scribe".into(),
            context: None,
            native_source: None,
        });

        assert_eq!(counter(&app), 1, "the directed document action ran");
        assert!(
            tilo_actions::door::survey(&app.collect_resources())
                .iter()
                .any(|descriptor| descriptor.address.action == "reset"),
            "the post-mutation surface exposes the newly valid reset action"
        );
        assert_eq!(
            app.actions.action_surface_publication_attempts,
            publication_attempts.saturating_add(1),
            "the broker event batch republishes without waiting for native window input"
        );
    }

    // Case `accepted_broker_invocation_republishes_when_its_terminal_update_arrives`.
    {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_async_post_bump_reset()),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        app.collaboration.manager = admitted_delivery_manager();

        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(
            0xa5a5_a5a5_a5a5_a5a5_0000_0000_0000_002b,
        ));
        app.apply_collab_event_for_test(CollabEvent::Invocation {
            invocation,
            id: 43,
            resource: "test.counter".into(),
            action: "async-bump".into(),
            argument: None,
            actor: "agent:Scribe".into(),
            context: None,
            native_source: None,
        });
        assert_eq!(counter(&app), 0, "admission alone does not mutate");
        assert_eq!(
            app.actions
                .directed_invocations
                .get(&invocation)
                .map(|pending| pending.wire_id),
            Some(43),
            "Accepted remains pending until the widget reports a terminal update"
        );

        app.refresh_focus_order();
        assert_eq!(counter(&app), 1, "the later model pass applied the job");
        assert!(
            tilo_actions::door::survey(&app.collect_resources())
                .iter()
                .any(|descriptor| descriptor.address.action == "reset"),
            "the terminal document now exposes reset"
        );
        let publication_attempts = app.actions.action_surface_publication_attempts;

        assert!(
            app.poll_directed_invocations(),
            "the terminal update arrived"
        );
        assert!(app.actions.directed_invocations.is_empty());
        assert_eq!(
            app.actions.action_surface_publication_attempts,
            publication_attempts.saturating_add(1),
            "terminal lifecycle polling republishes without native input"
        );
    }

    // Case `undecodable_document_action_preserves_pending_edit`.
    {
        let resource = Resource::new("test.invalid", "test.invalid", "Invalid action").with_action(
            ActionSpec::immediate(
                "decode",
                "Decode",
                "test.unregistered",
                &CounterAction::Bump,
            )
            .unwrap(),
        );
        let mut app = lazy_app(resource);
        let before = app.state.doc.get_heads();
        app.state.doc.put(ROOT, "draft", "unfinished").unwrap();
        let pending = app.state.doc.pending_ops();
        let address = tilo_actions::ActionAddress::new("test.invalid", "decode");
        let actor = app.collaboration.local_author.0.clone();
        let result = app.invoke_resource_action(ResourceInvocation::new(&address, &actor));
        assert!(
            matches!(result, Err(tilo_actions::DoorError::UnregisteredType(ref id))
        if id == "test.unregistered"),
            "unexpected result: {result:?}"
        );
        assert_eq!(app.state.doc.pending_ops(), pending);
        assert_eq!(app.state.doc.base_heads(), before);
        assert!(app.state.peek_last_undo().is_none());
    }

    // Case `refused_and_routed_actions_preserve_pending_edit`.
    {
        let mut app = app();
        let before = app.state.doc.get_heads();
        app.state.doc.put(ROOT, "draft", "unfinished").unwrap();
        let pending = app.state.doc.pending_ops();
        for (action, actor) in [
            ("host-bump", app.collaboration.local_author.0.clone()),
            ("inspect", "agent:Scribe".to_string()),
            ("missing", app.collaboration.local_author.0.clone()),
        ] {
            let address = tilo_actions::ActionAddress::new("test.counter", action);
            let result = app.invoke_resource_action(ResourceInvocation::new(&address, &actor));
            assert!(result.is_err(), "unadmitted action must fail: {action}");
            assert_eq!(app.state.doc.pending_ops(), pending, "{action}");
            assert_eq!(app.state.doc.base_heads(), before, "{action}");
            assert!(app.state.peek_last_undo().is_none());
        }
        app.state.doc.put(ROOT, "draft", "finished").unwrap();
        let context = tilo_actions::ActionContext::for_resource(
            "Finish draft",
            "test.draft",
            "note",
            "Draft",
        );
        assert!(app.state.commit_action_authored_with_context(
            None,
            None,
            Some(app.collaboration.local_author.clone()),
            Some(context),
        ));
        let changes = app.state.doc.get_changes(&before);
        assert_eq!(changes.len(), 1, "refusal must not split the pending edit");
        let context = changes[0]
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .unwrap();
        assert_eq!(context.action_label, "Finish draft");
    }

    // Case `host_required_document_action_runs_only_after_broker_admission`.
    {
        let mut app = app();
        let address = tilo_actions::ActionAddress::new("test.counter", "host-bump");

        let actor = app.collaboration.local_author.0.clone();
        let local = app.invoke_resource_action(ResourceInvocation::new(&address, &actor));
        assert!(local.is_err(), "an unbrokered client must fail closed");
        assert_eq!(counter(&app), 0);

        let request = ResourceInvocation::new(&address, "agent:Scribe")
            .confirmed(true)
            .broker_admitted(true)
            .invocation(Some(tilo_actions::InvocationId::new()));
        let admitted = app.invoke_resource_action(request);
        assert!(admitted.is_ok(), "the authoritative broker admitted it");
        assert_eq!(counter(&app), 1);
        assert_eq!(
            app.state
                .peek_last_undo()
                .and_then(|entry| entry.author.as_ref()),
            Some(&crate::permissions::UserId("agent:Scribe".into()))
        );
    }

    // Case `directed_view_action_cannot_steal_the_local_view`.
    {
        let mut app = app();
        let invocation = tilo_actions::InvocationId::new();
        let error = app
            .invoke_resource_action(
                ResourceInvocation::new(
                    &tilo_actions::ActionAddress::new("test.counter", "inspect"),
                    "agent:Scribe",
                )
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(invocation)),
            )
            .expect_err("a remote presentation action must be refused");

        assert_eq!(counter(&app), 0, "the private view action never ran");
        assert!(
            matches!(
                error,
                tilo_actions::DoorError::Unavailable { ref code, .. } if code == "private_view"
            ),
            "unexpected refusal: {error:?}"
        );
    }

    // Case `a_device_only_effect_runs_for_the_person_at_the_device_alone`.
    {
        /// A typed effect vocabulary as an app declares it.
        #[derive(Clone, Debug, tilo_resource::Effect)]
        #[effect(operation = "test.counter")]
        enum Folder {
            /// Copy a folder on this computer.
            #[effect(device_only)]
            CopyLocalFolder,
            /// Count the copies.
            Count,
        }
        let mut resource = Resource::new("test.folder", "test.folder", "Folder");
        for spec in <Folder as tilo_resource::Effect>::specs() {
            resource = resource.with_action(spec);
        }
        let mut app = lazy_app(resource);
        let invoke = |app: &mut App, action: &str, actor: &str| {
            app.invoke_resource_action(
                ResourceInvocation::new(
                    &tilo_actions::ActionAddress::new("test.folder", action),
                    actor,
                )
                .broker_admitted(true)
                .invocation(Some(tilo_actions::InvocationId::new())),
            )
        };
        assert!(matches!(
            invoke(&mut app, "copy-local-folder", "person:guest"),
            Err(tilo_actions::DoorError::Unavailable { ref code, .. }) if code == "local_device_only"
        ));
        assert!(
            invoke(&mut app, "count", "person:guest").is_ok(),
            "a collaborator still reaches the shared effects"
        );
        let me = app.collaboration.local_author.0.clone();
        assert!(invoke(&mut app, "copy-local-folder", &me).is_ok());
    }

    // Case `directed_local_device_effect_cannot_run_for_a_remote_writer`.
    {
        let action = ActionSpec::operation(
            "copy-private-folder",
            "Copy private folder",
            OperationKind::Effect,
            COUNTER_TYPE,
            &CounterAction::Bump,
        )
        .unwrap()
        .with_contract(tilo_actions::ActionContract {
            tags: vec!["tilo.local-device-only".into()],
            ..tilo_actions::ActionContract::default()
        });
        let mut app = lazy_app(
            Resource::new(
                "test.private-folder",
                "test.private-folder",
                "Private folder",
            )
            .with_action(action),
        );
        let address =
            tilo_actions::ActionAddress::new("test.private-folder", "copy-private-folder");
        let remote = app.invoke_resource_action(
            ResourceInvocation::new(&address, "agent:Scribe")
                .broker_admitted(true)
                .invocation(Some(tilo_actions::InvocationId::new())),
        );
        assert!(matches!(
            remote,
            Err(tilo_actions::DoorError::Unavailable { ref code, .. }) if code == "local_device_only"
        ));

        let actor = app.collaboration.local_author.0.clone();
        assert!(
            app.invoke_resource_action(
                ResourceInvocation::new(&address, &actor)
                    .broker_admitted(true)
                    .invocation(Some(tilo_actions::InvocationId::new())),
            )
            .is_ok()
        );
    }
}

#[test]
fn local_control_results_stay_local_and_report_refusal_or_success() {
    // Case `local_control_result_does_not_broadcast_into_component_siblings`.
    {
        use crate::application_test_support::ProbeHost;
        use crate::widget::{Column, TileHost};
        for closed in [false, true] {
            let owner = ProbeHost::new();
            let (component, sibling_seen) =
                crate::widget::control_result_probe::component(&owner, 11, false);
            let (legacy, legacy_seen) = crate::widget::control_result_probe::Probe::new(false);
            let root = Column::new()
                .push(Box::new(legacy))
                .push(Box::new(TileHost::from_component(component)));
            let mut host = App::new_with_controlled_inputs(State::new(), Box::new(root));
            if closed {
                host.close_components();
            }
            legacy_seen.enqueue("missing");
            host.dispatch_control_invocations();
            assert_eq!(legacy_seen.results.borrow().len(), 1);
            assert!(
                sibling_seen.results.borrow().is_empty(),
                "legacy feedback must not enter bound sibling"
            );
            assert_eq!(sibling_seen.calls.get(), 0);
        }
    }

    // Case `bound_local_control_result_targets_only_the_emitting_instance`.
    {
        use crate::application_test_support::ProbeHost;
        use crate::widget::{Column, TileHost};
        let owner = ProbeHost::new();
        let (first, first_seen) = crate::widget::control_result_probe::component(&owner, 11, false);
        let (second, second_seen) =
            crate::widget::control_result_probe::component(&owner, 12, false);
        let root = Column::new()
            .push(Box::new(TileHost::from_component(first)))
            .push(Box::new(TileHost::from_component(second)));
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(root));
        first_seen.enqueue("run");
        host.dispatch_control_invocations();
        assert_eq!(first_seen.results.borrow().len(), 1);
        assert!(second_seen.results.borrow().is_empty());
        assert_eq!(first_seen.calls.get(), 1);
        assert_eq!(second_seen.calls.get(), 0);
    }

    // Case `bound_local_control_result_reports_permission_refusal_and_success`.
    {
        use crate::application_test_support::{ProbeHost, runner_inputs};
        for accepted in [false, true] {
            let owner = ProbeHost::new();
            let (component, observed) =
                crate::widget::control_result_probe::component(&owner, 11, accepted);
            let binding = component.callback_binding();
            let mut host = App::from_component(component, runner_inputs()).unwrap();
            observed.enqueue("run");
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 1);
            let expected = if accepted {
                crate::widget::ResourceOperationStart::Accepted
            } else {
                crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )
            };
            assert_eq!(*observed.results.borrow(), vec![Ok(expected)]);
            observed.enqueue("run");
            *binding.permissions.borrow_mut() = PermissionContext::from_capabilities(
                collab_types::caps::Actor::person(Some(collab_types::role::Role::Viewer), "reader"),
                Vec::new(),
            );
            let before = owner
                .records
                .read(&binding.checkout.id, automerge::AutoCommit::base_heads)
                .unwrap();
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 1, "revoked permission blocks handler");
            assert_eq!(
                owner
                    .records
                    .read(&binding.checkout.id, automerge::AutoCommit::base_heads)
                    .unwrap(),
                before
            );
            assert_eq!(observed.results.borrow().len(), 2);
            assert!(matches!(observed.results.borrow().last(), Some(Err(_))));
        }
    }

    // Case `local_control_result_keeps_confirmation_as_refusal`.
    {
        let (mut probe, observed) = crate::widget::control_result_probe::Probe::new(false);
        probe.confirmation = true;
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));
        observed.enqueue("run");
        host.dispatch_control_invocations();
        assert_eq!(observed.calls.get(), 0);
        assert_eq!(
            *observed.results.borrow(),
            vec![Err(tilo_actions::DoorError::ConfirmationRequired)]
        );
        assert!(host.action_palette().is_some());
    }

    // Case `local_control_result_reports_completed_and_accepted`.
    {
        use crate::widget::control_result_probe::Probe;
        for accepted in [false, true] {
            let (probe, observed) = Probe::new(accepted);
            let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));
            observed.enqueue("run");
            host.dispatch_control_invocations();
            assert_eq!(
                observed.calls.get(),
                1,
                "accepted={accepted}; feedback={:?}",
                observed.results.borrow()
            );
            let expected = if accepted {
                crate::widget::ResourceOperationStart::Accepted
            } else {
                crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )
            };
            assert_eq!(*observed.results.borrow(), vec![Ok(expected)]);
            observed.enqueue("missing");
            host.dispatch_control_invocations();
            assert_eq!(observed.calls.get(), 1);
            assert!(matches!(
                observed.results.borrow().last(),
                Some(Err(tilo_actions::DoorError::Unknown(_)))
            ));
        }
    }

    // Case `local_control_result_reports_fresh_permission_refusal_without_mutation`.
    {
        use crate::widget::control_result_probe::Probe;
        let (probe, observed) = Probe::new(false);
        let mut host = App::new_with_controlled_inputs(State::new(), Box::new(probe));
        observed.enqueue("run");
        let readonly = PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(Some(collab_types::role::Role::Viewer), "reader"),
            Vec::new(),
        );
        host.set_permission_ctx(readonly);
        let before = host.state.doc.base_heads();
        host.dispatch_control_invocations();
        assert_eq!(observed.calls.get(), 0, "permission checked before handler");
        assert_eq!(host.state.doc.base_heads(), before);
        assert_eq!(
            automerge::transaction::Transactable::pending_ops(&host.state.doc),
            0
        );
        assert!(matches!(
            observed.results.borrow().as_slice(),
            [Err(tilo_actions::DoorError::Unknown(_))]
        ));
        assert!(observed.pending.borrow().is_empty());
    }

    // Case `local_effect_completion_does_not_mutate_the_app_document`.
    {
        let mut app = app();
        let before = app.state.doc.get_heads();
        let address = tilo_actions::ActionAddress::new("test.counter", "ping");
        let actor = app.collaboration.local_author.0.clone();
        let result = app.invoke_resource_action(ResourceInvocation::new(&address, &actor));
        assert!(matches!(
            result,
            Ok(crate::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(app.state.doc.get_heads(), before);
        assert!(app.state.peek_last_undo().is_none());
    }
}

fn label(app: &App) -> String {
    match app.state.doc.get(&ROOT, "label") {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

#[test]
fn semantic_invocation_matches_the_affordance_dispatch() {
    // Case `semantic_action_carries_the_same_encoded_action_the_affordance_dispatches`.
    {
        let mut app = app();
        app.refresh_semantic_tree();
        let bump = app
            .semantic_tree()
            .action(&SemanticId("r:test.counter".into()), "bump")
            .expect("bump affordance");

        let mut host = crate::action_host::ActionHost::new();
        let mut host_doc = automerge::AutoCommit::new();
        host.dispatch(&mut host_doc, COUNTER_TYPE, &CounterAction::Bump);
        let from_affordance = host
            .last_dispatched()
            .expect("the affordance dispatched")
            .clone();

        match &bump.kind {
            SemanticActionKind::App(enc) => assert_eq!(
                enc, &from_affordance,
                "the a11y node exposes the same EncodedAction the on-screen control dispatches"
            ),
            other => panic!("expected an app action, got {other:?}"),
        }
    }

    // Case `invoking_an_immediate_action_dispatches_it_through_the_action_model`.
    {
        let mut app = app();
        app.refresh_semantic_tree();
        let id = SemanticId("r:test.counter".into());
        assert_eq!(counter(&app), 0);

        assert!(app.invoke_semantic_action(&id, "bump", None).unwrap());
        assert_eq!(
            counter(&app),
            1,
            "the invoked action mutated the shared doc"
        );
        assert!(app.invoke_semantic_action(&id, "bump", None).unwrap());
        assert_eq!(counter(&app), 2);

        // An unknown action name is a graceful no-op.
        assert!(!app.invoke_semantic_action(&id, "nope", None).unwrap());
        assert_eq!(counter(&app), 2);
    }

    // Case `invoking_a_prompted_action_uses_the_supplied_argument`.
    {
        let mut app = app();
        app.refresh_semantic_tree();
        let id = SemanticId("r:test.counter".into());
        assert!(
            app.invoke_semantic_action(&id, "set-label", Some("hello"))
                .unwrap()
        );
        assert_eq!(
            label(&app),
            "hello",
            "the prompted argument reached the built action"
        );
    }

    // Case `invoke_matches_a_direct_affordance_dispatch_byte_for_byte`.
    {
        let mut app = app();
        app.refresh_semantic_tree();
        app.invoke_semantic_action(&SemanticId("r:test.counter".into()), "bump", None)
            .unwrap();

        let mut host = crate::action_host::ActionHost::new();
        let mut host_doc = automerge::AutoCommit::new();
        host.dispatch(&mut host_doc, COUNTER_TYPE, &CounterAction::Bump);

        let via_a11y = counter(&app);
        let via_affordance = match host_doc.get(&ROOT, "n") {
            Ok(Some((v, _))) => v.as_i64().unwrap_or(0),
            _ => 0,
        };
        assert_eq!(
            via_a11y, via_affordance,
            "both paths produce the same result"
        );
    }
}

#[test]
fn retained_and_virtual_trees_dispatch_through_semantic_routes() {
    // Case `virtual_tree_rows_dispatch_through_platform_and_semantic_routes`.
    {
        let root = crate::widget::TreeNode::branch(
            1,
            "Root",
            (),
            vec![crate::widget::TreeNode::leaf(2, "Child", ())],
        );
        let mut app = App::new(
            State::new(),
            Box::new(
                crate::widget::TreeView::new(root)
                    .with_id("items-tree")
                    .with_expanded([1]),
            ),
        );
        app.init_layout();

        app.refresh_semantic_tree();
        let child = app
            .semantic_tree()
            .iter()
            .find(|node| {
                node.role == crate::a11y::Role::TreeItem && node.name.as_deref() == Some("Child")
            })
            .map(|node| node.id.clone())
            .expect("semantic child row");
        assert!(
            app.invoke_semantic_action(&child, "activate", None)
                .expect("semantic row activation")
        );
        app.refresh_semantic_tree();
        assert!(
            app.semantic_tree()
                .node(&child)
                .is_some_and(|node| node.state.selected)
        );

        let update = app.build_a11y_tree_update(1.0);
        assert_eq!(
            update
                .nodes
                .iter()
                .filter(|(_, node)| node.role() == accesskit::Role::TreeItem)
                .count(),
            2
        );
        let root_row = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::TreeItem)
            .map(|(id, node)| {
                assert!(node.supports_action(accesskit::Action::Collapse));
                *id
            })
            .expect("virtual root row");
        let route = crate::accesskit_bridge::resolve_node_id(
            app.root_widget.as_ref(),
            &app.layout.node_map,
            root_row,
        )
        .expect("root row route");
        assert!(matches!(
            route,
            crate::accesskit_bridge::A11yTarget::Synthetic {
                semantic_target: Some(ref target),
                ..
            } if target == "node:1"
        ));
        assert_eq!(
            app.dispatch_a11y_action(root_row, &crate::a11y::Action::Collapse),
            None
        );
        app.refresh_semantic_tree();
        let root_semantics = app
            .semantic_tree()
            .iter()
            .find(|node| {
                node.role == crate::a11y::Role::TreeItem && node.name.as_deref() == Some("Root")
            })
            .expect("root semantics after collapse");
        assert_eq!(root_semantics.state.expanded, Some(false));

        let collapsed = app.build_a11y_tree_update(1.0);
        let rows = collapsed
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == accesskit::Role::TreeItem)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "collapsing removes the child row");
        assert!(rows[0].1.supports_action(accesskit::Action::Expand));
    }

    // Case `nav_shell_accessible_autocomplete_edits_and_selects_through_host_dispatch`.
    {
        let search = crate::widget::Autocomplete::new()
            .with_id("city-search")
            .with_placeholder("Search city")
            .with_suggester(|query| {
                if query == "Ber" {
                    vec![crate::widget::Suggestion::new("Berlin")]
                } else {
                    Vec::new()
                }
            });
        let nav = crate::widget::NavShell::new()
            .overview(Box::new(search))
            .content(Box::new(crate::widget::Column::new()));
        let mut app = App::new(State::new(), Box::new(nav));
        app.init_layout();
        let update = app.build_a11y_tree_update(1.0);
        let field = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Search city"))
            .unwrap()
            .0;
        app.dispatch_a11y_action(field, &crate::a11y::Action::SetValue("Ber".into()));
        let update = app.build_a11y_tree_update(1.0);
        let result = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Berlin"))
            .expect("native edit must publish a result")
            .0;
        app.dispatch_a11y_action(result, &crate::a11y::Action::Activate);
        let update = app.build_a11y_tree_update(1.0);
        let field = &update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Search city"))
            .unwrap()
            .1;
        assert_eq!(field.value(), Some("Berlin"));
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Berlin"))
        );
    }

    // Case `retained_tree_delegate_is_nested_once_and_keeps_its_own_action`.
    {
        let presses = std::rc::Rc::new(std::cell::Cell::new(0_usize));
        let captured = std::rc::Rc::clone(&presses);
        let root = crate::widget::TreeNode::leaf(7, "Item", ());
        let tree = crate::widget::TreeView::new(root).with_row_delegate(move |_, id, _| {
            let button_id = format!("action-{id}");
            let captured = std::rc::Rc::clone(&captured);
            Box::new(
                crate::widget::Button::new(&button_id, "Action").on_click(move || {
                    captured.set(captured.get().saturating_add(1));
                }),
            )
        });
        let mut app = App::new(State::new(), Box::new(tree));
        app.init_layout();

        let update = app.build_a11y_tree_update(1.0);
        let buttons = update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == accesskit::Role::Button)
            .collect::<Vec<_>>();
        assert_eq!(buttons.len(), 1, "delegate is not announced twice");
        let button_id = buttons[0].0;
        let row = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::TreeItem)
            .map(|(_, node)| node)
            .expect("tree item");
        assert_eq!(row.children(), &[button_id]);
        assert!(
            row.bounds().is_some(),
            "platform row keeps viewport geometry"
        );
        assert_eq!(
            app.dispatch_a11y_action(button_id, &crate::a11y::Action::Activate),
            None
        );
        assert_eq!(presses.get(), 1);

        app.refresh_semantic_tree();
        let semantic_buttons = app
            .semantic_tree()
            .iter()
            .filter(|node| node.role == crate::a11y::Role::Button)
            .collect::<Vec<_>>();
        assert_eq!(semantic_buttons.len(), 1);
        let semantic_row = app
            .semantic_tree()
            .iter()
            .find(|node| node.role == crate::a11y::Role::TreeItem)
            .expect("semantic tree item");
        assert!(semantic_row.children.contains(&semantic_buttons[0].id));
        assert!(
            semantic_row.bounds.is_some(),
            "inspection row keeps viewport geometry"
        );
        assert!(
            !semantic_row.state.selected,
            "button activation does not select row"
        );
    }

    // Case `synthetic_drawn_control_dispatches_through_retained_semantic_tree`.
    {
        let changed = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
        let captured = std::rc::Rc::clone(&changed);
        let group = crate::widget::ToggleGroup::new(
            vec![
                crate::widget::toggle_group::Segment::new("Light"),
                crate::widget::toggle_group::Segment::new("Dark"),
            ],
            0,
        )
        .on_change(move |index| captured.set(index));
        let mut app = App::new(State::new(), Box::new(group));
        app.init_layout();
        app.refresh_semantic_tree();
        let dark = app
            .semantic_tree()
            .find_by_name("Dark")
            .map(|node| node.id.clone())
            .expect("synthetic Dark segment");

        assert!(
            app.invoke_semantic_action(&dark, "activate", None)
                .expect("semantic dispatch")
        );
        assert_eq!(changed.get(), 1);
    }

    // Case `effectful_control_is_not_exported_without_a_domain_contract`.
    {
        let presses = std::rc::Rc::new(std::cell::Cell::new(0_usize));
        let captured = std::rc::Rc::clone(&presses);
        let button = Button::new("drive-me", "Drive me").on_click(move || {
            captured.set(captured.get().saturating_add(1));
        });
        let mut app = App::new(State::new(), Box::new(button));
        app.init_layout();

        let resource = app
            .collect_resources()
            .into_iter()
            .find(|resource| resource.kind == "tilo.ui.control" && resource.label == "Drive me");
        assert!(resource.is_none_or(|resource| {
            resource
                .actions
                .iter()
                .all(|action| action.id != "activate")
        }));
        assert_eq!(presses.get(), 0);
    }
}

#[test]
fn an_effects_edit_is_applied_for_its_invoker_before_it_answers() {
    let mut card = CounterCard::new();
    card.bump_after_ping = true;
    let mut app = App::new(State::new(), Box::new(card));
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.init_layout();
    app.collaboration.manager = disconnected_delivery_manager();
    let invocation = tilo_actions::InvocationId::new();
    app.invoke_published_action(
        7,
        invocation,
        "test.counter".into(),
        "ping".into(),
        None,
        "agent:Scribe",
        None,
    );
    assert_eq!(counter(&app), 1, "the effect's edit was applied");
    assert_eq!(
        app.state
            .peek_last_undo()
            .and_then(|entry| entry.author.as_ref()),
        Some(&crate::permissions::UserId("agent:Scribe".into())),
        "by the effect's invoker, not the person at the device"
    );
    assert!(
        app.actions.acting_for.is_empty(),
        "the answered invocation runs for nobody"
    );

    // An effect a viewer may take lends them no write: its edit is refused.
    app.invoke_published_action(
        8,
        tilo_actions::InvocationId::new(),
        "test.counter".into(),
        "peek".into(),
        None,
        "agent:Viewer",
        None,
    );
    assert_eq!(counter(&app), 1, "the viewer's effect wrote nothing");
    assert!(app.actions.acting_for.is_empty());

    // An edit the app submits on its own is the local person's.
    let local = app.collaboration.local_author.0.clone();
    app.invoke_resource_action(ResourceInvocation::new(
        &tilo_actions::ActionAddress::new("test.counter", "bump"),
        &local,
    ))
    .expect("the local person bumps");
    assert_eq!(counter(&app), 2);
    assert_eq!(
        app.state
            .peek_last_undo()
            .and_then(|entry| entry.author.as_ref()),
        Some(&crate::permissions::UserId(local))
    );
}

#[test]
fn with_a_broker_an_effects_edit_is_admitted_there_for_its_invoker() {
    let terminal = |sent: &[CollabRequest], invocation: tilo_actions::InvocationId| {
        sent.iter().find_map(|request| match request {
            CollabRequest::UpdateInvocation { update, .. }
                if update.is_terminal() && update.invocation() == invocation =>
            {
                Some(update.clone())
            }
            _ => None,
        })
    };
    for refused in [true, false] {
        let mut card = CounterCard::new();
        card.bump_after_ping = true;
        let mut app = App::new(State::new(), Box::new(card));
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        let sent = Rc::new(RefCell::new(vec![]));
        app.collaboration.manager =
            CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
        let invocation = tilo_actions::InvocationId::new();
        app.invoke_published_action(
            9,
            invocation,
            "test.counter".into(),
            "ping".into(),
            None,
            "agent:Scribe",
            None,
        );
        assert_eq!(
            counter(&app),
            0,
            "nothing is applied before the broker admits it"
        );
        let follow_up = sent
            .borrow()
            .iter()
            .find_map(|request| match request {
                CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
                } => Some(invoke.clone()),
                _ => None,
            })
            .expect("the edit goes to the broker");
        assert_eq!(follow_up.on_behalf_of, Some(invocation));
        assert_eq!(follow_up.target.address.action, "bump");
        assert!(
            terminal(&sent.borrow(), invocation).is_none(),
            "the effect answers once its edit is admitted"
        );

        let request_id = *app.actions.follow_ups.keys().next().unwrap();
        let receipt = if refused {
            Err("NotPermitted".to_owned())
        } else {
            Ok((
                follow_up.invocation,
                tilo_actions::InvocationUpdate::Accepted {
                    invocation: follow_up.invocation,
                },
            ))
        };
        assert!(app.settle_follow_up(request_id, &receipt));
        match terminal(&sent.borrow(), invocation) {
            Some(tilo_actions::InvocationUpdate::Failed { error, .. }) if refused => {
                assert_eq!(error.code, "permission_denied");
            }
            Some(tilo_actions::InvocationUpdate::Succeeded { .. }) if !refused => {}
            other => panic!("unexpected answer: {other:?}"),
        }
        assert!(app.actions.acting_for.is_empty());
    }
}

#[test]
fn broker_invocation_uuid_collision_preserves_original_operation_route() {
    for conflicting_action in ["bump", "async-bump"] {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_async_post_bump_reset()),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        app.collaboration.manager = admitted_delivery_manager();
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(
            0xa5a5_a5a5_a5a5_a5a5_0000_0000_0000_002b,
        ));
        app.invoke_published_action(
            43,
            invocation,
            "test.counter".into(),
            "async-bump".into(),
            None,
            "device:first",
            None,
        );
        // A conflicting caller must never replace the active operation's
        // cancellation/completion route or run a second effect.
        app.invoke_published_action(
            44,
            invocation,
            "test.counter".into(),
            conflicting_action.into(),
            None,
            "device:second",
            None,
        );
        assert_eq!(
            counter(&app),
            0,
            "conflicting invocation was rejected before its effect"
        );
        assert_eq!(
            app.actions
                .directed_invocations
                .get(&invocation)
                .unwrap()
                .wire_id,
            43
        );
        app.request_invocation_cancellation(44);
        assert!(
            !app.actions
                .directed_invocations
                .get(&invocation)
                .unwrap()
                .cancellation_requested
        );
        app.request_invocation_cancellation(43);
        assert!(
            app.actions
                .directed_invocations
                .get(&invocation)
                .unwrap()
                .cancellation_requested
        );
        app.refresh_focus_order();
        assert_eq!(counter(&app), 1, "only the first operation ran");
        assert!(app.poll_directed_invocations());
        assert!(app.actions.directed_invocations.is_empty());
    }
}

include!("action_directory_snapshot_tests.rs");

fn directory_query(
    revision: impl Into<String>,
    parent_resource: impl Into<String>,
    max_results: u32,
    max_bytes: u32,
    max_work: u32,
) -> runtime_collab::ActionDirectoryQuery {
    runtime_collab::ActionDirectoryQuery {
        peer_id: 1,
        revision: revision.into(),
        cursor: Vec::new(),
        needle: String::new(),
        parent_resource: parent_resource.into(),
        max_results,
        max_bytes,
        max_work,
        max_depth: 8,
    }
}

#[test]
fn action_directory_pages_caps_and_cursors_fail_closed() {
    // Case `directory_pages_parent_actions_and_refuses_stale_resolution`.
    {
        let mut app = app();
        let root = app.action_directory_page(directory_query("", "", 8, 8_192, 8));
        assert_eq!(root.rows.len(), 1);
        assert_eq!(root.rows[0].resource, "test.counter");
        assert!(root.rows[0].action.is_none());
        assert!(root.rows[0].has_children);

        let actions =
            app.action_directory_page(directory_query(root.revision, "test.counter", 8, 8_192, 8));
        assert!(actions.rows.len() >= 5, "parent exposes its action rows");
        assert!(actions.rows.iter().all(|row| row.action.is_some()));
        assert!(matches!(
            app.resolve_action_directory("stale", "test.counter", "inspect"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
    }

    // Case `broker_request_ids_use_the_final_value_then_fail_closed`.
    {
        let mut app = app();
        app.actions.next_broker_request_id = Some(u64::MAX);
        assert_eq!(app.take_broker_request_id().unwrap(), u64::MAX);
        assert!(matches!(
            app.take_broker_request_id(),
            Err(tilo_actions::DoorError::Unavailable { code, .. })
                if code == "broker_request_identity_exhausted"
        ));
    }

    // Case `action_directory_cursor_keys_do_not_reuse_a_process_counter`.
    {
        let mut app = app();
        let first = app.action_directory_cursor_key();
        let second = app.action_directory_cursor_key();
        assert_ne!(first, second);
        assert!(first.starts_with("tilo-dir-"));
        assert!(second.starts_with("tilo-dir-"));
    }

    // Case `directory_hard_caps_never_materialize_an_oversized_row`.
    {
        let mut app = app();
        let page = app.action_directory_page(directory_query("", "", 1, 1, 1));
        assert!(
            page.rows.is_empty(),
            "one byte cannot contain a resource row"
        );
        assert!(page.budget_exhausted);
        assert!(!page.exhausted);
    }

    // Case `directory_pages_the_129th_widget_root_without_hiding_it`.
    {
        let mut app = App::new(
            State::new(),
            Box::new(CounterCard::with_directory_roots(
                ACTION_DIRECTORY_MAX_RESULTS + 1,
            )),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        let first = app.action_directory_page(directory_query(
            "",
            "",
            u32::try_from(ACTION_DIRECTORY_MAX_RESULTS).unwrap_or(u32::MAX),
            64 * 1024,
            u32::try_from(ACTION_DIRECTORY_MAX_WORK).unwrap_or(u32::MAX),
        ));
        assert_eq!(first.rows.len(), ACTION_DIRECTORY_MAX_RESULTS);
        assert!(!first.exhausted);
        let tail = app.action_directory_page(runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: first.revision.clone(),
            cursor: first.next_cursor,
            needle: String::new(),
            parent_resource: String::new(),
            max_results: 8,
            max_bytes: 8_192,
            max_work: 32,
            max_depth: 8,
        });
        assert_eq!(tail.rows.len(), 1);
        assert_eq!(tail.rows[0].resource, "test.counter:128");
        assert!(tail.exhausted);
    }
}

#[test]
fn directory_finds_a_100k_tail_without_scanning_the_corpus() {
    let provider = LargeDirectoryProvider::new(100_000);
    let calls = std::sync::Arc::clone(&provider.calls);
    let largest_budget = std::sync::Arc::clone(&provider.largest_budget);
    let corpus = provider.catalog.clone();
    let mut app = App::new(
        State::new(),
        Box::new(CounterCard::with_directory_provider(provider)),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    let page = app.action_directory_page(runtime_collab::ActionDirectoryQuery {
        peer_id: 1,
        revision: String::new(),
        cursor: Vec::new(),
        needle: "needle-99999".into(),
        parent_resource: String::new(),
        max_results: 8,
        max_bytes: 8_192,
        max_work: 8,
        max_depth: 8,
    });
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].resource, "test.item:99999");
    // One bounded call finds the only record holding the needle, and the
    // index knows nothing else matches.
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(page.exhausted);
    assert!(
        largest_budget.load(std::sync::atomic::Ordering::Relaxed) <= 8,
        "the adapter passed the caller's bounded work budget to the retained index"
    );
    // Freeing a 100k-row index costs seconds in a debug build and proves
    // nothing here: the app lets go of its handle, and the last one is
    // released off the test thread.
    drop(app);
    std::thread::spawn(move || drop(corpus));
}

#[test]
fn directory_cursor_saturation_evicts_old_cursors_as_stale() {
    let mut app = app();
    let revision = app.action_directory_revision();
    let first = app.action_directory_next_cursor(ActionDirectoryCursor {
        revision: revision.clone(),
        needle: String::new(),
        parent_resource: String::new(),
        provider_index: 0,
        provider_cursor: None,
        browse_offset: 0,
        root_widget_index: 0,
        root_provider_cursor: None,
    });
    for offset in 1..=ACTION_DIRECTORY_MAX_CURSORS {
        let _ = app.action_directory_next_cursor(ActionDirectoryCursor {
            revision: revision.clone(),
            needle: String::new(),
            parent_resource: String::new(),
            provider_index: 0,
            provider_cursor: None,
            browse_offset: offset,
            root_widget_index: 0,
            root_provider_cursor: None,
        });
    }
    assert!(app.actions.action_directory_cursors.len() <= ACTION_DIRECTORY_MAX_CURSORS);
    let page = app.action_directory_page(runtime_collab::ActionDirectoryQuery {
        peer_id: 1,
        revision,
        cursor: first,
        needle: String::new(),
        parent_resource: String::new(),
        max_results: 1,
        max_bytes: 1_024,
        max_work: 1,
        max_depth: 1,
    });
    assert!(page.stale_cursor);
}

#[test]
fn a_needle_finds_actions_published_only_in_the_resource_tree() {
    let mut app = app();
    let mut query = directory_query("", "", 8, 65_536, 64);
    query.needle = "ping".into();
    let page = app.action_directory_page(query);
    assert!(page.exhausted, "{page:?}");
    let found = page
        .rows
        .iter()
        .map(|row| (row.resource.as_str(), row.action.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(found, [("test.counter", Some("ping"))]);

    let mut query = directory_query("", "", 1, 65_536, 64);
    query.needle = "BUMP".into();
    let first = app.action_directory_page(query.clone());
    assert_eq!(first.rows.len(), 1);
    assert!(!first.exhausted);
    query.revision = first.revision.clone();
    query.cursor = first.next_cursor.clone();
    let second = app.action_directory_page(query);
    assert_eq!(second.rows.len(), 1);
    assert_ne!(
        first.rows[0].action, second.rows[0].action,
        "the cursor resumes after the returned match"
    );
}

#[test]
fn a_needle_finds_an_action_by_its_parameters() {
    let mut app = app();
    let mut query = directory_query("", "", 8, 65_536, 64);
    query.needle = "value".into();
    let page = app.action_directory_page(query);
    let found = page
        .rows
        .iter()
        .map(|row| (row.resource.as_str(), row.action.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(found, [("test.counter", Some("set-label"))], "{page:?}");
}

#[cfg(all(target_os = "linux", feature = "native"))]
#[path = "native_pty_wake_tests.rs"]
mod native_pty_wake_tests;
