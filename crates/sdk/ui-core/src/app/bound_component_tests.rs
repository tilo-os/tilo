use super::*;
use crate::application::{
    AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use crate::widget::{AppKind, AppWidget, Column, IconDescriptor, TileHost};
use crate::widget::{AppModel, ModelPath};
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::cell::RefCell;
use std::rc::Rc;
thread_local! { static CAPTURES: RefCell<Vec<(&'static str, crate::LatestRequest<u32>)>> = const { RefCell::new(vec![]) }; }
thread_local! { static DISCOVERY_COUNT: Cell<usize> = const { Cell::new(0) }; }
thread_local! { static CONTROL_AUTHORITY: RefCell<Vec<(bool, bool)>> = const { RefCell::new(vec![]) }; }
thread_local! { static COMPOUND_MODE: Cell<u8> = const { Cell::new(0) }; }
thread_local! { static DOCUMENT_JOB_MODE: Cell<bool> = const { Cell::new(false) }; }
thread_local! {
    static PAGED_SEARCH_COUNTS: RefCell<Option<Rc<[Cell<usize>; 2]>>> = const { RefCell::new(None) };
}

thread_local! {
    static COLLABORATION_OBSERVATIONS: RefCell<Option<(Rc<Cell<usize>>, Rc<RefCell<Vec<crate::WakeHandle>>>)>> = const { RefCell::new(None) };
}

fn collaboration_probe_factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|_| Ok(()))
        .map_err(crate::launch::LaunchError::Record)?;
    let probe = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "bound-collaboration",
        "fixture.bound-collaboration",
    );
    COLLABORATION_OBSERVATIONS.with_borrow_mut(|slot| {
        *slot = Some((
            Rc::clone(&probe.collab_events),
            Rc::clone(&probe.collab_wake_handles),
        ));
    });
    Ok(AppComponent::new(
        context,
        probe.root,
        tilo_actions::ActionRegistry::new(),
    ))
}
struct PagedProbe {
    catalog: crate::search::SearchCatalog,
    resolutions: Rc<[Cell<usize>; 2]>,
    owner: usize,
}
impl PagedProbe {
    fn new(strict: bool, resolutions: Rc<[Cell<usize>; 2]>) -> Self {
        let catalog = crate::search::SearchCatalog::new("Bound paging probe");
        for id in 0..65 {
            let resource = format!("bound:paged:{id}");
            catalog
                .upsert(
                    &resource,
                    "paged",
                    crate::search::SearchResult::for_action(
                        format!("Paged kind {id}"),
                        format!("{} paged {id}", if strict { "Allowed" } else { "Revoked" }),
                        tilo_actions::ActionAddress::new(&resource, "apply"),
                    ),
                )
                .unwrap();
        }
        Self {
            catalog,
            resolutions,
            owner: usize::from(strict),
        }
    }
}
impl crate::search::SearchProvider for PagedProbe {
    fn name(&self) -> &'static str {
        "Bound paging probe"
    }
    fn search(&self, _: &str) -> Vec<crate::search::SearchResult> {
        panic!("the controlled paging fixture must use search_page")
    }
    fn search_page(
        &self,
        query: &crate::search::SearchQuery,
        cursor: Option<&crate::search::SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> crate::search::SearchPage {
        self.catalog
            .query_page(query.text(), cursor, work_budget, result_limit)
    }
    // Deliberately inherit unknown generation: validation cannot cache a
    // claim that this provider's authorization declarations are stable.
    fn resolve_action_resource(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::Resource> {
        let counter = &self.resolutions[self.owner];
        counter.set(counter.get().saturating_add(1));
        address.resource.strip_prefix("bound:paged:")?;
        (address.action == "apply").then(|| {
            tilo_actions::Resource::new(&address.resource, "probe", "Paged").with_action(
                tilo_actions::ActionSpec::immediate("apply", "Apply", "bound.apply", &FollowUp)
                    .unwrap(),
            )
        })
    }
}
#[derive(Default)]
struct Probe {
    strict: bool,
    collision: Option<String>,
    actions: crate::action_host::ActionHost,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FollowUp;
impl tilo_actions::Action for FollowUp {
    fn apply(&self, doc: &mut automerge::AutoCommit) -> Result<(), tilo_actions::ActionError> {
        doc.put(ROOT, "follow-up", true)?;
        Ok(())
    }
}
#[derive(Debug, serde::Serialize)]
struct EmptyFollowUp {
    refuse: bool,
}
impl tilo_actions::Action for EmptyFollowUp {
    fn apply(&self, doc: &mut automerge::AutoCommit) -> Result<(), tilo_actions::ActionError> {
        if self.refuse {
            doc.put(ROOT, "failed-action", true)?;
            Err(tilo_actions::ActionError::Refused(
                "controlled failure".into(),
            ))
        } else {
            Ok(())
        }
    }
}
impl Probe {
    fn capture(label: &'static str) {
        CAPTURES.with_borrow_mut(|v| v.push((label, crate::LatestRequest::new())));
    }
}

struct LazyProbe {
    strict: bool,
}
impl crate::search::SearchProvider for LazyProbe {
    fn name(&self) -> &'static str {
        "Probe lazy"
    }
    fn action_directory_generation(&self) -> Option<u64> {
        Some(0)
    }
    fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
        match query {
            "sealed" => vec![crate::search::SearchResult::for_action(
                "Probe",
                if self.strict {
                    "Allowed sealed sibling title"
                } else {
                    "Hidden sealed write title"
                },
                tilo_actions::ActionAddress::new("bound:sealed", "apply"),
            )],
            "sealedopaque" => vec![crate::search::SearchResult::new(
                "Probe",
                if self.strict {
                    "Allowed sealedopaque title"
                } else {
                    "Hidden sealedopaque title"
                },
                "opaque",
            )],
            "lazy" => vec![crate::search::SearchResult::for_action(
                "Probe",
                "Lazy",
                tilo_actions::ActionAddress::new("bound:lazy", "view"),
            )],
            "opaque" => vec![crate::search::SearchResult::new(
                "Probe", "Opaque", "opaque",
            )],
            _ => vec![],
        }
    }
    fn resolve_action_resource(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::Resource> {
        if address.resource == "bound:sealed" && address.action == "apply" {
            return Some(
                tilo_actions::Resource::new("bound:sealed", "probe", "Sealed").with_action(
                    tilo_actions::ActionSpec::immediate("apply", "Apply", "bound.apply", &FollowUp)
                        .unwrap(),
                ),
            );
        }
        (address.resource == "bound:lazy" && address.action == "view").then(|| {
            tilo_actions::Resource::new("bound:lazy", "probe", "Lazy").with_action(
                tilo_actions::ActionSpec::operation(
                    "view",
                    "View",
                    tilo_actions::OperationKind::View,
                    "bound.view",
                    &(),
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::view()),
            )
        })
    }
}
impl Widget for Probe {
    crate::provides!(
        dyn crate::undo::CommittedActions,
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["bound.follow-up", "bound.view"]
    }
    #[allow(
        clippy::panic_in_result_fn,
        reason = "probe asserts canonical admission before returning the toolkit result"
    )]
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(crate::host_operation::OperationRequest { type_id, .. }) = op.unscoped() else {
            return Ok(None);
        };
        if type_id == "bound.view" {
            if COMPOUND_MODE.get() == 1 {
                self.actions
                    .dispatch(doc.document_mut(), "bound.apply", &FollowUp);
            }
            Self::capture("bound-view");
            return Ok(Some(crate::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        assert_eq!(type_id, "bound.follow-up");
        assert_eq!(path, &ROOT);
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some("child")
        );
        let mode = COMPOUND_MODE.get();
        if matches!(mode, 3 | 4 | 6) {
            doc.document_mut().put(ROOT, "raw-before", true).unwrap();
        }
        match mode {
            2 | 3 => {}
            7 => self.actions.dispatch_with_context(
                doc.document_mut(),
                "bound.apply",
                &FollowUp,
                tilo_actions::ActionContext::for_resource("bound:probe", "probe", "Probe"),
            ),
            8 => {
                let mut queue = tilo_actions::OptimisticQueue::new(doc.document_mut());
                self.actions
                    .dispatch_optimistic(doc.document_mut(), &mut queue, "bound.apply", &FollowUp)
                    .unwrap();
            }
            9 | 10 => {
                assert_eq!(
                    self.actions
                        .try_dispatch(
                            doc.document_mut(),
                            "bound.empty",
                            &EmptyFollowUp { refuse: mode == 10 }
                        )
                        .is_err(),
                    mode == 10
                );
            }
            11 => {
                doc.document_mut().put(ROOT, "raw-unwind", true).unwrap();
                panic!("controlled compound unwind");
            }
            _ => self
                .actions
                .dispatch(doc.document_mut(), "bound.apply", &FollowUp),
        }
        if matches!(mode, 5 | 6) {
            doc.document_mut().put(ROOT, "raw-after", true).unwrap();
        }
        Self::capture("mutable-operation");
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
    }
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if !cancellation_requested {
            return None;
        }
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some("child")
        );
        if !PUBLICATION_NORMAL_POLL.get() {
            assert!(!crate::local_state::LocalState::new(0).writer().try_set(1));
        }
        Self::capture("mutable-cancel");
        Some(tilo_actions::InvocationUpdate::Cancelled {
            invocation,
            message: None,
        })
    }

    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        let (doc, _) = crate::doc::document_ref(doc, path);
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some("child")
        );
        Self::capture("sync");
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        Self::capture("build");
        let node = tree.new_leaf(taffy::Style::default()).unwrap();
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn measure(
        &mut self,
        _: taffy::Size<Option<f32>>,
        _: taffy::Size<taffy::AvailableSpace>,
        _: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        Self::capture("measure");
        taffy::Size {
            width: 30.0,
            height: 20.0,
        }
    }
    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {
        Self::capture("apply");
    }
    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
        Self::capture("draw");
    }
    fn handle_event(&mut self, _: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        assert_eq!(
            ctx.state.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some("child")
        );
        Self::capture("event");
    }
    fn name(&self) -> &'static str {
        "bound-probe"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
    fn is_focusable(&self) -> bool {
        Self::capture("focus-order");
        true
    }
    fn set_focus(&mut self, _: bool) {
        Self::capture("focus");
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Self::capture("accessibility");
        Some(crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name("Child"))
    }
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Self::capture("ime");
        Some(winit::window::ImePurpose::Normal)
    }
    fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
        Self::capture("ime-rect");
        Some(vello::kurbo::Rect::ZERO)
    }
    fn perform_action(
        &mut self,
        _: &crate::a11y::Action,
        _key_path: &[u64],
        doc: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let (doc, _) = crate::doc::document(doc, path);
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some("child")
        );
        doc.document_mut().put(ROOT, "action", true).unwrap();
        Self::capture("action");
        true
    }
}

