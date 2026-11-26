tilo_test_env::scenario! {
    fn directory_disclosure_prefers_reads_to_private_views;

    phase directory_disclosure_prefers_a_document_read_then_a_read_of_the_record;
    // A record is disclosed through the read of its document; failing that,
    // through the read of the record itself — a record with no document read,
    // whose own window views (Show history, render) come before History's
    // `review-restore`, a read of the record, is disclosed through that read:
    // whoever may read the record sees it, an agent included, so its listing
    // reaches the record's actions (Terminal's session and its `run`).
    {
        let view = ActionSpec::operation(
            "render",
            "Private window",
            OperationKind::View,
            COUNTER_TYPE,
            &CounterAction::Bump,
        )
        .unwrap();
        let read = ActionSpec::operation(
            "read",
            "Read document",
            OperationKind::View,
            COUNTER_TYPE,
            &CounterAction::Bump,
        )
        .unwrap()
        .reads_document();
        let resource = ResourceSpec::new("readable", "test.document", "Document")
            .with_action(view)
            .with_action(read);
        let (app, _) = declared_directory(0);
        let row = app.directory_resource_row(&resource, "", true).unwrap();
        assert_eq!(row.visibility.as_ref().unwrap().action, "read");
        let resources = vec![resource];
        let descriptors = tilo_actions::api::survey(&resources);
        let snapshot = crate::app::action_directory_snapshot::ActionDirectorySnapshot::build(
            &resources,
            &descriptors,
            |_| None,
        );
        assert_eq!(
            snapshot.roots[0].visibility.as_ref().unwrap().action,
            "read"
        );
        // An explicit private disclosure binding must never be replaced by a
        // different action merely because that action is remotely readable.
        let snapshot = crate::app::action_directory_snapshot::ActionDirectorySnapshot::build(
            &resources,
            &descriptors,
            |_| Some(tilo_actions::ActionAddress::new("readable", "render")),
        );
        assert_eq!(
            snapshot.roots[0].visibility.as_ref().unwrap().action,
            "render"
        );

        let view = |id: &str| {
            ActionSpec::operation(
                id,
                id,
                OperationKind::View,
                COUNTER_TYPE,
                &CounterAction::Bump,
            )
            .unwrap()
        };
        let review = view("review").reads_resource();
        let session = ResourceSpec::new("session", "test.session", "Session")
            .with_action(view("render"))
            .with_action(view("history"))
            .with_action(review.clone());
        let readable = ResourceSpec::new("readable", "test.document", "Document")
            .with_action(view("render"))
            .with_action(review)
            .with_action(view("read").reads_document());
        for (resource, disclosed) in [(&session, "review"), (&readable, "read")] {
            let row = app.directory_resource_row(resource, "", true).unwrap();
            assert_eq!(row.visibility.as_ref().unwrap().action, disclosed);
        }
        let resources = vec![session, readable];
        let descriptors = tilo_actions::api::survey(&resources);
        let snapshot = crate::app::action_directory_snapshot::ActionDirectorySnapshot::build(
            &resources,
            &descriptors,
            |_| None,
        );
        let disclosed: Vec<&str> = snapshot
            .roots
            .iter()
            .map(|row| row.visibility.as_ref().unwrap().action.as_str())
            .collect();
        assert_eq!(disclosed, ["review", "read"]);
    }
}

// No bounded page or exact-resource override: this is the ordinary app declaration API.
struct DeclaredDirectory {
    calls: std::rc::Rc<Cell<usize>>,
    semantic_calls: std::rc::Rc<Cell<usize>>,
    count: usize,
    domain_provider: Option<DirectoryDomainProvider>,
}
impl Widget for DeclaredDirectory {
    crate::provides!(
        dyn crate::widget::resources::PublishesResources,
        dyn crate::search::capability::SearchSource,
        dyn crate::widget::ResourceOperations
    );

    fn sync(&mut self, _: &AppModel, _: &ModelPath) {}
    crate::testing::inert_widget_methods!(unregistered_leaf_layout, name = "declared-directory");
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.semantic_calls.set(self.semantic_calls.get() + 1);
        Some(crate::a11y::AccessNode::new(crate::a11y::Role::Group))
    }
}

