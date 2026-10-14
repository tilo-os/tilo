// No bounded page or exact-resource override: this is the ordinary app declaration API.
struct DeclaredDirectory {
    calls: std::rc::Rc<Cell<usize>>,
    semantic_calls: std::rc::Rc<Cell<usize>>,
    count: usize,
    domain_provider: Option<DirectoryDomainProvider>,
}
impl Widget for DeclaredDirectory {
    crate::provides!(
        dyn crate::resources::PublishesResources,
        dyn crate::search::capability::SearchSource
    );

    fn sync(&mut self, _: &AppModel, _: &ModelPath) {}
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        t.new_leaf(taffy::Style::default()).unwrap()
    }
    fn name(&self) -> &'static str {
        "declared-directory"
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.semantic_calls.set(self.semantic_calls.get() + 1);
        Some(crate::a11y::AccessNode::new(crate::a11y::Role::Group))
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[COUNTER_TYPE]
    }
}

impl crate::search::capability::SearchSource for DeclaredDirectory {
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        self.domain_provider
            .clone()
            .map(|provider| vec![Box::new(provider) as Box<dyn crate::search::SearchProvider>])
            .unwrap_or_default()
    }
}

impl crate::resources::PublishesResources for DeclaredDirectory {
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<Resource> {
        self.calls.set(self.calls.get() + 1);
        let effect = doc.get(ROOT, "effect").unwrap().is_some();
        let wide = doc.get(ROOT, "wide").unwrap().is_some();
        let mut resources: Vec<Resource> = (0..self.count)
            .map(|index| {
                let mut resource = Resource::new(
                    format!("declared:{index}"),
                    "declared",
                    format!("Declared {index}"),
                );
                if doc.get(ROOT, "removed").unwrap().is_none() {
                    resource = resource.with_action(
                        ActionSpec::operation(
                            "inspect",
                            "Inspect",
                            if effect {
                                OperationKind::Effect
                            } else {
                                OperationKind::View
                            },
                            COUNTER_TYPE,
                            &if doc.get(ROOT, "changed-payload").unwrap().is_some() {
                                CounterAction::Reset
                            } else {
                                CounterAction::Bump
                            },
                        )
                        .unwrap()
                        .with_contract(if effect {
                            tilo_actions::ActionContract::default()
                        } else {
                            tilo_actions::ActionContract::view()
                        }),
                    );
                }
                if doc.get(ROOT, "prompted").unwrap().is_some() {
                    let domain = doc.get(ROOT, "changed-prompt-domain").unwrap().is_some();
                    let handler = doc.get(ROOT, "changed-prompt-handler").unwrap().is_some();
                    let unknown = doc.get(ROOT, "lost-prompt-identity").unwrap().is_some();
                    resource.actions = vec![
                        ActionSpec::prompted_operation(
                            "inspect",
                            "Inspect",
                            "Value",
                            "",
                            OperationKind::View,
                            COUNTER_TYPE,
                            |value| {
                                EncodedAction::encode(
                                    COUNTER_TYPE,
                                    &CounterAction::SetLabel(value.into()),
                                )
                                .unwrap()
                            },
                        )
                        .with_contract(tilo_actions::ActionContract::view())
                        .with_continuation_identity(move |query| {
                            (!unknown && matches!(query, tilo_actions::ContinuationQuery::Draft))
                                .then(|| tilo_actions::ContinuationEvidence::Draft {
                                    resolution_domain: tilo_actions::ContinuationKey::new(
                                        "prompt-domain",
                                        [vec![u8::from(domain)]],
                                    ),
                                    handler: tilo_actions::ContinuationKey::new(
                                        "prompt-handler",
                                        [vec![u8::from(handler)]],
                                    ),
                                })
                        }),
                    ];
                }
                if doc.get(ROOT, "changed-contract").unwrap().is_some() {
                    for action in &mut resource.actions {
                        action.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
                    }
                }
                if wide {
                    for extra in 0..140 {
                        resource = resource.with_action(
                            ActionSpec::operation(
                                format!("extra:{extra}"),
                                "Extra action",
                                OperationKind::View,
                                COUNTER_TYPE,
                                &CounterAction::Bump,
                            )
                            .unwrap()
                            .with_contract(tilo_actions::ActionContract::view()),
                        );
                    }
                }
                resource
            })
            .collect();
        if doc.get(ROOT, "hierarchy").unwrap().is_some() && resources.len() > 1 {
            let child = resources.pop().unwrap();
            resources[0].children.push(child);
        }
        resources
    }
    fn action_directory_invocation_domain(
        &self,
        doc: &automerge::AutoCommit,
    ) -> crate::widget::ActionDirectoryInvocationDomain {
        // This fixture has no lazy provider or hidden handler state: every
        // target is the fixed CounterAction encoded in resources() below.
        if doc.get(ROOT, "unknown-domain").unwrap().is_some() {
            return crate::widget::ActionDirectoryInvocationDomain::Unsupported;
        }
        if doc.get(ROOT, "changed-domain").unwrap().is_some() {
            return crate::widget::ActionDirectoryInvocationDomain::versioned(
                &tilo_actions::ContinuationEvidence::Draft {
                    resolution_domain: tilo_actions::ContinuationKey::new(
                        "root",
                        [b"changed".as_slice()],
                    ),
                    handler: tilo_actions::ContinuationKey::new("handler", [b"v1".as_slice()]),
                },
            );
        }
        crate::widget::ActionDirectoryInvocationDomain::MaterializedOnly
    }
}
fn declared_directory(count: usize) -> (App, std::rc::Rc<Cell<usize>>) {
    let calls = std::rc::Rc::new(Cell::new(0));
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(DeclaredDirectory {
            domain_provider: None,
            calls: calls.clone(),
            semantic_calls: std::rc::Rc::new(Cell::new(0)),
            count,
        }),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    (app, calls)
}
fn publish_declared_directory(app: &mut App) {
    // The same validated snapshot used by the normal, connected publisher.
    let resources = app.publication_resources();
    assert!(
        !resources.is_empty(),
        "fixture must pass real surface validation"
    );
    let descriptors = tilo_actions::door::survey(&resources);
    app.capture_action_directory_snapshot(&resources, &descriptors);
}
#[test]
fn directory_snapshot_pages_and_resolves_without_redeclaring_resources() {
    // Case `directory_snapshot_resources_only_root_parent_and_exact_without_redeclaration`.
    {
        let (mut app, calls) = declared_directory(1);
        publish_declared_directory(&mut app);
        assert_eq!(calls.get(), 1);
        let root = app.action_directory_page(directory_query("", "", 8, 8192, 8));
        assert_eq!(
            root.rows.len(),
            1,
            "ordinary resources-only app must be discoverable"
        );
        assert_eq!(root.rows[0].resource, "declared:0");
        let parent =
            app.action_directory_page(directory_query(&root.revision, "declared:0", 8, 8192, 8));
        assert_eq!(parent.rows.len(), 1);
        assert_eq!(parent.rows[0].action.as_deref(), Some("inspect"));
        assert!(matches!(
            app.resolve_action_directory(&root.revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        assert_eq!(
            calls.get(),
            1,
            "remote directory operations cannot call resources()"
        );
    }

    // Case `directory_snapshot_pages_roots_and_resumes_byte_and_work_limits`.
    {
        let (mut app, calls) = declared_directory(137);
        publish_declared_directory(&mut app);
        let first = app.action_directory_page(directory_query("", "", 999, 65536, 999));
        assert_eq!(first.rows.len(), 128);
        let mut query = directory_query(&first.revision, "", 999, 65536, 999);
        query.cursor = first.next_cursor;
        let tail = app.action_directory_page(query);
        assert_eq!(tail.rows.len(), 9);
        assert_eq!(tail.rows[0].resource, "declared:128");
        assert!(tail.exhausted);
        let limited = app.action_directory_page(directory_query("", "", 128, 65536, 3));
        assert_eq!(limited.rows.len(), 3);
        let tiny = app.action_directory_page(directory_query("", "", 128, 1, 512));
        assert!(tiny.rows.is_empty());
        assert!(tiny.budget_exhausted);
        let mut query = directory_query(&tiny.revision, "", 1, 65536, 512);
        query.cursor = tiny.next_cursor;
        query.max_depth = 0;
        let retry = app.action_directory_page(query);
        assert_eq!(retry.rows[0].resource, "declared:0");
        assert!(!retry.rows[0].has_children);
        assert_eq!(calls.get(), 1);
    }

    // Case `directory_snapshot_mixed_provider_keeps_only_unprojected_tail`.
    {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_directory_roots(3)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        publish_declared_directory(&mut app);
        let mut query = directory_query("", "", 1, 65536, 512);
        let mut ids = Vec::new();
        loop {
            let page = app.action_directory_page(query.clone());
            ids.extend(
                page.rows
                    .iter()
                    .filter(|row| row.resource.starts_with("test.counter"))
                    .map(|row| row.resource.clone()),
            );
            if page.exhausted {
                break;
            }
            query.revision = page.revision;
            query.cursor = page.next_cursor;
        }
        assert_eq!(ids, ["test.counter", "test.counter:1", "test.counter:2"]);
    }

    // Case `directory_snapshot_parent_actions_page_and_effects_do_not_invent_container_visibility`.
    {
        let (mut app, calls) = declared_directory(1);
        app.state_mut_for_test()
            .doc
            .put(ROOT, "wide", true)
            .unwrap();
        publish_declared_directory(&mut app);
        let first = app.action_directory_page(directory_query("", "declared:0", 128, 65536, 512));
        assert!(first.rows.len() > 1 && first.rows.len() <= 128);
        let mut addresses = first
            .rows
            .iter()
            .map(|row| row.action.clone().unwrap())
            .collect::<std::collections::HashSet<_>>();
        let mut cursor = first.next_cursor;
        let mut exhausted = first.exhausted;
        while !exhausted {
            let mut query = directory_query(&first.revision, "declared:0", 128, 65536, 512);
            query.cursor = cursor;
            let page = app.action_directory_page(query);
            assert!(!page.rows.is_empty());
            addresses.extend(page.rows.iter().map(|row| row.action.clone().unwrap()));
            cursor = page.next_cursor;
            exhausted = page.exhausted;
        }
        assert_eq!(addresses.len(), 141);
        assert_eq!(calls.get(), 1);
        let (mut effect_app, _) = declared_directory(1);
        effect_app
            .state_mut_for_test()
            .doc
            .put(ROOT, "effect", true)
            .unwrap();
        publish_declared_directory(&mut effect_app);
        let root = effect_app.action_directory_page(directory_query("", "", 8, 8192, 8));
        assert_eq!(root.rows.len(), 1);
        assert_eq!(root.rows[0].action.as_deref(), Some("inspect"));
        assert_eq!(
            root.rows[0].descriptor.as_ref().unwrap().operation_kind,
            tilo_actions::OperationKind::Effect
        );
        let contract = &root.rows[0].visibility.as_ref().unwrap().contract;
        assert_eq!(contract.kind, tilo_actions::ActionKind::Undoable);
    }

    // Case `directory_snapshot_parent_merges_exact_provider_extensions_without_duplicates`.
    {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(ExtendedDirectory));
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        publish_declared_directory(&mut app);
        let mut query = directory_query("", "extended:parent", 1, 8192, 1);
        let mut rows = Vec::new();
        for _ in 0..20 {
            let page = app.action_directory_page(query.clone());
            rows.extend(page.rows);
            if page.exhausted {
                break;
            }
            query.revision = page.revision;
            query.cursor = page.next_cursor;
        }
        assert_eq!(
            rows.iter()
                .map(|row| (row.resource.as_str(), row.action.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("extended:parent", Some("inspect")),
                ("extended:a", None),
                ("extended:parent", Some("tail")),
                ("extended:b", None)
            ]
        );
        assert_eq!(rows[0].resource_label, "Published label");
        let contract = &rows[0].visibility.as_ref().unwrap().contract;
        assert_eq!(contract.kind, tilo_actions::ActionKind::Read);
        let revision = app.action_directory_revision();
        assert!(matches!(
            app.resolve_action_directory(&revision, "extended:parent", "tail"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        assert!(matches!(
            app.resolve_action_directory(&revision, "extended:b", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
    }

    // Case `directory_snapshot_depth_ceiling_disables_further_descent`.
    {
        let (mut app, _) = declared_directory(1);
        let mut resources = app.publication_resources();
        let mut nested = resources.pop().unwrap();
        for depth in (0..17).rev() {
            let mut parent = Resource::new(format!("depth:{depth}"), "depth", "Depth");
            parent.children.push(nested);
            nested = parent;
        }
        let descriptors = tilo_actions::door::survey(std::slice::from_ref(&nested));
        app.capture_action_directory_snapshot(std::slice::from_ref(&nested), &descriptors);
        let mut query = directory_query("", "depth:15", 8, 8192, 8);
        query.max_depth = u16::MAX;
        let page = app.action_directory_page(query);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].resource, "depth:16");
        assert!(!page.rows[0].has_children);
    }

    // Case `directory_snapshot_remote_requests_do_not_rebuild_semantics_or_execute_prompt_closures`.
    {
        let calls = std::rc::Rc::new(Cell::new(0));
        let semantics = std::rc::Rc::new(Cell::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(DeclaredDirectory {
                domain_provider: None,
                calls: calls.clone(),
                semantic_calls: semantics.clone(),
                count: 1,
            }),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        publish_declared_directory(&mut app);
        let surveyed = semantics.get();
        assert!(surveyed > 0);
        let mut resources = app.publication_resources();
        resources[0].actions.push(ActionSpec::prompted_param(
            "prompt",
            "Prompt",
            tilo_actions::ActionParam::required("value", tilo_actions::ParamType::Text, "Value"),
            "",
            ExecutionLocus::ClientLocal,
            |_| panic!("discovery must not execute an action-input closure"),
        ));
        let descriptors = tilo_actions::door::survey(&resources);
        app.capture_action_directory_snapshot(&resources, &descriptors);
        let revision = app.action_directory_revision();
        app.action_directory_page(directory_query(&revision, "", 8, 8192, 8));
        app.action_directory_page(directory_query(&revision, "declared:0", 8, 8192, 8));
        assert!(matches!(
            app.resolve_action_directory(&revision, "declared:0", "prompt"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        assert_eq!(semantics.get(), surveyed);
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn directory_snapshot_revision_evicts_on_every_semantic_change() {
    // Case `directory_snapshot_unchanged_publish_stable_but_mutation_and_max_evict`.
    {
        let (mut app, calls) = declared_directory(1);
        app.actions.resource_generation.set(u64::MAX);
        publish_declared_directory(&mut app);
        let revision = app.action_directory_revision();
        publish_declared_directory(&mut app);
        assert_eq!(app.action_directory_revision(), revision);
        assert_eq!(calls.get(), 1);
        app.state_mut_for_test()
            .doc
            .put(ROOT, "removed", true)
            .unwrap();
        assert!(matches!(
            app.resolve_action_directory(&revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
        let invalid = app.action_directory_page(directory_query("", "", 8, 8192, 8));
        assert!(invalid.rows.is_empty());
        assert_eq!(
            calls.get(),
            1,
            "invalidation does not permit remote rebuilding"
        );
        publish_declared_directory(&mut app);
        assert!(
            app.action_directory_page(directory_query("", "", 8, 8192, 8))
                .rows
                .is_empty()
        );
        assert_eq!(calls.get(), 2);
        let address = tilo_actions::ActionAddress::new("declared:0", "inspect");
        let actor = app.collaboration.local_author.0.clone();
        assert!(
            app.invoke_resource_action(ResourceInvocation::new(&address, &actor))
                .is_err()
        );
    }

    // Case `directory_snapshot_permission_change_at_max_evicts_without_committing_pending_doc`.
    {
        let (mut app, calls) = declared_directory(1);
        app.state_mut_for_test()
            .doc
            .put(ROOT, "pending", 1_i64)
            .unwrap();
        let pending = app.state.doc.pending_ops();
        assert!(pending > 0);
        publish_declared_directory(&mut app);
        let revision = app.action_directory_revision();
        app.action_directory_page(directory_query("", "", 1, 8192, 1));
        assert_eq!(
            app.state.doc.pending_ops(),
            pending,
            "discovery must not close the undo transaction"
        );
        app.actions.permission_generation.set(u64::MAX);
        app.set_permission_ctx(crate::permissions::PermissionContext::stranger(
            "restricted",
        ));
        assert_ne!(app.action_directory_revision(), revision);
        assert!(
            app.action_directory_page(directory_query("", "", 8, 8192, 8))
                .rows
                .is_empty()
        );
        assert_eq!(calls.get(), 1);
        publish_declared_directory(&mut app);
        let restricted = app.action_directory_revision();
        app.set_permission_ctx(PermissionContext::permissive());
        assert_ne!(app.action_directory_revision(), restricted);
        assert!(
            app.action_directory_page(directory_query("", "", 8, 8192, 8))
                .rows
                .is_empty()
        );
    }

    // Case `directory_snapshot_hierarchy_change_invalidates_cursor_even_with_same_descriptors`.
    {
        let (mut app, _) = declared_directory(2);
        publish_declared_directory(&mut app);
        let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
        let mut resources = app.publication_resources();
        let descriptors = tilo_actions::door::survey(&resources);
        let child = resources.pop().unwrap();
        resources[0].children.push(child);
        assert_eq!(tilo_actions::door::survey(&resources), descriptors);
        app.capture_action_directory_snapshot(&resources, &descriptors);
        assert_ne!(app.action_directory_revision(), page.revision);
        let mut query = directory_query(&page.revision, "", 1, 8192, 8);
        query.cursor = page.next_cursor;
        assert!(app.action_directory_page(query).stale_cursor);
        let parent = app.action_directory_page(directory_query("", "declared:0", 8, 8192, 8));
        assert_eq!(parent.rows.len(), 2);
        assert_eq!(parent.rows[1].resource, "declared:1");
    }

    // Case `directory_snapshot_provider_generation_expires_cached_projection`.
    {
        let provider = LargeDirectoryProvider::new(1);
        let catalog = provider.catalog.clone();
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_directory_provider(provider)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        publish_declared_directory(&mut app);
        let revision = app.action_directory_revision();
        catalog
            .upsert(
                "test.item:1",
                "New record",
                crate::search::SearchResult::for_action(
                    "Large directory",
                    "1",
                    tilo_actions::ActionAddress::new("test.item:1", "open"),
                ),
            )
            .unwrap();
        assert_ne!(app.action_directory_revision(), revision);
        assert!(app.actions.action_directory_snapshot.is_none());
    }

    // Case `directory_snapshot_old_descriptor_never_bypasses_changed_confirmation_contract`.
    {
        let (mut app, _) = declared_directory(1);
        publish_declared_directory(&mut app);
        let revision = app.action_directory_revision();
        assert!(matches!(
            app.resolve_action_directory(&revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        app.state_mut_for_test()
            .doc
            .put(ROOT, "changed-contract", true)
            .unwrap();
        let address = tilo_actions::ActionAddress::new("declared:0", "inspect");
        let actor = app.collaboration.local_author.0.clone();
        let error = app
            .invoke_resource_action(ResourceInvocation::new(&address, &actor))
            .unwrap_err();
        assert_eq!(error, tilo_actions::DoorError::ConfirmationRequired);
    }
}

struct ExtendedDirectory;
impl ExtendedDirectory {
    fn resource_tree(extra: bool) -> Resource {
        fn inspect(id: &str, label: &str) -> Resource {
            Resource::new(id, "extended", label).with_action(
                ActionSpec::operation(
                    "inspect",
                    "Inspect",
                    OperationKind::View,
                    COUNTER_TYPE,
                    &CounterAction::Bump,
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::view()),
            )
        }
        let mut parent = inspect(
            "extended:parent",
            if extra {
                "Provider label must not replace publication"
            } else {
                "Published label"
            },
        );
        parent.children.push(inspect("extended:a", "A"));
        if extra {
            parent.actions[0].contract.kind = tilo_actions::ActionKind::Undoable;
            parent.actions.push(
                ActionSpec::operation(
                    "tail",
                    "Provider action",
                    OperationKind::View,
                    COUNTER_TYPE,
                    &CounterAction::Bump,
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::view()),
            );
            parent.children.push(inspect("extended:b", "B"));
        }
        parent
    }
}
impl Widget for ExtendedDirectory {
    crate::provides!(dyn crate::resources::PublishesResources);

    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        t.new_leaf(taffy::Style::default()).unwrap()
    }
    fn name(&self) -> &'static str {
        "extended-directory"
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[COUNTER_TYPE]
    }
}

impl crate::resources::PublishesResources for ExtendedDirectory {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<Resource> {
        vec![Self::resource_tree(false)]
    }
    fn resource(&self, _: &automerge::AutoCommit, id: &str) -> Option<Resource> {
        let tree = Self::resource_tree(true);
        if id == tree.id.as_str() {
            Some(tree)
        } else {
            tree.children
                .into_iter()
                .find(|child| child.id.as_str() == id)
        }
    }
}

#[cfg(all(unix, feature = "broker-transport"))]
fn connected_declared_directory() -> (App, std::rc::Rc<Cell<usize>>) {
    let (mut app, calls) = declared_directory(1);
    app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
    assert!(app.collaboration.manager.is_connected());
    app.init_layout();
    // Join delivers an initial catalogue event. Drain it before measuring an
    // otherwise idle pump, so startup traffic cannot mask missing refresh.
    app.collaboration.manager.wait_for_commands_for_test();
    app.pump_collab_presence();
    app.publish_action_surface();
    app.collaboration.manager.wait_for_commands_for_test();
    assert!(
        !app.pump_collab_presence(),
        "fixture must be idle after the join baseline"
    );
    assert!(app.actions.last_published_actions.is_some());
    assert_eq!(
        app.action_directory_page(directory_query("", "", 8, 8192, 8))
            .rows
            .len(),
        1
    );
    (app, calls)
}

#[cfg(all(unix, feature = "broker-transport"))]
#[test]
fn directory_lifecycle_publishes_once_ready_and_refreshes_on_idle() {
    // Case `a_manager_that_connects_after_launch_publishes_the_action_surface_once_ready`.
    {
        use crate::collaboration::CollaborationState;
        let (mut app, _) = declared_directory(1);
        let (manager, _scope, wakes) = CollaborationManager::connecting_for_lifecycle_test();
        app.collaboration.manager = manager;
        app.init_layout();
        assert_eq!(app.collaboration_state(), CollaborationState::Connecting);
        app.publish_action_surface();
        assert!(
            app.actions.last_published_actions.is_none(),
            "nothing rides the primary route before the broker acknowledges it"
        );

        let deadline = tilo_test_env::hang_deadline();
        while app.collaboration_state() == CollaborationState::Connecting {
            app.pump_collab_presence();
            if app.collaboration_state() != CollaborationState::Connecting {
                break;
            }
            wakes
                .recv_timeout(deadline.saturating_duration_since(crate::time::now()))
                .expect("the connecting manager wakes the app when it settles");
        }

        assert_eq!(app.collaboration_state(), CollaborationState::Ready);
        assert!(
            app.actions.last_published_actions.is_some(),
            "becoming ready publishes the current action surface without another trigger"
        );
    }

    // Case `directory_lifecycle_idle_refreshes_after_normal_sync_without_remote_query`.
    {
        let (mut app, calls) = connected_declared_directory();
        let revision = app.action_directory_revision();
        app.state_mut_for_test()
            .doc
            .put(ROOT, "changed-contract", true)
            .unwrap();
        // This production entrypoint has the same sync -> invalidate ordering as
        // RedrawRequested, without requiring a live winit Window.
        app.refresh_focus_order();
        assert!(app.actions.action_directory_snapshot.is_none());
        let pending = app.state.doc.pending_ops();
        assert!(pending > 0);
        assert!(
            !app.pump_collab_presence(),
            "no inbound event may trigger publication for this regression"
        );
        assert!(
            app.actions.action_directory_snapshot.is_some(),
            "idle must repair the snapshot evicted by sync"
        );
        let rows = app.action_directory_page(directory_query("", "declared:0", 8, 8192, 8));
        assert_eq!(rows.rows.len(), 1);
        assert_ne!(rows.revision, revision);
        let contract = &rows.rows[0].visibility.as_ref().unwrap().contract;
        assert_eq!(
            contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert_eq!(
            app.state.doc.pending_ops(),
            pending,
            "refresh must not commit pending undo work"
        );
        let materializations = calls.get();
        let revision = app.action_directory_revision();
        for _ in 0..3 {
            app.pump_collab_presence();
        }
        assert_eq!(
            calls.get(),
            materializations,
            "stable idle must not rematerialize declarations"
        );
        assert_eq!(app.action_directory_revision(), revision);
    }

    // Case `directory_lifecycle_presence_eviction_recovers_on_normal_idle_without_input`.
    {
        let (mut app, calls) = connected_declared_directory();
        app.apply_collab_event_for_test(CollabEvent::RemoteCursorMoved {
            user_id: "peer".into(),
            x: 1.0,
            y: 2.0,
        });
        assert!(
            app.actions.action_directory_snapshot.is_none(),
            "presence retains canonical cache invalidation"
        );
        assert!(
            !app.pump_collab_presence(),
            "presence was already applied; this is an empty idle pump"
        );
        assert!(
            app.actions.action_directory_snapshot.is_some(),
            "presence-only invalidation must not permanently remove discovery"
        );
        let root = app.action_directory_page(directory_query("", "", 8, 8192, 8));
        assert_eq!(root.rows.len(), 1);
        assert!(matches!(
            app.resolve_action_directory(&root.revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        let materializations = calls.get();
        app.pump_collab_presence();
        assert_eq!(calls.get(), materializations);
    }
}

/// A search source whose providers are costly to make (an index behind
/// them) and which describes them without making them.
struct DescribedSearch {
    made: std::rc::Rc<Cell<usize>>,
    generation: std::rc::Rc<Cell<u64>>,
}
impl Widget for DescribedSearch {
    crate::provides!(dyn crate::search::capability::SearchSource);

    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        t.new_leaf(taffy::Style::default()).unwrap()
    }
    fn name(&self) -> &'static str {
        "described-search"
    }
}
impl crate::search::capability::SearchSource for DescribedSearch {
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        self.made.set(self.made.get() + 1);
        Vec::new()
    }
    fn search_provider_descriptors(
        &self,
    ) -> Vec<crate::search::capability::SearchProviderDescriptor> {
        vec![crate::search::capability::SearchProviderDescriptor::new(
            "described",
            Some(self.generation.get()),
            crate::widget::ActionDirectoryInvocationDomain::Unsupported,
        )]
    }
}

#[test]
fn directory_revision_describes_search_sources_without_making_them() {
    let made = std::rc::Rc::new(Cell::new(0));
    let generation = std::rc::Rc::new(Cell::new(0));
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(DescribedSearch {
            made: made.clone(),
            generation: generation.clone(),
        }),
    );
    let revision = app.action_directory_revision();
    assert_eq!(app.action_directory_revision(), revision);
    generation.set(1);
    assert_ne!(
        app.action_directory_revision(),
        revision,
        "a provider naming a new build moves the directory"
    );
    assert_eq!(made.get(), 0, "the witness never makes a provider");
}