impl crate::search::capability::SearchSource for Probe {
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        if let Some(counts) = PAGED_SEARCH_COUNTS.with_borrow(Clone::clone) {
            return vec![Box::new(PagedProbe::new(self.strict, counts))];
        }
        vec![Box::new(LazyProbe {
            strict: self.strict,
        })]
    }
    fn activate_search_result(
        &mut self,
        id: &str,
        doc: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        let (doc, _) = crate::doc::document(doc, path);
        assert_eq!(id, "opaque");
        doc.document_mut().put(ROOT, "navigation", true).unwrap();
    }
}

impl crate::resources::PublishesResources for Probe {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        DISCOVERY_COUNT.set(DISCOVERY_COUNT.get().saturating_add(1));
        let mut action = tilo_actions::ActionSpec::operation(
            "follow-up",
            "Follow up",
            tilo_actions::OperationKind::Effect,
            "bound.follow-up",
            &(),
        )
        .unwrap();
        action.contract.execution = tilo_actions::ExecutionMode::Background { cancellable: true };
        action.contract.result = tilo_actions::ResultSpec::Job;
        if self.strict {
            action.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
        }
        let mut document = tilo_actions::ActionSpec::immediate(
            "apply",
            "Apply follow up",
            "bound.apply",
            &FollowUp,
        )
        .unwrap();
        if DOCUMENT_JOB_MODE.get() {
            document.contract.execution =
                tilo_actions::ExecutionMode::Background { cancellable: true };
            document.contract.result = tilo_actions::ResultSpec::Job;
        }
        vec![
            tilo_actions::Resource::new(
                self.collision.as_deref().unwrap_or("bound:probe"),
                "probe",
                "Probe",
            )
            .with_action(action)
            .with_action(document)
            .with_action(
                tilo_actions::ActionSpec::operation(
                    "view",
                    "View",
                    tilo_actions::OperationKind::View,
                    "bound.view",
                    &(),
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::view()),
            ),
        ]
    }
    fn action_directory_resource_page(
        &self,
        doc: &automerge::AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<crate::widget::ActionDirectoryResourcePage> {
        Some(crate::widget::ActionDirectoryResourcePage {
            resources: if cursor.is_empty() && max_results > 0 {
                self.resources(doc)
            } else {
                vec![]
            },
            next_cursor: vec![],
            exhausted: true,
        })
    }
    fn resource(&self, doc: &automerge::AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        self.resources(doc)
            .into_iter()
            .find(|resource| resource.id.as_str() == id)
    }
    fn action_directory_visibility(
        &self,
        _: &automerge::AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        (self.strict && id == "bound:probe")
            .then(|| tilo_actions::ActionAddress::new(id, "follow-up"))
    }
}

impl crate::widget::controls::ActionControls for Probe {
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        CONTROL_AUTHORITY.with_borrow_mut(|seen| {
            seen.push((
                self.strict,
                allowed.contains(&tilo_actions::ActionAddress::new("bound:probe", "apply")),
            ));
        });
    }
}

impl crate::undo::CommittedActions for Probe {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}
impl AppWidget for Probe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Probe"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("P")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}
#[allow(clippy::needless_pass_by_value, reason = "factory contract")]
fn factory(
    context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    build_probe(context, &target, false)
}
#[allow(clippy::needless_pass_by_value, reason = "factory contract")]
fn strict_factory(
    context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    build_probe(context, &target, true)
}
fn build_probe(
    mut context: HostContext,
    target: &LaunchTarget,
    strict: bool,
) -> Result<AppComponent, crate::launch::LaunchError> {
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|doc| {
                doc.put(ROOT, "marker", "child").map_err(|e| e.to_string())
            })
            .map_err(crate::launch::LaunchError::Record)?;
    }
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<FollowUp>("bound.apply");
    Ok(AppComponent::new(
        context,
        Box::new(Probe {
            strict,
            collision: None,
            actions: crate::action_host::ActionHost::new(),
        }),
        registry,
    ))
}
#[allow(
    clippy::too_many_lines,
    reason = "one actual App lifetime verifies descendant delivery, mutable admission and closed cancellation"
)]
#[test]
fn bound_components_keep_child_binding_and_focused_document_history() {
    // Case `real_app_descendant_focus_ime_semantics_and_measure_keep_child_binding`.
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut owners = crate::record::RecordOwners::new(root.clone());
        let records = owners.open("so.tilo.binding-probe", "A").unwrap();
        let views =
            Rc::new(crate::application_view::InstanceViewStore::new(root, "D", "A").unwrap());
        let definition = ApplicationDefinition {
            app_id: "so.tilo.binding-probe",
            title: "Probe",
            icon: IconDescriptor::Glyph("P"),
            record_kind: crate::record::RecordKind::new("probe"),

            required: vec![],
            optional: vec![],
            opens: &[],
            room_record: false,
            create: factory,
        };
        let context = HostContext::prepare(
            &definition,
            HostContextInputs {
                records: records.clone(),
                views,
                device_id: "D".into(),
                instance_id: 1,
                permissions: Rc::new(RefCell::new(
                    crate::permissions::PermissionContext::permissive(),
                )),
                author: crate::permissions::UserId("probe-author".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            &LaunchTarget::New,
        )
        .unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let checkout = component.record().clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        component
            .mount(move |event| {
                sender.send(event).unwrap();
            })
            .unwrap();
        let mut outer = crate::wake::WakeScope::new();
        let (tx, rx) = std::sync::mpsc::channel();
        outer
            .bind(move |event| {
                tx.send(event).unwrap();
            })
            .unwrap();
        let mut state = State::new();
        state.doc.put(ROOT, "marker", "shell").unwrap();
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            outer,
            state,
            Box::new(
                Column::new().push(Box::new(
                    crate::widget::Row::new().push(Box::new(
                        crate::widget::Container::new()
                            .with_child(Box::new(TileHost::from_component(component))),
                    )),
                )),
            ),
        );
        CAPTURES.with_borrow_mut(Vec::clear);
        app.init_layout();
        app.refresh_focus_order();
        let leaf = app.focusable_order[0];
        app.set_focused_node(Some(leaf));
        let _ = app.ime_window_state();
        app.dispatch_a11y_action(
            accesskit::NodeId(u64::from(leaf).saturating_add(1)),
            &crate::a11y::Action::Focus,
        );
        let layout_root = app.layout.root_node.unwrap();
        app.layout.layout_dirty = true;
        run_layout_pass(&mut app, layout_root, 400, 300, 1.0, true, None).unwrap();
        assert!(
            tilo_actions::door::resolve(
                &app.collect_local_resources(),
                &tilo_actions::ActionAddress::new("bound:probe", "follow-up")
            )
            .is_some(),
            "fixture must publish its operation before admission"
        );
        let actor = app.collaboration.local_author.0.clone();
        assert!(matches!(
            app.invoke_resource_action(
                ResourceInvocation::new(
                    &tilo_actions::ActionAddress::new("bound:probe", "follow-up"),
                    &actor,
                )
                .invocation(Some(tilo_actions::InvocationId::new())),
            )
            .unwrap(),
            crate::widget::ResourceOperationStart::Accepted
        ));
        assert!(
            records
                .read(&checkout.id, |doc| doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_bool()
                    .unwrap())
                .unwrap()
        );
        assert!(app.state.doc.get(ROOT, "follow-up").unwrap().is_none());
        let captures = CAPTURES.with_borrow_mut(std::mem::take);
        assert_eq!(
            captures
                .iter()
                .filter(|(label, _)| *label == "mutable-operation")
                .count(),
            1
        );
        for label in [
            "build",
            "sync",
            "measure",
            "apply",
            "focus-order",
            "focus",
            "ime",
            "accessibility",
            "action",
        ] {
            assert!(
                captures.iter().any(|(actual, _)| *actual == label),
                "missing {label}"
            );
        }
        for (label, mut request) in captures {
            assert!(request.begin().complete(1));
            assert!(receiver.try_recv().is_ok(), "{label} missed child");
            assert!(rx.try_recv().is_err(), "{label} woke outer");
        }
        let close_probe = *app.actions.bound_invocations.keys().next().unwrap();
        app.actions.local_invocations.insert(
            close_probe,
            (
                tilo_actions::ActionAddress::new("bound:probe", "follow-up"),
                tilo_actions::OperationKind::Effect,
            ),
        );
        app.close_admission.save_waiting = true;
        app.close_admission.save_local = Some(close_probe);
        app.close_components();
        assert_eq!(
            CAPTURES.with_borrow(|c| c
                .iter()
                .filter(|(label, _)| *label == "mutable-cancel")
                .count()),
            1
        );
        assert_eq!(app.component_close_reports().len(), 1);
        app.close_components();
        assert_eq!(app.component_close_reports().len(), 1);
        assert_eq!(app.take_component_close_reports().len(), 1);
        assert!(app.take_component_close_reports().is_empty());
    }

    // Case `document_history_keeps_focused_record_after_focus_moves_and_revokes_live`.
    {
        let mut fixture = ActionFixture::new();
        let mut entry_ids = Vec::new();
        for (index, binding) in fixture.bindings.iter().enumerate() {
            let hash = fixture
                .records
                .with(&binding.checkout.id, |doc| {
                    doc.put(ROOT, "history_marker", format!("record-{index}"))
                        .unwrap();
                    doc.commit().unwrap()
                })
                .unwrap();
            entry_ids.push(crate::history_source::entry_id_from_hash(&hash.to_string()));
        }
        fixture
            .app
            .state
            .doc
            .put(ROOT, "history_marker", "outer")
            .unwrap();
        let pending = fixture.app.state.doc.pending_ops();
        let focus_for = |app: &App, instance| {
            app.layout
                .node_map
                .keys()
                .copied()
                .find(|node| {
                    app.bindings
                        .binding_for(*node)
                        .is_some_and(|binding| binding.instance_id == instance)
                })
                .unwrap()
        };
        fixture.app.focused_node = Some(focus_for(&fixture.app, fixture.bindings[0].instance_id));
        fixture.app.open_document_history().unwrap();
        fixture.app.focused_node = Some(focus_for(&fixture.app, fixture.bindings[1].instance_id));
        fixture
            .app
            .select_document_history(&[entry_ids[0]])
            .unwrap();
        let snapshot = fixture.app.document_history_snapshot().unwrap().unwrap();
        let after =
            automerge::AutoCommit::load(&snapshot.comparison.unwrap().after_document).unwrap();
        assert_eq!(
            after
                .get(ROOT, "history_marker")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("record-0")
        );
        assert!(
            fixture
                .app
                .select_document_history(&[entry_ids[1]])
                .is_err()
        );
        assert_eq!(fixture.app.state.doc.pending_ops(), pending);
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::stranger("fixture-user").at_unix_time(100);
        assert!(fixture.app.document_history_snapshot().is_err());
        assert!(!fixture.app.document_history.is_open());
    }

    // Case `document_history_does_not_fall_back_when_component_closes`.
    {
        let mut fixture = ActionFixture::new();
        let binding = &fixture.bindings[0];
        fixture.app.focused_node = fixture.app.layout.node_map.keys().copied().find(|node| {
            fixture
                .app
                .bindings
                .binding_for(*node)
                .is_some_and(|candidate| candidate.instance_id == binding.instance_id)
        });
        assert!(fixture.app.focused_node.is_some());
        fixture.app.open_document_history().unwrap();
        binding.closing.set(true);
        fixture.app.focused_node = None;
        assert!(fixture.app.document_history_snapshot().is_err());
        assert!(!fixture.app.document_history.is_open());
    }

    // Case `document_history_requires_a_choice_when_multiple_components_have_no_focus`.
    {
        let mut fixture = ActionFixture::new();
        fixture.app.focused_node = None;
        assert!(fixture.app.open_document_history().is_err());
        assert!(fixture.app.document_history_snapshot().unwrap().is_none());
    }
}
struct ActionFixture {
    _temp: tempfile::TempDir,
    app: App,
    records: crate::record::RecordStore,
    bindings: Vec<crate::bound_dispatch::CallbackBinding>,
    permissions: Rc<RefCell<crate::permissions::PermissionContext>>,
}