impl crate::widget::ResourceOperations for DeclaredDirectory {
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

impl crate::widget::resources::PublishesResources for DeclaredDirectory {
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<ResourceSpec> {
        self.calls.set(self.calls.get() + 1);
        let effect = doc.get(ROOT, "effect").unwrap().is_some();
        let wide = doc.get(ROOT, "wide").unwrap().is_some();
        // Each generation of the document lists differently: its revision is
        // a new one.
        let generation = doc
            .get(ROOT, "generation")
            .unwrap()
            .map(|(value, _)| format!(" {value:?}"))
            .unwrap_or_default();
        let mut resources: Vec<ResourceSpec> = (0..self.count)
            .map(|index| {
                let mut resource = ResourceSpec::new(
                    format!("declared:{index}"),
                    "declared",
                    if doc.get(ROOT, "large-label").unwrap().is_some() {
                        format!("{}{generation}", "L".repeat(32 * 1024))
                    } else {
                        format!("Declared {index}{generation}")
                    },
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
                        action.type_mut().contract.confirmation =
                            tilo_actions::ConfirmationPolicy::Confirm;
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
    let descriptors = tilo_actions::api::survey(&resources);
    app.capture_action_directory_snapshot(&resources, &descriptors);
}

tilo_test_env::scenario! {
    fn retained_local_directory_bounds_what_it_pins;

    phase retained_local_directory_survives_mutation_but_never_resolves_ordinary_stale_input;
    {
        let (mut app, calls) = declared_directory(1);
        publish_declared_directory(&mut app);
        let revision = app.action_directory_revision();
        let pin = app
            .try_pin_local_document_action_directory(&revision)
            .ok()
            .unwrap();
        app.state_mut_for_test()
            .doc
            .put(ROOT, "removed", true)
            .unwrap();
        assert!(matches!(
            app.resolve_action_directory(&revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
        publish_declared_directory(&mut app);
        assert!(matches!(
            app.resolve_local_document_action_directory(&revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { revision: Some(exact), .. }
                if exact == revision
        ));
        for wrong in ["", "not-retained"] {
            assert!(matches!(
                app.resolve_local_document_action_directory(wrong, "declared:0", "inspect"),
                runtime_collab::ActionDirectoryResolution::StaleRevision
            ));
        }
        assert!(matches!(
            app.resolve_local_document_action_directory(&revision, "declared:0", "other"),
            runtime_collab::ActionDirectoryResolution::NotFound
        ));
        let calls_before = calls.get();
        drop(pin);
        assert!(matches!(
            app.resolve_local_document_action_directory(&revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
        assert_eq!(
            calls.get(),
            calls_before,
            "local resolution never rebuilds live resources"
        );
        // The same pin asked for again is one held snapshot, not another; a
        // revision the app never published is no pin at all, and the pin
        // holds until it is dropped.
        let (mut app, _) = declared_directory(1);
        publish_declared_directory(&mut app);
        let revision = app.action_directory_revision();
        let pins = (0..1024)
            .map(|_| {
                app.try_pin_local_document_action_directory(&revision)
                    .ok()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(pins.iter().all(|pin| std::rc::Rc::ptr_eq(&pins[0], pin)));
        assert_eq!(app.actions.local_document_directory_pins.len(), 1);
        assert!(
            app.try_pin_local_document_action_directory("wrong")
                .ok()
                .is_none()
        );
        assert!(matches!(
            app.resolve_local_document_action_directory(&revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        drop(pins);
        assert!(matches!(
            app.resolve_local_document_action_directory(&revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
        assert!(
            app.try_pin_local_document_action_directory(&revision)
                .ok()
                .is_some()
        );
    }

    phase a_report_holds_only_the_row_it_is_reported_against;
    // A change on its way to the broker holds the one row it is reported
    // against, not the whole surface its input froze: the next keystroke's
    // input fits to freeze its own.
    {
        let (mut app, _) = declared_directory(2);
        publish_declared_directory(&mut app);
        let revision = app.action_directory_revision();
        let surface = app
            .try_pin_local_document_action_directory(&revision)
            .ok()
            .unwrap();
        let capture = crate::local_document_action::CapturedAction {
            ownership: None,
            invocation: uuid::Uuid::nil(),
            capture_id: uuid::Uuid::nil(),
            revision: revision.clone(),
            published: Some(crate::local_document_action::PublishedInvocation {
                resource: "declared:0".into(),
                action: "inspect".into(),
                arguments: serde_json::Map::new(),
            }),
            encoded: tilo_actions::EncodedAction {
                type_id: COUNTER_TYPE.into(),
                payload: Vec::new(),
            },
            context: None,
            original_heads: Vec::new(),
            mutation_hashes: Vec::new(),
        };
        let held = app.report_pin(&capture).expect("the row the report names");
        assert!(matches!(
            app.resolve_local_document_action_directory(&revision, "declared:1", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        drop(surface);
        assert!(matches!(
            app.resolve_local_document_action_directory(&revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        assert!(matches!(
            app.resolve_local_document_action_directory(&revision, "declared:1", "inspect"),
            runtime_collab::ActionDirectoryResolution::NotFound
        ));
        drop(held);
        assert!(
            app.actions
                .local_document_directory_pins
                .iter()
                .all(|pin| pin.strong_count() == 0)
        );
    }

    phase retained_local_directory_rejects_oversized_snapshot;
    {
        // 64 resources with 32 KiB labels: several times the pin budget in
        // memory actually held, not only in the estimate.
        let (mut app, _) = declared_directory(64);
        app.state_mut_for_test()
            .doc
            .put(ROOT, "large-label", true)
            .unwrap();
        publish_declared_directory(&mut app);
        let revision = app.action_directory_revision();
        assert!(
            app.try_pin_local_document_action_directory(&revision)
                .ok()
                .is_none()
        );
        assert!(app.actions.local_document_directory_pins.is_empty());
    }

    phase a_snapshot_is_estimated_only_when_an_input_pins_it;
    // Every publication builds a snapshot, and serializing each of its actions
    // to estimate what it holds cost an app with many resources most of a
    // publication: the estimate waits for the local input that pins it.
    {
        let (mut app, _) = declared_directory(40);
        publish_declared_directory(&mut app);
        let snapshot = app.actions.action_directory_snapshot.clone().unwrap();
        assert!(
            !snapshot.retention_estimated(),
            "a publication estimated a snapshot nothing pinned"
        );
        let revision = app.action_directory_revision();
        let _pin = app
            .try_pin_local_document_action_directory(&revision)
            .ok()
            .unwrap();
        assert!(snapshot.retention_estimated(), "a pin is bounded by it");
    }

    phase retained_local_directory_distinct_revision_limit_preserves_pending_reports;
    {
        let (mut app, _) = declared_directory(1);
        publish_declared_directory(&mut app);
        let old = app.action_directory_revision();
        #[expect(
            clippy::collection_is_never_read,
            reason = "the pins are guards: holding them fills the limit, popping one frees a slot"
        )]
        let mut pins = vec![
            app.try_pin_local_document_action_directory(&old)
                .ok()
                .unwrap(),
        ];
        // Tiny subsequent snapshots isolate the count limit from the byte limit.
        for generation in 1..64 {
            app.state_mut_for_test()
                .doc
                .put(ROOT, "generation", generation)
                .unwrap();
            publish_declared_directory(&mut app);
            let revision = app.action_directory_revision();
            assert_ne!(revision, old);
            pins.push(
                app.try_pin_local_document_action_directory(&revision)
                    .ok()
                    .unwrap(),
            );
        }
        assert_eq!(app.actions.local_document_directory_pins.len(), 64);
        app.state_mut_for_test()
            .doc
            .put(ROOT, "generation", 64)
            .unwrap();
        publish_declared_directory(&mut app);
        let revision = app.action_directory_revision();
        assert!(
            app.try_pin_local_document_action_directory(&revision)
                .ok()
                .is_none()
        );
        assert!(matches!(
            app.resolve_local_document_action_directory(&old, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        pins.pop();
        assert!(
            app.try_pin_local_document_action_directory(&revision)
                .ok()
                .is_some()
        );
    }

    phase retained_local_directory_byte_pressure_preserves_old_revision_and_reclaims_dropped_pins;
    {
        let (mut app, _) = declared_directory(1);
        app.state_mut_for_test()
            .doc
            .put(ROOT, "large-label", true)
            .unwrap();
        publish_declared_directory(&mut app);
        let old = app.action_directory_revision();
        let mut pins = vec![
            app.try_pin_local_document_action_directory(&old)
                .ok()
                .unwrap(),
        ];
        let mut rejected = None;
        for generation in 1..64 {
            app.state_mut_for_test()
                .doc
                .put(ROOT, "generation", generation)
                .unwrap();
            publish_declared_directory(&mut app);
            let revision = app.action_directory_revision();
            if let Ok(pin) = app.try_pin_local_document_action_directory(&revision) {
                pins.push(pin);
            } else {
                rejected = Some(revision);
                break;
            }
        }
        let rejected = rejected.expect("byte budget must be reached before the revision limit");
        assert!(pins.len() < 64);
        assert!(matches!(
            app.resolve_local_document_action_directory(&old, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        assert!(matches!(
            app.resolve_local_document_action_directory(&rejected, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
        pins.clear();
        assert!(
            app.try_pin_local_document_action_directory(&rejected)
                .ok()
                .is_some()
        );
        assert!(matches!(
            app.resolve_local_document_action_directory(&old, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
    }
}

struct ExtendedDirectory;
impl ExtendedDirectory {
    fn resource_tree(extra: bool) -> ResourceSpec {
        fn inspect(id: &str, label: &str) -> ResourceSpec {
            ResourceSpec::new(id, "extended", label).with_action(
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
            parent.actions[0].type_mut().contract.kind = tilo_actions::ActionKind::Undoable;
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
    crate::provides!(
        dyn crate::widget::resources::PublishesResources,
        dyn crate::widget::ResourceOperations
    );

    crate::testing::inert_widget_methods!(unregistered_leaf_layout, name = "extended-directory");
}

impl crate::widget::ResourceOperations for ExtendedDirectory {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[COUNTER_TYPE]
    }
}

impl crate::widget::resources::PublishesResources for ExtendedDirectory {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<ResourceSpec> {
        vec![Self::resource_tree(false)]
    }
    fn resource(&self, _: &automerge::AutoCommit, id: &str) -> Option<ResourceSpec> {
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
fn connected_declared_directory_of(count: usize) -> (App, std::rc::Rc<Cell<usize>>) {
    let (mut app, calls) = declared_directory(count);
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
        count
    );
    (app, calls)
}

tilo_test_env::scenario! {
    #[cfg(all(unix, feature = "broker-transport"))]
    fn directory_lifecycle_over_the_broker_transport;

    phase a_pass_that_changes_nothing_listed_keeps_the_revision_and_its_cursor;
    // A pass that derives the directory again without changing what it lists
    // (a sync on every wake of a busy app) keeps the revision a reader holds and
    // the cursor its next page continues from.
    {
        let (mut app, _) = connected_declared_directory_of(3);
        let first = app.action_directory_page(directory_query("", "", 1, 8192, 8));
        assert_eq!(first.rows.len(), 1);
        assert!(!first.next_cursor.is_empty());
        for _ in 0..3 {
            assert!(app.sync_model(true));
            app.pump_collab_presence();
        }
        let mut next = directory_query(first.revision.clone(), "", 1, 8192, 8);
        next.cursor.clone_from(&first.next_cursor);
        let second = app.action_directory_page(next);
        assert!(
            !second.stale_cursor,
            "nothing listed changed, so the read continues"
        );
        assert_eq!(second.revision, first.revision);
        assert_eq!(second.rows.len(), 1);
        assert_ne!(second.rows[0], first.rows[0]);
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

    crate::testing::inert_widget_methods!(unregistered_leaf_layout, name = "described-search");
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

tilo_test_env::scenario! {
    fn directory_snapshot_pages_names_and_revises;

    phase directory_snapshot_pages_and_resolves_without_redeclaring_resources;
    {
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
            let visibility = root.rows[0].visibility.as_ref().unwrap();
            assert_eq!(
                visibility.governing_kind,
                Some(tilo_actions::OperationKind::Effect)
            );
            assert_eq!(visibility.contract.kind, tilo_actions::ActionKind::Undoable);
        }

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
            for row in &rows {
                let visibility = row.visibility.as_ref().unwrap();
                assert!(visibility.governing_kind.is_some());
                if let Some(descriptor) = &row.descriptor {
                    assert_eq!(visibility.governing_kind, Some(descriptor.operation_kind));
                }
            }
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

        {
            let (mut app, _) = declared_directory(1);
            let mut resources = app.publication_resources();
            let mut nested = resources.pop().unwrap();
            for depth in (0..17).rev() {
                let mut parent = ResourceSpec::new(format!("depth:{depth}"), "depth", "Depth");
                parent.children.push(nested);
                nested = parent;
            }
            let descriptors = tilo_actions::api::survey(std::slice::from_ref(&nested));
            app.capture_action_directory_snapshot(std::slice::from_ref(&nested), &descriptors);
            let mut query = directory_query("", "depth:15", 8, 8192, 8);
            query.max_depth = u16::MAX;
            let page = app.action_directory_page(query);
            assert_eq!(page.rows.len(), 1);
            assert_eq!(page.rows[0].resource, "depth:16");
            assert!(!page.rows[0].has_children);
        }

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
            let descriptors = tilo_actions::api::survey(&resources);
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

    phase directory_snapshot_revision_evicts_on_every_semantic_change;
    {
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
            let actor = tilo_actions::Actor::Member(app.local_author.clone());
            assert!(
                app.invoke_resource_action(ResourceInvocation::new(&address, &actor))
                    .is_err()
            );
        }

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
                &crate::permissions::MemberId::person("restricted"),
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

        {
            let (mut app, _) = declared_directory(2);
            publish_declared_directory(&mut app);
            let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
            let mut resources = app.publication_resources();
            let descriptors = tilo_actions::api::survey(&resources);
            let child = resources.pop().unwrap();
            resources[0].children.push(child);
            assert_eq!(tilo_actions::api::survey(&resources), descriptors);
            app.capture_action_directory_snapshot(&resources, &descriptors);
            assert_ne!(app.action_directory_revision(), page.revision);
            let mut query = directory_query(&page.revision, "", 1, 8192, 8);
            query.cursor = page.next_cursor;
            assert!(app.action_directory_page(query).stale_cursor);
            let parent = app.action_directory_page(directory_query("", "declared:0", 8, 8192, 8));
            assert_eq!(parent.rows.len(), 2);
            assert_eq!(parent.rows[1].resource, "declared:1");
        }

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
                .upsert_resource(
                    crate::search::SearchItem::new(
                        "test.item:1",
                        crate::search::SearchResult::for_action(
                            "Large directory",
                            "1",
                            tilo_actions::ActionAddress::new("test.item:1", "open"),
                        ),
                    )
                    .field("New record", crate::text_match::FieldWeight::UNRANKED),
                )
                .unwrap();
            assert_ne!(app.action_directory_revision(), revision);
            assert!(app.actions.action_directory_snapshot.is_none());
        }

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
            let actor = tilo_actions::Actor::Member(app.local_author.clone());
            let error = app
                .invoke_resource_action(ResourceInvocation::new(&address, &actor))
                .unwrap_err();
            assert_eq!(error, tilo_actions::ApiError::ConfirmationRequired);
        }
    }

    phase directory_revision_describes_search_sources_without_making_them;
    {
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
        assert_eq!(made.get(), 0, "the observation never makes a provider");
    }

    phase directory_names_a_child_among_its_parents_children_alone;
    // A name asked under a parent is answered among that parent's children
    // alone, whole: the child is found without a page of its siblings, and
    // neither the top level nor another parent's children answer for it.
    {
        let (mut app, _) = declared_directory(3);
        app.state_mut_for_test()
            .doc
            .put(ROOT, "hierarchy", true)
            .unwrap();
        let named = |app: &mut App, parent: &str, name: &str| {
            let mut query = directory_query("", parent, 8, 65536, 1);
            query.resource_name = name.into();
            app.action_directory_page(query)
        };
        for published in [false, true] {
            if published {
                publish_declared_directory(&mut app);
            }
            let page = named(&mut app, "declared:0", "declared 2");
            assert_eq!(
                page.rows
                    .iter()
                    .map(|row| (row.parent_resource.as_str(), row.resource.as_str()))
                    .collect::<Vec<_>>(),
                [("declared:0", "declared:2")],
                "published: {published}"
            );
            assert!(page.exhausted && page.next_cursor.is_empty());
            assert!(page.rows[0].action.is_none());
            assert!(
                named(&mut app, "", "Declared 2").rows.is_empty(),
                "not at the top level (published: {published})"
            );
            assert!(
                named(&mut app, "declared:1", "Declared 2").rows.is_empty(),
                "not under another parent (published: {published})"
            );
        }
    }

    phase directory_names_a_resource_whole_wherever_a_listing_would_page_it;
    {
        let (mut app, _) = declared_directory(137);
        let named = |app: &mut App, name: &str| {
            let mut query = directory_query("", "", 8, 65536, 1);
            query.resource_name = name.into();
            app.action_directory_page(query)
        };
        let ids = |page: &runtime_collab::ActionDirectoryPage| {
            page.rows
                .iter()
                .map(|row| row.resource.clone())
                .collect::<Vec<_>>()
        };
        // Unpublished, as between a write and the next publication, and then
        // from the published snapshot: the same answer either way.
        for published in [false, true] {
            if published {
                publish_declared_directory(&mut app);
            }
            let listed = app.action_directory_page(directory_query("", "", 128, 65536, 512));
            assert!(
                !ids(&listed).contains(&"declared:136".to_owned()),
                "the listing's first page ends before it"
            );
            let page = named(&mut app, "declared 136");
            assert_eq!(ids(&page), ["declared:136"], "published: {published}");
            assert!(page.exhausted && page.next_cursor.is_empty());
            assert!(page.rows[0].action.is_none());
            let by_kind = named(&mut app, "declared");
            assert_eq!(by_kind.rows.len(), 8, "a kind several share names each");
            assert!(named(&mut app, "undeclared").rows.is_empty());
        }
    }
}

tilo_test_env::scenario! {
    fn a_paged_read_continues_the_snapshot_its_revision_named;

    phase a_page_read_on_answers_though_the_directory_moved_meanwhile;
    // A read that pages a directory reads one snapshot: the revision it
    // started on keeps answering while this app holds it, whatever changed
    // since. A revision it never held is stale, and asked for again.
    {
        let (mut app, _) = declared_directory(3);
        publish_declared_directory(&mut app);
        let first = app.action_directory_page(directory_query("", "", 1, 8192, 8));
        assert_eq!(first.rows.len(), 1);
        assert!(!first.next_cursor.is_empty(), "the listing goes on");
        let revision = first.revision.clone();
        let first_resource = first.rows[0].resource.clone();
        let mut second = directory_query(&revision, "", 1, 8192, 8);
        second.cursor = first.next_cursor.clone();
        // The app's document moves on and publishes what it now lists: a new
        // revision, whose rows are not those the reader started on.
        app.state_mut_for_test()
            .doc
            .put(ROOT, "wide", true)
            .unwrap();
        publish_declared_directory(&mut app);
        let page = app.action_directory_page(second);
        assert!(!page.stale_cursor, "the read keeps the snapshot it started on");
        assert_eq!(page.revision, revision);
        assert_eq!(page.rows.len(), 1);
        assert_ne!(page.rows[0].resource, first_resource, "the second row follows the first");
        // What the app publishes meanwhile is no part of the read either.
        app.state_mut_for_test()
            .doc
            .put(ROOT, "effect", true)
            .unwrap();
        publish_declared_directory(&mut app);
        let mut third = directory_query(&revision, "", 1, 8192, 8);
        third.cursor = page.next_cursor.clone();
        let page = app.action_directory_page(third);
        assert!(!page.stale_cursor, "the third page reads on in the same snapshot");
        assert_eq!(page.revision, revision);
        assert_eq!(page.rows.len(), 1);
        // A revision this app never held is still stale.
        let unknown = directory_query("v5:unknown", "", 1, 8192, 8);
        assert!(app.action_directory_page(unknown).stale_cursor);
    }
}