impl ActionFixture {
    fn new() -> Self {
        Self::with_same_record(false)
    }
    fn with_same_record(shared: bool) -> Self {
        Self::with_authority(shared, false)
    }
    fn with_authority(shared: bool, separate: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().into());
        let records = owners
            .open(
                "so.tilo.binding-probe",
                "00000000-0000-0000-0000-000000000004",
            )
            .unwrap();
        let views = Rc::new(
            crate::application_view::InstanceViewStore::new(
                temp.path().into(),
                "D",
                "00000000-0000-0000-0000-000000000004",
            )
            .unwrap(),
        );
        let permissions = Rc::new(RefCell::new(
            crate::permissions::PermissionContext::granted(
                "fixture-user",
                &crate::permissions::TreePath::root(),
                crate::permissions::policy::KindSet::ALL,
                Some(110),
            )
            .at_unix_time(100),
        ));
        let mut column = Column::new();
        let mut bindings: Vec<crate::bound_dispatch::CallbackBinding> = Vec::new();
        for (instance, create) in [
            (11, factory as crate::application::ComponentFactory),
            (12, strict_factory),
        ] {
            let definition = ApplicationDefinition {
                app_id: "so.tilo.binding-probe",
                title: "Probe",
                icon: IconDescriptor::Glyph("P"),
                record_kind: crate::record::RecordKind::new("probe"),

                required: vec![],
                optional: vec![],
                opens: &[],
                room_record: false,
                create,
            };
            let target = if shared && !bindings.is_empty() {
                LaunchTarget::Record {
                    checkout: bindings[0].checkout.clone(),
                    reveal: None,
                }
            } else {
                LaunchTarget::New
            };
            let context = HostContext::prepare(
                &definition,
                HostContextInputs {
                    records: records.clone(),
                    views: views.clone(),
                    device_id: "D".into(),
                    instance_id: instance,
                    permissions: if separate && instance == 12 {
                        Rc::new(RefCell::new(permissions.borrow().clone()))
                    } else {
                        permissions.clone()
                    },
                    author: crate::permissions::UserId("fixture-user".into()),
                    action_context: tilo_actions::ActionContext::default(),
                },
                &target,
            )
            .unwrap();
            let component = definition
                .instantiate(context, target, RestoreInput::None)
                .unwrap();
            bindings.push(component.callback_binding());
            column = column.push(Box::new(TileHost::from_component(component)));
        }
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(column));
        app.init_layout();
        Self {
            _temp: temp,
            app,
            records,
            bindings,
            permissions,
        }
    }
    fn broker() -> Self {
        let mut fixture = Self::with_same_record(true);
        // This fixture captures broker submission and injects admitted delivery;
        // it does not exercise authenticated native document history.
        fixture.app.collaboration.manager =
            super::semantic_tree_app_tests::admitted_delivery_manager();
        fixture.app.actions.bound_broker_route_for_test =
            Some(crate::collaboration::PrimaryRouteWitness {
                route: fixture.bindings[0].broker_route().unwrap(),
                epoch: uuid::Uuid::new_v4(),
            });
        fixture
    }
    fn target(&self, index: usize, action: &str) -> crate::bound_dispatch::BoundActionTarget {
        let binding = self.bindings[index].clone();
        crate::bound_dispatch::BoundActionTarget {
            input: crate::bound_dispatch::Input::Discrete,
            address: crate::bound_dispatch::BoundActionAddress {
                instance_id: binding.instance_id,
                address: tilo_actions::ActionAddress::new("bound:probe", action),
            },
            binding,
        }
    }
}

include!("directory_permission_tests.rs");

include!("invocation_locus_tests.rs");

include!("owner_publication_tests.rs");

include!("action_directory_snapshot_bound_tests.rs");
include!("bound_record_sync_tests.rs");
include!("standalone_history_record_restore_tests.rs");
include!("shared_forest_tests.rs");

#[test]
fn background_document_job_is_broker_admitted_before_its_canonical_edit() {
    DOCUMENT_JOB_MODE.set(true);
    let mut fixture = ActionFixture::broker();
    let target = fixture.target(0, "apply");
    let actor = fixture.app.collaboration.local_author.0.clone();
    let before = fixture
        .records
        .read(&target.binding.checkout.id, |doc| doc.clone().get_heads())
        .unwrap();
    let mut requests = Vec::new();
    let started = fixture
        .app
        .invoke_bound_target_with_broker(
            &target,
            &ResourceInvocation::new(&target.address.address, &actor).confirmed(true),
            Some(&mut |request| requests.push(request)),
        )
        .unwrap();
    assert!(matches!(
        started,
        crate::widget::ResourceOperationStart::Accepted
    ));
    assert_eq!(
        requests.len(),
        1,
        "document work needs an actual native source invocation"
    );
    assert_eq!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc.clone().get_heads())
            .unwrap(),
        before,
        "no canonical edit or outside consequence before broker admission"
    );
    let CollabRequest::RemoteAction {
        message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
    } = requests.pop().unwrap()
    else {
        panic!("expected canonical broker request")
    };
    assert_eq!(request.target.provider, tilo_actions::remote::OWN_PROVIDER);
    let delivered = fixture
        .app
        .invoke_bound_target_with_broker(
            &target,
            &ResourceInvocation::new(&target.address.address, &actor)
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(request.invocation)),
            Some(&mut |request| requests.push(request)),
        )
        .unwrap();
    assert!(matches!(
        delivered,
        crate::widget::ResourceOperationStart::Completed(_)
    ));
    assert!(
        requests.is_empty(),
        "an admitted delivery must not route in a loop"
    );
    assert_ne!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc.clone().get_heads())
            .unwrap(),
        before
    );
    DOCUMENT_JOB_MODE.set(false);
}

#[allow(
    clippy::too_many_lines,
    reason = "one controlled lifetime traces publication through refusal and delivery"
)]
#[test]
fn bound_publication_and_cache_keep_contracts_and_id_lifetimes() {
    // Case `task4_bound_publication_preserves_contracts_and_directed_owner`.
    {
        let mut fixture = ActionFixture::broker();
        let outer = fixture.app.state.doc.get_heads();
        let address = tilo_actions::ActionAddress::new("bound:probe", "follow-up");
        let actor = fixture.app.collaboration.local_author.0.clone();
        let error = fixture
            .app
            .invoke_resource_action(ResourceInvocation::new(&address, &actor).confirmed(true))
            .unwrap_err();
        assert_eq!(
            crate::bound_dispatch::door_error_code(&error),
            "ambiguous_action_target"
        );
        let resources = fixture.app.publication_resources();
        let first = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[0], "bound:probe")
            .unwrap();
        let second = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        assert_ne!(first, second);
        let descriptor = |id: &str| {
            tilo_actions::door::resolve(
                &resources,
                &tilo_actions::ActionAddress::new(id, "follow-up"),
            )
            .unwrap()
        };
        assert_eq!(
            descriptor(&first).contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        );
        assert_eq!(
            descriptor(&second).contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        let revision = fixture.app.action_directory_revision();
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &second, "follow-up"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        fixture
            .app
            .invoke_resource_action(
                ResourceInvocation::new(
                    &tilo_actions::ActionAddress::new(&second, "follow-up"),
                    &actor,
                )
                .confirmed(true)
                .broker_admitted(true)
                .evidence(Some(&serde_json::json!({"confirm": true})))
                .invocation(Some(tilo_actions::InvocationId::new())),
            )
            .unwrap();
        assert!(
            fixture
                .records
                .read_history(&fixture.bindings[0].checkout.id, |doc, history| history
                    .next_undo_in(doc, &fixture.bindings[0].author, 11)
                    .is_none())
                .unwrap()
        );
        fixture
            .records
            .with_history(&fixture.bindings[1].checkout.id, |doc, history| {
                assert!(doc.get(ROOT, "follow-up").unwrap().is_some());
                assert_eq!(
                    history
                        .next_undo_in(doc, &fixture.bindings[1].author, 12)
                        .unwrap()
                        .instance_id,
                    Some(12)
                );
            })
            .unwrap();
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
    }

    // Case `task4_bound_cache_tracks_permission_time_and_never_reuses_exhaustion`.
    {
        let mut fixture = ActionFixture::new();
        DISCOVERY_COUNT.set(0);
        let _ = fixture.app.collect_resources();
        let first = DISCOVERY_COUNT.get();
        let _ = fixture.app.collect_resources();
        assert_eq!(DISCOVERY_COUNT.get(), first);
        let expired = fixture.permissions.borrow().clone().at_unix_time(120);
        *fixture.permissions.borrow_mut() = expired;
        let resources = fixture.app.collect_resources();
        assert!(
            tilo_actions::door::resolve(
                &resources,
                &tilo_actions::ActionAddress::new("bound:probe", "follow-up")
            )
            .is_none()
        );
        assert!(DISCOVERY_COUNT.get() > first);
        let expired = fixture.permissions.borrow().clone();
        *fixture.permissions.borrow_mut() = expired.at_unix_time(100);
        fixture
            .records
            .with_action_state(&fixture.bindings[0].checkout.id, |_, _, state| {
                state.generation = Some(u64::MAX);
            })
            .unwrap();
        fixture.bindings[0]
            .with_document(&mut fixture.app.state.doc, |_, _| {})
            .unwrap();
        let _ = fixture.app.collect_resources();
        let before = DISCOVERY_COUNT.get();
        let _ = fixture.app.collect_resources();
        assert!(DISCOVERY_COUNT.get() > before);
        assert_eq!(
            fixture
                .records
                .action_generation(&fixture.bindings[0].checkout.id)
                .unwrap(),
            None
        );
    }

    // Case `task4_semantic_and_palette_targets_keep_equal_id_lifetimes`.
    {
        let mut fixture = ActionFixture::new();
        fixture.app.refresh_semantic_tree();
        let targets = fixture
            .app
            .accessibility
            .semantic_tree
            .bound_actions_for_test();
        let (id, _) = targets
            .iter()
            .find(|(_, target)| {
                target.address.instance_id == 12 && target.address.address.action == "view"
            })
            .unwrap();
        let outer = fixture.app.state.doc.get_heads();
        assert!(
            fixture
                .app
                .invoke_semantic_action(id, "view", None)
                .unwrap()
        );
        fixture.app.open_action_palette();
        let target = fixture.target(0, "view");
        assert!(
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .activate_bound(&target)
        );
        let pending = fixture
            .app
            .actions
            .action_palette
            .as_mut()
            .unwrap()
            .take_operation()
            .unwrap();
        assert!(
            pending
                .bound
                .as_ref()
                .unwrap()
                .binding
                .same_component(&target.binding)
        );
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
        fixture.app.close_components();
        let actor = fixture.app.collaboration.local_author.0.clone();
        assert!(
            fixture
                .app
                .invoke_bound_target(
                    &target,
                    &ResourceInvocation::new(&target.address.address, &actor).confirmed(true)
                )
                .is_err()
        );
    }

    // Case `task4_bound_directory_limits_cursors_and_alias_refusal`.
    {
        let mut fixture = ActionFixture::broker();
        let query = runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: String::new(),
            cursor: vec![],
            needle: String::new(),
            parent_resource: String::new(),
            max_results: 1,
            max_bytes: 8192,
            max_work: 16,
            max_depth: 8,
        };
        let first = fixture.app.action_directory_page(query.clone());
        assert_eq!(first.rows.len(), 1);
        assert!(!first.next_cursor.is_empty());
        let second = fixture
            .app
            .action_directory_page(runtime_collab::ActionDirectoryQuery {
                revision: first.revision.clone(),
                cursor: first.next_cursor.clone(),
                ..query.clone()
            });
        assert_eq!(second.rows.len(), 1);
        assert_ne!(first.rows[0].resource, second.rows[0].resource);
        assert_eq!(
            first.rows[0]
                .visibility
                .as_ref()
                .map(|visibility| visibility.action.as_str()),
            Some("view")
        );
        assert_eq!(
            second.rows[0]
                .visibility
                .as_ref()
                .map(|visibility| visibility.action.as_str()),
            Some("follow-up")
        );
        let eager = fixture.app.publication_resources();
        for page in [&first, &second] {
            assert!(
                eager
                    .iter()
                    .any(|resource| resource.id.as_str() == page.rows[0].resource)
            );
        }
        let alias = second.rows[0].resource.clone();
        let previous_permissions = fixture.permissions.borrow().clone();
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::stranger("fixture-user").at_unix_time(100);
        let stale = fixture
            .app
            .action_directory_page(runtime_collab::ActionDirectoryQuery {
                revision: first.revision.clone(),
                cursor: first.next_cursor,
                ..query
            });
        assert!(stale.stale_cursor);
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "follow-up")
                .is_none()
        );
        let actor = fixture.app.collaboration.local_author.0.clone();
        assert!(
            fixture
                .app
                .invoke_resource_action(
                    ResourceInvocation::new(
                        &tilo_actions::ActionAddress::new(&alias, "follow-up"),
                        &actor,
                    )
                    .confirmed(true)
                    .broker_admitted(true),
                )
                .is_err()
        );
        *fixture.permissions.borrow_mut() = previous_permissions;
        for malformed in [
            format!("{}broken", crate::bound_dispatch::PUBLICATION_PREFIX),
            format!("{alias}:extra"),
            format!(
                "{}{}",
                crate::bound_dispatch::PUBLICATION_PREFIX,
                "x".repeat(4096)
            ),
        ] {
            assert!(
                fixture
                    .app
                    .actions
                    .bound_publication
                    .target(&tilo_actions::ActionAddress::new(&malformed, "view"))
                    .is_err()
            );
            assert!(
                fixture
                    .app
                    .resolve_publication_resource(&malformed, "view")
                    .is_none()
            );
        }
        fixture.app.close_components();
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "follow-up")
                .is_none()
        );
        let mut replacement = ActionFixture::broker();
        fixture.app.actions.bound_broker_route_for_test =
            replacement.app.actions.bound_broker_route_for_test.clone();
        fixture.app.root_widget = std::mem::replace(
            &mut replacement.app.root_widget,
            Box::new(crate::widget::Text::new("empty")),
        );
        fixture.app.invalidate_widget_resource_cache();
        let replacements = fixture.app.publication_resources();
        assert!(!replacements.is_empty());
        assert!(
            replacements
                .iter()
                .all(|resource| resource.id.as_str() != alias)
        );
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "view")
                .is_none()
        );
    }

    // Case `task4_reserved_publication_collision_never_routes_to_legacy_widget`.
    {
        let mut fixture = ActionFixture::broker();
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        let old = std::mem::replace(
            &mut fixture.app.root_widget,
            Box::new(crate::widget::Text::new("empty")),
        );
        fixture.app.root_widget = Box::new(
            Column::new()
                .push(Box::new(Probe {
                    collision: Some(alias.clone()),
                    ..Probe::default()
                }))
                .push(old),
        );
        fixture
            .app
            .actions
            .action_registry
            .register::<FollowUp>("bound.apply");
        fixture.app.invalidate_widget_resource_cache();
        let resources = fixture.app.publication_resources();
        assert_eq!(
            resources
                .iter()
                .filter(|resource| resource.id.as_str() == alias)
                .count(),
            1
        );
        let descriptor = tilo_actions::door::resolve(
            &resources,
            &tilo_actions::ActionAddress::new(&alias, "follow-up"),
        )
        .unwrap();
        assert_eq!(
            descriptor.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            fixture
                .app
                .actions
                .bound_publication
                .alias(&fixture.bindings[0], &"x".repeat(4096))
                .is_err()
        );
    }

    // Case `task4_bound_control_authority_refreshes_without_an_outer_generation_change`.
    {
        let mut fixture = ActionFixture::new();
        CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
        fixture.app.refresh_control_action_availability();
        assert_eq!(
            CONTROL_AUTHORITY.with_borrow(Clone::clone),
            vec![(false, true), (true, true)]
        );
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::stranger("fixture-user").at_unix_time(100);
        CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
        fixture.app.refresh_control_action_availability();
        assert_eq!(
            CONTROL_AUTHORITY.with_borrow(Clone::clone),
            vec![(false, false), (true, false)]
        );
    }
}

#[test]
fn compound_bound_effects_preserve_typing_custody_and_redo() {
    // Case `task4_compound_view_and_noop_effect_preserve_pending_typing`.
    {
        for (action, mode) in [("view", 0), ("follow-up", 2), ("view", 1)] {
            COMPOUND_MODE.set(mode);
            let mut fixture = ActionFixture::with_same_record(true);
            let a = fixture.bindings[0].clone();
            let b = fixture.bindings[1].clone();
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "first").unwrap();
            })
            .unwrap();
            let pending_before = fixture
                .records
                .read(
                    &a.checkout.id,
                    automerge::transaction::Transactable::pending_ops,
                )
                .unwrap();
            let target = fixture.target(1, action);
            fixture
                .app
                .invoke_bound_target(
                    &target,
                    &ResourceInvocation::new(&target.address.address, "fixture-user")
                        .confirmed(true)
                        .invocation(Some(tilo_actions::InvocationId(uuid::Uuid::from_u128(91)))),
                )
                .unwrap();
            if mode == 0 {
                assert_eq!(
                    fixture
                        .records
                        .read(
                            &a.checkout.id,
                            automerge::transaction::Transactable::pending_ops
                        )
                        .unwrap(),
                    pending_before
                );
            }
            let b_entry = fixture
                .records
                .read_history(&a.checkout.id, |doc, history| {
                    history.next_undo_in(doc, &b.author, b.instance_id)
                })
                .unwrap();
            assert_eq!(b_entry.is_some(), mode == 1);
            if mode != 1 {
                assert!(
                    fixture
                        .records
                        .read_history(&a.checkout.id, |doc, history| history
                            .next_undo_in(doc, &a.author, a.instance_id)
                            .is_none())
                        .unwrap()
                );
            }
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "continued").unwrap();
            })
            .unwrap();
            a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            fixture
                .records
                .read(&a.checkout.id, |doc| {
                    assert_eq!(doc.get(ROOT, "typing").unwrap().is_some(), mode == 1);
                    assert_eq!(doc.get(ROOT, "follow-up").unwrap().is_some(), mode == 1);
                })
                .unwrap();
            COMPOUND_MODE.set(0);
        }
    }

    // Case `task4_undeclared_effect_spans_are_retained_excluded_and_keep_custody`.
    {
        for mode in [3, 4, 5, 6] {
            COMPOUND_MODE.set(mode);
            CAPTURES.with_borrow_mut(Vec::clear);
            let mut fixture = ActionFixture::with_same_record(true);
            let a = fixture.bindings[0].clone();
            let b = fixture.bindings[1].clone();
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "prior").unwrap();
            })
            .unwrap();
            let generation = fixture.records.action_generation(&a.checkout.id).unwrap();
            let target = fixture.target(1, "follow-up");
            let error = fixture
                .app
                .invoke_bound_target(
                    &target,
                    &ResourceInvocation::new(&target.address.address, "fixture-user")
                        .confirmed(true)
                        .invocation(Some(tilo_actions::InvocationId(uuid::Uuid::from_u128(92)))),
                )
                .unwrap_err();
            assert_eq!(
                crate::bound_dispatch::door_error_code(&error),
                "undeclared_document_mutation"
            );
            assert!(error.to_string().contains("retained"));
            assert_ne!(
                fixture.records.action_generation(&a.checkout.id).unwrap(),
                generation
            );
            b.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "later", true).unwrap();
            })
            .unwrap();
            b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            if mode != 3 {
                b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                    .unwrap()
                    .unwrap();
            }
            assert!(!b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo);
            a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            fixture
                .records
                .read(&a.checkout.id, |doc| {
                    assert!(doc.get(ROOT, "later").unwrap().is_none());
                    assert!(doc.get(ROOT, "typing").unwrap().is_none());
                    assert!(doc.get(ROOT, "follow-up").unwrap().is_none());
                    assert_eq!(
                        doc.get(ROOT, "raw-before").unwrap().is_some(),
                        matches!(mode, 3 | 4 | 6)
                    );
                    assert_eq!(
                        doc.get(ROOT, "raw-after").unwrap().is_some(),
                        matches!(mode, 5 | 6)
                    );
                })
                .unwrap();
            drop(fixture);
            assert!(CAPTURES.with_borrow(|captures| {
                captures.iter().any(|(name, _)| *name == "mutable-cancel")
            }));
            COMPOUND_MODE.set(0);
        }
    }

    // Case `task4_compound_contextual_optimistic_noop_and_failed_producers`.
    {
        for mode in [7, 8, 9, 10] {
            COMPOUND_MODE.set(mode);
            let mut fixture = ActionFixture::with_same_record(true);
            let a = fixture.bindings[0].clone();
            let b = fixture.bindings[1].clone();
            a.with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "prior").unwrap();
            })
            .unwrap();
            let target = fixture.target(1, "follow-up");
            fixture
                .app
                .invoke_bound_target(
                    &target,
                    &ResourceInvocation::new(&target.address.address, "fixture-user")
                        .confirmed(true),
                )
                .unwrap();
            assert_eq!(
                b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo,
                mode < 9
            );
            if mode < 9 {
                b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                    .unwrap()
                    .unwrap();
                assert!(!b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo);
            } else {
                assert!(
                    fixture
                        .records
                        .read_history(&a.checkout.id, |doc, history| history
                            .next_undo_in(doc, &a.author, a.instance_id)
                            .is_none())
                        .unwrap()
                );
            }
            a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
            fixture
                .records
                .read(&a.checkout.id, |doc| {
                    assert!(doc.get(ROOT, "typing").unwrap().is_none());
                    assert!(doc.get(ROOT, "follow-up").unwrap().is_none());
                    assert!(doc.get(ROOT, "failed-action").unwrap().is_none());
                })
                .unwrap();
            COMPOUND_MODE.set(0);
        }
    }

    // Case `task4_compound_unwind_restores_frame_and_preserves_unrelated_redo`.
    {
        COMPOUND_MODE.set(11);
        let mut fixture = ActionFixture::with_same_record(true);
        let a = fixture.bindings[0].clone();
        let b = fixture.bindings[1].clone();
        a.with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "typing", "prior").unwrap();
        })
        .unwrap();
        a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        let target = fixture.target(1, "follow-up");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = fixture.app.invoke_bound_target(
                &target,
                &ResourceInvocation::new(&target.address.address, "fixture-user").confirmed(true),
            );
        }));
        assert!(result.is_err());
        COMPOUND_MODE.set(0);
        assert!(a.undo_status(&mut fixture.app.state.doc).unwrap().can_redo);
        assert!(!b.undo_status(&mut fixture.app.state.doc).unwrap().can_undo);
        b.with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "later", true).unwrap();
        })
        .unwrap();
        b.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        a.history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Redo)
            .unwrap()
            .unwrap();
        fixture
            .records
            .read(&a.checkout.id, |doc| {
                assert!(doc.get(ROOT, "raw-unwind").unwrap().is_some());
                assert!(doc.get(ROOT, "typing").unwrap().is_some());
                assert!(doc.get(ROOT, "later").unwrap().is_none());
            })
            .unwrap();
    }

    // Case `task4_pending_status_selects_its_owner_without_closing_typing`.
    {
        let mut fixture = ActionFixture::new();
        let binding = fixture.bindings[0].clone();
        let outer = fixture.app.state.doc.get_heads();
        binding
            .with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "typing", "pending").unwrap();
            })
            .unwrap();
        let pending = fixture
            .records
            .read(
                &binding.checkout.id,
                automerge::transaction::Transactable::pending_ops,
            )
            .unwrap();
        assert!(
            binding
                .undo_status(&mut fixture.app.state.doc)
                .unwrap()
                .can_undo
        );
        assert!(
            !fixture.bindings[1]
                .undo_status(&mut fixture.app.state.doc)
                .unwrap()
                .can_undo
        );
        assert_eq!(
            fixture
                .records
                .read(
                    &binding.checkout.id,
                    automerge::transaction::Transactable::pending_ops
                )
                .unwrap(),
            pending
        );
        let entry = binding
            .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        assert_eq!(entry.instance_id, Some(11));
        assert!(
            binding
                .undo_status(&mut fixture.app.state.doc)
                .unwrap()
                .can_redo
        );
        assert!(
            fixture
                .records
                .read(&binding.checkout.id, |doc| doc
                    .get(ROOT, "typing")
                    .unwrap()
                    .is_none())
                .unwrap()
        );
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "fresh and revoked authority share the full visible-row and activation oracle; one controlled paging fixture covers completion, revocation and prompt pause"
)]
#[test]
fn bound_search_and_palette_disclose_only_authorized_provider_rows() {
    // Case `task4_denied_bound_provider_rows_are_not_disclosed`.
    {
        for initially_allowed in [false, true] {
            let mut fixture = ActionFixture::with_authority(false, true);
            let denied =
                crate::permissions::PermissionContext::stranger("fixture-user").at_unix_time(100);
            if !initially_allowed {
                *fixture.permissions.borrow_mut() = denied.clone();
            }
            fixture.app.open_action_palette();
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("sealed");
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .advance_search_page();
            if initially_allowed {
                assert!(
                    fixture
                        .app
                        .action_palette()
                        .unwrap()
                        .result_labels()
                        .contains(&"Hidden sealed write title".to_owned())
                );
                *fixture.permissions.borrow_mut() = denied;
                fixture.app.refresh_action_palette_resources();
                fixture
                    .app
                    .actions
                    .action_palette
                    .as_mut()
                    .unwrap()
                    .advance_search_page();
            }
            let labels = fixture.app.action_palette().unwrap().result_labels();
            assert!(!labels.contains(&"Hidden sealed write title".to_owned()));
            let allowed = labels
                .iter()
                .position(|label| label == "Allowed sealed sibling title")
                .unwrap();
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_cursor(allowed);
            assert!(
                fixture
                    .app
                    .action_palette_handle_key(&action_palette_tests::synth_enter())
            );
            for (index, binding) in fixture.bindings.iter().enumerate() {
                fixture
                    .records
                    .read(&binding.checkout.id, |doc| {
                        assert_eq!(doc.get(ROOT, "follow-up").unwrap().is_some(), index == 1);
                    })
                    .unwrap();
            }
            fixture.app.open_action_palette();
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("sealedopaque");
            fixture
                .app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .advance_search_page();
            let labels = fixture.app.action_palette().unwrap().result_labels();
            assert!(!labels.contains(&"Hidden sealedopaque title".to_owned()));
            assert!(labels.contains(&"Allowed sealedopaque title".to_owned()));
            assert!(
                fixture
                    .records
                    .read(&fixture.bindings[0].checkout.id, |doc| doc
                        .get(ROOT, "navigation")
                        .unwrap()
                        .is_none())
                    .unwrap()
            );
            assert!(
                fixture
                    .app
                    .state
                    .doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .is_none()
            );
        }
    }

    // Case `task4_completed_bound_search_validation_is_bounded`.
    {
        let mut fixture = ActionFixture::with_authority(false, true);
        let confirmation = fixture.target(1, "follow-up");
        let counts = Rc::new([Cell::new(0), Cell::new(0)]);
        PAGED_SEARCH_COUNTS.with_borrow_mut(|slot| *slot = Some(counts.clone()));
        fixture.app.open_action_palette();
        PAGED_SEARCH_COUNTS.with_borrow_mut(|slot| *slot = None);
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        palette.set_query("paged");
        let mut ticks = 0;
        while palette.advance_search_page().pending {
            ticks += 1;
            assert!(ticks < 20, "initial bounded query must finish");
        }
        assert_eq!(palette.result_labels().len(), 130, "no total-kind clipping");
        let step = |palette: &mut crate::action_palette::ActionPalette| {
            for counter in counts.iter() {
                counter.set(0);
            }
            let progress = palette.advance_search_page();
            let callbacks: usize = counts.iter().map(Cell::get).sum();
            assert!(
                callbacks <= 32,
                "{callbacks} resolver callbacks in one step"
            );
            assert!(progress.inspected <= 32);
            assert!(callbacks <= progress.inspected, "unaccounted provider work");
            progress
        };
        // A completed-query redraw starts at most one finite validation pass.
        let mut progress = step(palette);
        ticks = 0;
        while progress.pending {
            progress = step(palette);
            ticks += 1;
            assert!(
                ticks < 20,
                "completed validation must stop requesting redraws"
            );
        }
        assert_eq!(palette.result_labels().len(), 130);
        // Revoke during the next finite pass after this owner was visited;
        // queue order must not make the already-validated-row oracle accidental.
        ticks = 0;
        loop {
            assert!(step(palette).pending);
            if counts[0].get() > 0 {
                break;
            }
            ticks += 1;
            assert!(ticks < 5, "the first owner must participate in this pass");
        }
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::stranger("fixture-user").at_unix_time(100);
        ticks = 0;
        loop {
            progress = step(palette);
            assert!(
                palette
                    .result_labels()
                    .iter()
                    .all(|label| !label.starts_with("Revoked")),
                "unchecked stale rows must remain hidden"
            );
            if !progress.pending {
                break;
            }
            ticks += 1;
            assert!(ticks < 20, "revocation validation must finish");
        }
        assert_eq!(palette.result_labels().len(), 65);
        assert!(
            palette
                .result_labels()
                .iter()
                .all(|label| label.starts_with("Allowed"))
        );
        // The existing strict owner's follow-up enters the real confirmation
        // field. A partially drained validation pass must pause behind it.
        palette.set_query("");
        for tick in 0..20 {
            if !palette.advance_search_page().pending {
                break;
            }
            assert!(tick < 19, "reset query must complete");
        }
        assert!(step(palette).pending);
        assert!(palette.activate_bound(&confirmation));
        assert!(palette.in_argument_mode());
        assert!(palette.result_labels().is_empty());
        for _ in 0..2 {
            let progress = step(palette);
            assert_eq!(progress.inspected, 0);
            assert!(!progress.pending);
            assert!(palette.result_labels().is_empty());
        }
        palette.open();
        for tick in 0..20 {
            if !palette.advance_search_page().pending {
                break;
            }
            assert!(tick < 19, "reset query must complete");
        }
        assert_eq!(
            palette
                .result_labels()
                .iter()
                .filter(|label| label.starts_with("Allowed"))
                .count(),
            65
        );
    }

    // Case `task4_lazy_palette_and_opaque_navigation_keep_provider_owner`.
    {
        let mut fixture = ActionFixture::new();
        fixture.app.open_action_palette();
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        palette.set_query("lazy");
        for _ in 0..4 {
            palette.advance_search_page();
        }
        let targets = palette.referenced_bound_targets();
        assert_eq!(targets.len(), 2);
        fixture.app.refresh_action_palette_resources();
        let target = targets
            .iter()
            .find(|target| target.address.instance_id == 12)
            .unwrap();
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        assert!(palette.activate_bound(target));
        let pending = palette.take_operation().unwrap();
        assert!(
            pending
                .bound
                .unwrap()
                .binding
                .same_component(&target.binding)
        );
        fixture
            .app
            .invoke_bound_target(
                target,
                &ResourceInvocation::new(&target.address.address, "fixture-user").confirmed(true),
            )
            .unwrap();
        fixture.app.open_action_palette();
        let palette = fixture.app.actions.action_palette.as_mut().unwrap();
        palette.set_query("opaque");
        for _ in 0..4 {
            palette.advance_search_page();
        }
        assert_eq!(palette.len(), 2);
        palette.set_cursor(1);
        palette.activate_selected();
        let (id, binding) = palette.take_scoped_navigation().unwrap();
        let binding = binding.unwrap();
        crate::bound_dispatch::activate_bound_search(
            fixture.app.root_widget.as_mut(),
            &mut fixture.app.state.doc,
            &binding,
            &id,
        )
        .unwrap();
        for owner in &fixture.bindings {
            assert_eq!(
                fixture
                    .records
                    .read(&owner.checkout.id, |doc| doc
                        .get(ROOT, "navigation")
                        .unwrap()
                        .is_some())
                    .unwrap(),
                owner.same_component(&binding)
            );
        }
        assert!(fixture.app.state.doc.get_heads().is_empty());
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one controlled lifetime traces publication through refusal and delivery; one broker-route refusal test exercises every invalid witness against the same fixture"
)]
#[test]
fn bound_broker_routes_keep_the_chosen_contract_and_refuse_bad_witnesses() {
    // Case `task4_qualified_broker_request_retains_chosen_contract_and_directed_owner`.
    {
        let mut fixture = ActionFixture::broker();
        let target = fixture.target(1, "follow-up");
        let resources = fixture.app.publication_resources();
        let actor = fixture.app.collaboration.local_author.0.clone();
        let evidence = serde_json::json!({"token": "controlled-confirmation"});
        let mut requests = Vec::new();
        // This seam captures submission only; no broker acceptance is simulated.
        fixture
            .app
            .invoke_bound_target_with_broker(
                &target,
                &ResourceInvocation::new(&target.address.address, &actor)
                    .confirmed(true)
                    .evidence(Some(&evidence)),
                Some(&mut |request| requests.push(request)),
            )
            .unwrap();
        assert_eq!(requests.len(), 1);
        let CollabRequest::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
        } = requests.pop().unwrap()
        else {
            panic!("expected action request")
        };
        assert_eq!(
            request.target.provider,
            tilo_actions::remote::OWN_PROVIDER,
            "an app routes its own action through the host"
        );
        assert_eq!(
            fixture
                .app
                .actions
                .broker_requests
                .get(&BrokerReply::Receipt(request.invocation)),
            Some(&0)
        );
        assert_eq!(
            request.argument,
            tilo_actions::remote::InvokeArgument::Json(serde_json::Map::new())
        );
        assert_eq!(
            request.confirmation,
            App::broker_confirmation(Some(&evidence))
        );
        let resource = request.target.address.resource;
        let action = request.target.address.action;
        let descriptor = tilo_actions::door::resolve(
            &resources,
            &tilo_actions::ActionAddress::new(&resource, &action),
        )
        .unwrap();
        assert_eq!(
            descriptor.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            fixture
                .app
                .actions
                .bound_publication
                .target(&tilo_actions::ActionAddress::new(&resource, &action))
                .unwrap()
                .unwrap()
                .binding
                .same_component(&target.binding)
        );
        for binding in &fixture.bindings {
            assert!(
                fixture
                    .records
                    .read(&binding.checkout.id, |doc| doc
                        .get(ROOT, "follow-up")
                        .unwrap()
                        .is_none())
                    .unwrap()
            );
        }
        fixture.app.invoke_published_action(
            41,
            tilo_actions::InvocationId(uuid::Uuid::from_u128(41)),
            resource,
            action,
            None,
            &actor,
            None,
        );
        assert!(
            fixture
                .records
                .read_history(&fixture.bindings[0].checkout.id, |doc, history| history
                    .next_undo_in(doc, &fixture.bindings[0].author, 11)
                    .is_none())
                .unwrap()
        );
        assert!(
            fixture
                .records
                .read(&target.binding.checkout.id, |doc| doc
                    .get(ROOT, "follow-up")
                    .unwrap()
                    .is_some())
                .unwrap()
        );
        assert_eq!(
            fixture
                .records
                .read_history(&target.binding.checkout.id, |_, history| history.revision())
                .unwrap(),
            1
        );
        let before = fixture.app.actions.next_broker_request_id;
        let mut sent = false;
        assert!(
            App::dispatch_broker_request(
                &mut fixture.app.actions.next_broker_request_id,
                false,
                &target.address.address,
                None,
                None,
                None,
                |_, _| sent = true
            )
            .is_err()
        );
        assert!(!sent);
        assert_eq!(fixture.app.actions.next_broker_request_id, before);
    }

    // Case `task4_checked_action_reuses_same_record_frame_and_refuses_cross_record`.
    {
        for shared in [true, false] {
            let mut fixture = ActionFixture::with_same_record(shared);
            let target = fixture.target(1, "apply");
            let first = fixture.bindings[0].clone();
            let outer = fixture.app.state.doc.get_heads();
            let actor = fixture.app.collaboration.local_author.0.clone();
            let result = first
                .with_document(&mut fixture.app.state.doc, |doc, _| {
                    doc.put(ROOT, "before-nested", "A").unwrap();
                    let result = crate::bound_dispatch::invoke_resource_action(
                        fixture.app.root_widget.as_mut(),
                        doc,
                        &target,
                        fixture.app.actions.action_transport.as_mut(),
                        crate::bound_dispatch::ResourceActionRequest {
                            argument: None,
                            arguments: None,
                            actor: &actor,
                            confirmed: true,
                            broker_admitted: false,
                            invocation: tilo_actions::InvocationId::new(),
                            source_invocation: None,
                            ambient: None,
                            scope: None,
                            route: None,
                        },
                    );
                    doc.put(ROOT, "after-nested", "A").unwrap();
                    result
                })
                .unwrap();
            if shared {
                result.unwrap();
                first
                    .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                    .unwrap()
                    .unwrap();
                fixture
                    .records
                    .read_history(&first.checkout.id, |doc, history| {
                        assert!(doc.get(ROOT, "after-nested").unwrap().is_none());
                        assert!(doc.get(ROOT, "before-nested").unwrap().is_some());
                        assert!(doc.get(ROOT, "follow-up").unwrap().is_some());
                        assert_eq!(
                            history
                                .next_undo_in(doc, &first.author, 12)
                                .unwrap()
                                .instance_id,
                            Some(12)
                        );
                    })
                    .unwrap();
            } else {
                assert!(matches!(
                    result,
                    Err(crate::bound_dispatch::BoundActionError::Dispatch(
                        crate::bound_dispatch::BoundDispatchError::CrossRecordReentry
                    ))
                ));
                assert!(
                    fixture
                        .records
                        .read(&target.binding.checkout.id, |doc| doc
                            .get(ROOT, "follow-up")
                            .unwrap()
                            .is_none())
                        .unwrap()
                );
            }
            assert_eq!(fixture.app.state.doc.get_heads(), outer);
        }
    }

    // Case `task4_bound_broker_route_refuses_wrong_app_workspace_record_and_missing_witness`.
    {
        let mut fixture = ActionFixture::broker();
        let target = fixture.target(1, "follow-up");
        let matching = fixture
            .app
            .actions
            .bound_broker_route_for_test
            .clone()
            .unwrap();
        assert_eq!(fixture.app.publication_app_id(), matching.route.app_id);
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&target.binding, "bound:probe")
            .unwrap();
        let revision = fixture.app.action_directory_revision();
        let mut wrong_app = matching.clone();
        wrong_app.route.app_id = "so.tilo.other".into();
        let mut wrong_workspace = matching.clone();
        wrong_workspace.route.cluster_id = uuid::Uuid::nil();
        let mut wrong_record = matching.clone();
        wrong_record.route.record = Some("different-record".into());
        for witness in [
            Some(wrong_app),
            Some(wrong_workspace),
            Some(wrong_record),
            None,
        ] {
            assert_bound_broker_route_refused(&mut fixture, &target, &alias, &revision, witness);
        }
        let mut replacement = matching;
        replacement.epoch = uuid::Uuid::new_v4();
        fixture.app.actions.bound_broker_route_for_test = Some(replacement);
        assert_ne!(fixture.app.action_directory_revision(), revision);
        assert!(
            fixture
                .app
                .resolve_publication_resource(&alias, "follow-up")
                .is_some()
        );
    }
}

fn assert_bound_broker_route_refused(
    fixture: &mut ActionFixture,
    target: &crate::bound_dispatch::BoundActionTarget,
    alias: &str,
    revision: &str,
    witness: Option<crate::collaboration::PrimaryRouteWitness>,
) {
    fixture.app.actions.bound_broker_route_for_test = witness;
    let resources = fixture.app.publication_resources();
    assert!(resources.iter().all(|resource| {
        !resource
            .id
            .as_str()
            .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
    }));
    assert!(
        fixture
            .app
            .resolve_publication_resource(alias, "follow-up")
            .is_none()
    );
    assert_ne!(fixture.app.action_directory_revision(), revision);
    let next_id = fixture.app.actions.next_broker_request_id;
    let mut sent = false;
    let error = fixture
        .app
        .invoke_bound_target_with_broker(
            target,
            &ResourceInvocation::new(&target.address.address, "fixture-user").confirmed(true),
            Some(&mut |_| sent = true),
        )
        .unwrap_err();
    assert_eq!(
        crate::bound_dispatch::door_error_code(&error),
        "bound_broker_route_unavailable"
    );
    assert!(!sent);
    assert_eq!(fixture.app.actions.next_broker_request_id, next_id);
    fixture.app.invoke_published_action(
        50,
        tilo_actions::InvocationId(uuid::Uuid::from_u128(50)),
        alias.to_owned(),
        "follow-up".into(),
        None,
        "fixture-user",
        None,
    );
    assert!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc
                .get(ROOT, "follow-up")
                .unwrap()
                .is_none())
            .unwrap()
    );
    // Local component View actions do not need this manager's broker route.
    fixture
        .app
        .invoke_bound_target(
            &fixture.target(1, "view"),
            &ResourceInvocation::new(
                &tilo_actions::ActionAddress::new("bound:probe", "view"),
                "fixture-user",
            )
            .confirmed(true),
        )
        .unwrap();
}

#[test]
fn collaboration_walk_enters_bound_owner_and_refuses_closing_lifetime() {
    let host = crate::application_test_support::ProbeHost::new();
    let definition = crate::application_test_support::definition(collaboration_probe_factory);
    let target = LaunchTarget::New;
    let context = host
        .prepare_with_permissions(
            &definition,
            91,
            &target,
            crate::application_test_support::runner_permissions(),
        )
        .unwrap();
    let component = definition
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    let binding = component.callback_binding();
    let mut app =
        App::from_component(component, crate::application_test_support::runner_inputs()).unwrap();
    let (events, wake_handles) =
        COLLABORATION_OBSERVATIONS.with_borrow(|slot| slot.clone().unwrap());

    crate::widget::handle_collab_event(
        app.root_widget.as_mut(),
        &CollabEvent::FollowChanged { peer_id: None },
    );
    assert_eq!(events.get(), 1);
    assert_eq!(wake_handles.borrow().len(), 1);
    assert!(wake_handles.borrow()[0].is_active());

    assert_eq!(
        app.root_widget.children().len(),
        1,
        "fixture retains its root"
    );
    binding.closing.set(true);
    crate::widget::handle_collab_event(
        app.root_widget.as_mut(),
        &CollabEvent::FollowChanged { peer_id: None },
    );
    assert_eq!(
        events.get(),
        1,
        "closing owner cannot be reentered by tree traversal"
    );
    assert_eq!(wake_handles.borrow().len(), 1);
    binding.closing.set(false);
    app.close_components();
    assert!(
        !wake_handles.borrow()[0].is_active(),
        "callback captured the component lifetime, not detached execution"
    );
}

/// Inside the Tilo shell the app's window does not present search: a
/// component's request to open it is handed to the host as an intent, and
/// the host's tile drains it.
#[test]
fn a_hosted_components_search_request_becomes_a_host_intent() {
    let request = crate::search::SearchRequest::within(crate::search::ScopeDecl::new(
        "folder:inbox",
        "Inbox",
    ))
    .with_query("invoice");

    let mut fixture = ActionFixture::new();
    fixture.bindings[0]
        .queued_controls
        .borrow_mut()
        .push_back(crate::search::open_search_invocation(&request));
    assert!(fixture.app.dispatch_control_invocations() > 0);
    assert!(
        fixture.app.action_palette().is_none(),
        "a window that does not present search opens no palette of its own"
    );
    assert_eq!(
        fixture.bindings[0].take_search_request(),
        Some(request.clone())
    );
    assert_eq!(fixture.bindings[1].take_search_request(), None);

    // The tile hosting the component hands the request to the shell once.
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().into());
    let records = owners
        .open(
            "so.tilo.binding-probe",
            "00000000-0000-0000-0000-000000000009",
        )
        .unwrap();
    let views = Rc::new(
        crate::application_view::InstanceViewStore::new(
            temp.path().into(),
            "D",
            "00000000-0000-0000-0000-000000000009",
        )
        .unwrap(),
    );
    let definition = ApplicationDefinition {
        app_id: "so.tilo.binding-probe",
        title: "Probe",
        icon: IconDescriptor::Glyph("P"),
        record_kind: crate::record::RecordKind::new("probe"),
        room_record: true,
        required: vec![],
        optional: vec![],
        opens: &[],
        create: factory,
    };
    let context = HostContext::prepare(
        &definition,
        HostContextInputs {
            records,
            views,
            device_id: "D".into(),
            instance_id: 21,
            permissions: Rc::new(RefCell::new(
                crate::permissions::PermissionContext::permissive(),
            )),
            author: crate::permissions::UserId("fixture-user".into()),
            action_context: tilo_actions::ActionContext::default(),
        },
        &LaunchTarget::New,
    )
    .unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let binding = component.callback_binding();
    let mut tile = TileHost::from_component(component);
    assert!(tile.take_host_intent().is_none());
    binding.request_search(request.clone());
    assert_eq!(
        tile.take_host_intent(),
        Some(crate::widget::AppHostIntent::OpenSearch(request))
    );
    assert!(tile.take_host_intent().is_none(), "the request drains once");
}

fn behaviour_probe_factory(
    context: HostContext,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    let mut component = collaboration_probe_factory(context, target, restore)?;
    component.set_behaviour(crate::application::ComponentBehaviour {
        autofocus_on_structure_change: true,
        appearance_reconcile: true,
        capture_protected: true,
        window: crate::application::WindowRole::InputSurface,
    });
    Ok(component)
}

fn hosted_behaviour_app(create: crate::application::ComponentFactory) -> App {
    let host = crate::application_test_support::ProbeHost::new();
    let definition = crate::application_test_support::definition(create);
    let target = LaunchTarget::New;
    let context = host
        .prepare_with_permissions(
            &definition,
            92,
            &target,
            crate::application_test_support::runner_permissions(),
        )
        .unwrap();
    let component = definition
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    App::from_component(component, crate::application_test_support::runner_inputs()).unwrap()
}

#[cfg(unix)]
#[path = "responsive_nav_publication_tests.rs"]
mod responsive_nav_publication_tests;

fn hosted_behaviour(
    create: crate::application::ComponentFactory,
) -> crate::application::ComponentBehaviour {
    let app = hosted_behaviour_app(create);
    crate::application::ComponentBehaviour {
        autofocus_on_structure_change: app.autofocus_on_structure_change,
        appearance_reconcile: app.appearance_reconcile,
        capture_protected: app.capture_protected(),
        window: if app.window.app_id.is_some() {
            crate::application::WindowRole::InputSurface
        } else {
            crate::application::WindowRole::Ordinary
        },
    }
}

#[test]
fn the_hosting_window_takes_the_components_behaviour() {
    assert_eq!(
        hosted_behaviour(collaboration_probe_factory),
        crate::application::ComponentBehaviour::default()
    );
    assert_eq!(
        hosted_behaviour(behaviour_probe_factory),
        crate::application::ComponentBehaviour {
            autofocus_on_structure_change: true,
            appearance_reconcile: true,
            capture_protected: true,
            window: crate::application::WindowRole::InputSurface,
        }
    );
}

#[test]
fn deferred_input_stamp_is_read_only_and_invalidates_an_edit_and_its_inverse() {
    let mut fixture = ActionFixture::new();
    let owner = fixture.bindings[0].clone();
    let node = *fixture
        .app
        .layout
        .node_map
        .keys()
        .find(|node| {
            fixture
                .app
                .bindings
                .binding_for(**node)
                .is_some_and(|binding| binding.same_component(&owner))
        })
        .unwrap();
    let pending = fixture.app.state.doc.pending_ops();
    let before = fixture
        .app
        .bindings
        .document_edit_stamp(node, &fixture.app.state.doc)
        .unwrap();
    let repeated = fixture
        .app
        .bindings
        .document_edit_stamp(node, &fixture.app.state.doc)
        .unwrap();
    assert!(before == repeated);
    assert_eq!(
        fixture.app.state.doc.pending_ops(),
        pending,
        "reading a stamp must not commit or mutate the host document"
    );
    owner
        .with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "dictation-guard", "edit").unwrap();
        })
        .unwrap();
    let edited = fixture
        .app
        .bindings
        .document_edit_stamp(node, &fixture.app.state.doc)
        .unwrap();
    assert!(edited != before);
    owner
        .with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.delete(ROOT, "dictation-guard").unwrap();
        })
        .unwrap();
    let reverted = fixture
        .app
        .bindings
        .document_edit_stamp(node, &fixture.app.state.doc)
        .unwrap();
    assert!(
        reverted != edited && reverted != before,
        "restoring the same text must not revive a deferred insertion"
    );
    owner.closing.set(true);
    assert!(
        fixture
            .app
            .bindings
            .document_edit_stamp(node, &fixture.app.state.doc)
            .is_err(),
        "a retired owner cannot issue a live stamp"
    );
}

#[test]
fn deferred_input_embedded_app_follows_its_actual_editable_child() {
    use crate::widget::{Column, TextInput, Widget};
    let mut app = App::from_scoped_parts_with_controlled_inputs(
        crate::wake::WakeScope::new(),
        State::new(),
        Box::new(
            Column::new()
                .push(Box::new(TextInput::local().with_initial_value("same")))
                .push(Box::new(TextInput::local().with_initial_value("same"))),
        ),
    );
    app.init_layout();
    app.refresh_focus_order();
    let first = app.focusable_order[0];
    let second = app.focusable_order[1];
    app.set_focused_node(Some(first));
    let mut embedded = crate::app::EmbeddedApp::new(app, |_| {}).unwrap();
    assert!(crate::ime::ImeWindowState::from_widget(&embedded).is_some());
    let stamp = embedded.ime_edit_fingerprint(&[3; 32]).unwrap();
    embedded.app_mut().set_focused_node(Some(second));
    assert_ne!(
        embedded.ime_edit_fingerprint(&[3; 32]).unwrap(),
        stamp,
        "identical embedded fields must remain distinct insertion targets"
    );
    let stamp = embedded.ime_edit_fingerprint(&[3; 32]).unwrap();
    embedded
        .app_mut()
        .state
        .doc
        .put(ROOT, "guard", "edit")
        .unwrap();
    embedded.app_mut().state.doc.delete(ROOT, "guard").unwrap();
    assert_ne!(
        embedded.ime_edit_fingerprint(&[3; 32]).unwrap(),
        stamp,
        "an edit and inverse must invalidate deferred embedded input"
    );
    embedded.app_mut().set_focused_node(None);
    assert!(crate::ime::ImeWindowState::from_widget(&embedded).is_none());
    assert!(embedded.ime_edit_fingerprint(&[3; 32]).is_none());
}

#[test]
fn input_surfaces_keep_their_controls_without_native_document_chrome() {
    for (create, ordinary) in [
        (
            collaboration_probe_factory as crate::application::ComponentFactory,
            true,
        ),
        (
            behaviour_probe_factory as crate::application::ComponentFactory,
            false,
        ),
    ] {
        let mut app = hosted_behaviour_app(create);
        app.prepare_native_presentation(crate::host_chrome::HostChrome::InWindow);
        assert_eq!(app.history_chrome_enabled, ordinary);
        assert_eq!(app.native_document_chrome, ordinary);
        assert_eq!(
            app.document_sync_status().is_some(),
            ordinary,
            "an intentionally unattached input service is not a failed document route"
        );
        for (width, height) in [(800, 600), (600, 800)] {
            app.init_layout();
            let root = app.layout.root_node.expect("native root layout exists");
            run_layout_pass(&mut app, root, width, height, 1.0, false, None).unwrap();
            app.refresh_semantic_tree();
            let names: Vec<_> = app
                .semantic_tree()
                .iter()
                .filter_map(|node| node.name.as_deref())
                .collect();
            assert!(
                names.contains(&"Live owner"),
                "the actual application's accessible label remains: {names:?}"
            );
            assert!(
                tilo_actions::door::resolve(
                    &app.collect_local_resources(),
                    &tilo_actions::ActionAddress::new("bound-collaboration", "export"),
                )
                .is_some(),
                "the app's declared control remains available"
            );
            assert_eq!(names.contains(&"History"), ordinary);
        }
        app.action_publication_failure = Some("explicit publication refusal".into());
        assert_eq!(
            app.document_sync_status(),
            Some("explicit publication refusal"),
            "system-surface presentation must still expose actual errors"
        );
        app.close_components();
    }
}

#[test]
fn owner_takeover_follows_accepted_bound_actions_and_preserves_denials() {
    let mut fixture = ActionFixture::broker();
    fixture.app.collaboration.local_author = crate::permissions::UserId("fixture-user".into());
    let target = fixture.target(0, "follow-up");
    let actor = fixture.app.collaboration.local_author.0.clone();
    let granted = fixture.permissions.borrow().clone();
    fixture.app.continuation.owner_view.following = true;
    fixture.app.continuation.owner_view.pending =
        Some(crate::collaboration::ContinuationMetadata {
            schema_version: 1,
            bytes: b"pending-peer-view".to_vec(),
        });
    *fixture.permissions.borrow_mut() =
        crate::permissions::PermissionContext::stranger("fixture-user").at_unix_time(100);
    let heads = fixture.app.state.doc.get_heads();
    let mut actual_before = fixture
        .records
        .read(&target.binding.checkout.id, Clone::clone)
        .unwrap();
    let actual_heads = actual_before.get_heads();
    assert!(
        fixture
            .app
            .invoke_bound_target(
                &target,
                &ResourceInvocation::new(&target.address.address, &actor)
                    .confirmed(true)
                    .broker_admitted(true)
                    .invocation(Some(tilo_actions::InvocationId::new())),
            )
            .is_err()
    );
    assert!(fixture.app.continuation.owner_view.following);
    assert!(!fixture.app.continuation.owner_view.awaiting_claim);
    assert!(fixture.app.continuation.owner_view.pending.is_some());
    assert_eq!(fixture.app.state.doc.get_heads(), heads);
    let mut actual_after = fixture
        .records
        .read(&target.binding.checkout.id, Clone::clone)
        .unwrap();
    assert_eq!(actual_after.get_heads(), actual_heads);
    *fixture.permissions.borrow_mut() = granted;
    let accepted = fixture.app.invoke_bound_target(
        &target,
        &ResourceInvocation::new(&target.address.address, &actor)
            .confirmed(true)
            .broker_admitted(true)
            .invocation(Some(tilo_actions::InvocationId::new())),
    );
    assert!(accepted.is_ok(), "authorized local action: {accepted:?}");
    assert!(!fixture.app.continuation.owner_view.following);
    assert!(fixture.app.continuation.owner_view.awaiting_claim);
    assert!(fixture.app.continuation.owner_view.pending.is_none());
}

/// Hover over bound components keeps their published snapshots (TIL-628).
/// The dispatch lends each owner its record, which advances the owner's
/// admission generation whether or not anything was written; a record left
/// unwritten carries the snapshot forward, a written one never does.
#[cfg(feature = "native")]
#[test]
fn bound_hover_carries_snapshots_only_across_unwritten_records() {
    struct Surface;
    impl crate::app::AppSurface for Surface {
        fn scale_factor(&self) -> f64 {
            1.0
        }
        fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
            winit::dpi::PhysicalSize::new(800, 600)
        }
        fn request_redraw(&self) {}
        fn set_cursor(&self, _: CursorIcon) {}
    }
    let mut fixture = ActionFixture::new();
    let resources = fixture.app.collect_resources();
    assert!(
        tilo_actions::door::resolve(
            &resources,
            &tilo_actions::ActionAddress::new("bound:probe", "view")
        )
        .is_some(),
        "the bound probes publish"
    );
    DISCOVERY_COUNT.set(0);
    let first = DISCOVERY_COUNT.get();
    let generation = fixture
        .records
        .action_generation(&fixture.bindings[0].checkout.id)
        .unwrap();
    for step in 0..50 {
        fixture.app.surface_input(
            &Surface,
            WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(20.0 + f64::from(step), 20.0),
            },
            false,
        );
    }
    assert_ne!(
        fixture
            .records
            .action_generation(&fixture.bindings[0].checkout.id)
            .unwrap(),
        generation,
        "hover lends the owner its record"
    );
    let _ = fixture.app.collect_resources();
    assert_eq!(
        DISCOVERY_COUNT.get(),
        first,
        "hover over bound components must not rediscover their resources"
    );

    let marks = |app: &App| {
        crate::bound_dispatch::bound_owner_marks(app.root_widget.as_ref(), &app.state.doc)
    };
    let before = marks(&fixture.app);
    fixture.bindings[0]
        .with_document(&mut fixture.app.state.doc, |_, _| {})
        .unwrap();
    let after = marks(&fixture.app);
    assert!(
        fixture
            .app
            .actions
            .bound_resource_cache
            .carry_across_input(&before, &after),
        "an unwritten loan carries the snapshot"
    );
    let _ = fixture.app.collect_resources();
    assert_eq!(DISCOVERY_COUNT.get(), first);

    let before = marks(&fixture.app);
    fixture.bindings[0]
        .with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "hovered", true).unwrap();
        })
        .unwrap();
    let after = marks(&fixture.app);
    assert!(
        !fixture
            .app
            .actions
            .bound_resource_cache
            .carry_across_input(&before, &after),
        "a written record is never carried"
    );
    let _ = fixture.app.collect_resources();
    assert!(
        DISCOVERY_COUNT.get() > first,
        "the written record is rediscovered"
    );
}

/// A frame that hands over the same component snapshot reuses what was
/// derived from it (the controls' allowed addresses, the component's semantic
/// nodes) instead of copying and walking the whole published tree again, and
/// a changed snapshot or permission still produces exactly what a fresh
/// derivation would (TIL-761).
#[test]
fn unchanged_component_snapshots_keep_their_controls_and_semantic_nodes() {
    let mut fixture = ActionFixture::new();
    let at = |fixture: &ActionFixture, time| {
        let moved = fixture.permissions.borrow().clone().at_unix_time(time);
        *fixture.permissions.borrow_mut() = moved;
    };
    let availability = |fixture: &mut ActionFixture| {
        CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
        fixture.app.refresh_control_action_availability();
        CONTROL_AUTHORITY.with_borrow(Clone::clone)
    };
    assert_eq!(
        availability(&mut fixture),
        vec![(false, true), (true, true)]
    );
    assert_eq!(
        availability(&mut fixture),
        vec![(false, true), (true, true)],
        "a reused derivation answers as the first did"
    );
    at(&fixture, 120);
    assert_eq!(
        availability(&mut fixture),
        vec![(false, false), (true, false)],
        "an expired grant withdraws the controls"
    );
    at(&fixture, 100);
    assert_eq!(
        availability(&mut fixture),
        vec![(false, true), (true, true)]
    );

    fixture.app.refresh_semantic_tree();
    let (id, _) = fixture
        .app
        .accessibility
        .semantic_tree
        .bound_actions_for_test()
        .into_iter()
        .find(|(_, target)| target.address.instance_id == 11)
        .expect("a bound component action in the semantic tree");
    let before = std::ptr::from_ref(
        fixture
            .app
            .accessibility
            .semantic_tree
            .node(&id)
            .expect("the component's resource node"),
    );
    assert!(
        fixture.app.refresh_semantic_tree().is_empty(),
        "nothing changed"
    );
    assert!(
        std::ptr::eq(
            before,
            fixture
                .app
                .accessibility
                .semantic_tree
                .node(&id)
                .expect("the component's resource node"),
        ),
        "an unchanged snapshot keeps its nodes rather than rebuilding them"
    );

    at(&fixture, 120);
    assert!(
        !fixture.app.refresh_semantic_tree().is_empty(),
        "withdrawn actions change the tree"
    );
    let content = |tree: &crate::a11y_tree::SemanticTree| {
        let mut bound = tree
            .bound_actions_for_test()
            .into_iter()
            .map(|(id, target)| (id, target.address.address.action))
            .collect::<Vec<_>>();
        bound.sort();
        (tree.iter().cloned().collect::<Vec<_>>(), bound, tree.len())
    };
    let retained = content(&fixture.app.accessibility.semantic_tree);
    fixture.app.accessibility.semantic_tree = crate::a11y_tree::SemanticTree::new();
    fixture.app.refresh_semantic_tree();
    assert_eq!(
        retained,
        content(&fixture.app.accessibility.semantic_tree),
        "the retained tree is the tree a fresh refresh builds"
    );
}
