// Included in the real action-API fixture module.
use std::rc::Rc;

struct HostedModelTransport(Rc<RefCell<Vec<CollabRequest>>>);
impl tilo_ui_base::collaboration::PeerTransport for HostedModelTransport {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        self.0.borrow_mut().push(request);
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        vec![]
    }
    // Legacy owner-route delivery is explicit; no native capture is synthesized.
    fn join_route(
        &mut self,
        _: tilo_ui_base::collaboration::RouteSpec,
        _: crate::resource::ResourceType,
    ) -> Result<(), String> {
        Ok(())
    }
    fn send_to_route(
        &mut self,
        _: tilo_ui_base::collaboration::RouteSpec,
        request: CollabRequest,
    ) -> Result<(), String> {
        self.send(request)
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}

tilo_test_env::scenario! {
    fn an_app_with_no_window_runs_its_model_and_answers_on_its_wakes;

    phase hosted_model_advances_retained_job_once_without_draw_or_private_fixture_drain;
    {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_async_post_bump_reset()),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        let sent = Rc::new(RefCell::new(vec![]));
        app.collaboration.manager =
            CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
        let invocation = tilo_actions::InvocationId::new();
        app.invoke_published_action(
            871,
            invocation,
            "test.counter".into(),
            "async-bump".into(),
            None,
            &tilo_actions::Actor::Member(tilo_actions::MemberId::person("original-caller")),
            None,
        );
        assert_eq!(counter(&app), 0);
        app.pump_hosted_model();
        assert_eq!(
            counter(&app),
            1,
            "a hosted model pump must advance accepted work"
        );
        // Poll runs before model work, so cancellation wins before a continuation.
        // A completion produced by this frame is emitted on the next bounded pass.
        assert!(app.actions.directed_invocations.contains_key(&invocation));
        app.pump_hosted_model();
        assert!(!app.actions.directed_invocations.contains_key(&invocation));
        app.pump_hosted_model();
        assert_eq!(counter(&app), 1, "the completed job must not run again");
        assert_eq!(sent.borrow().iter().filter(|request| matches!(request,
            CollabRequest::UpdateInvocation { update: tilo_actions::InvocationUpdate::Succeeded { invocation: observed, .. }, .. } if *observed == invocation
        )).count(), 1, "one actual completion emits exactly one terminal update");
    }

    phase an_app_with_no_window_publishes_and_answers_on_its_wakes;
    // An app the session runs with no window publishes its actions and answers
    // one on its wakes alone: nothing is laid out or drawn for it.
    {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_async_post_bump_reset()),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        let sent = Rc::new(RefCell::new(vec![]));
        app.collaboration.manager =
            CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
        app.wake_unshown();
        assert!(
            app.actions
                .last_published_actions
                .as_ref()
                .is_some_and(|actions| !actions.is_empty()),
            "its actions are published before anything shows"
        );
        let invocation = tilo_actions::InvocationId::new();
        app.invoke_published_action(
            871,
            invocation,
            "test.counter".into(),
            "async-bump".into(),
            None,
            &tilo_actions::Actor::Member(tilo_actions::MemberId::person("original-caller")),
            None,
        );
        for _ in 0..3 {
            app.wake_unshown();
        }
        assert_eq!(counter(&app), 1, "the action ran");
        assert!(
            sent.borrow().iter().any(|request| matches!(request,
                CollabRequest::UpdateInvocation { update: tilo_actions::InvocationUpdate::Succeeded { invocation: observed, .. }, .. } if *observed == invocation)),
            "its caller is answered"
        );
    }
}

/// An app with no window whose view waits for what its workspace holds
/// hears from the shell where the workspace stands, on its wakes alone, and
/// then publishes what the view answers, as a window of it does.
#[cfg(feature = "native")]
#[test]
fn an_app_with_no_window_hears_its_workspace_and_publishes_its_view() {
    use std::io::Write as _;
    let (app_side, mut shell_side) = std::os::unix::net::UnixStream::pair().unwrap();
    tilo_ui_base::workspace_sync::hosted_by_shell();
    let view = crate::widget::ArrivingView::new(&tilo_ui_base::workspace_sync::holdings(), |()| {
        Box::new(CounterCard::with_async_post_bump_reset()) as tilo_ui_base::widget::Element
    });
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(view));
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.floating.client =
        Some(crate::floating_surface::FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedModelTransport(Rc::default()));
    let bumps = |app: &App| {
        app.actions
            .last_published_actions
            .iter()
            .flatten()
            .any(|action| action.address.action == "async-bump")
    };
    app.wake_unshown();
    assert!(
        !bumps(&app),
        "nothing of the view is built before the workspace is"
    );
    let mut told = serde_json::to_vec(
        &crate::floating_surface::FloatingSurfaceHostMessage::appearance(
            &tilo_ui_base::theme::WindowAppearance::default(),
        ),
    )
    .unwrap();
    told.push(b'\n');
    shell_side.write_all(&told).unwrap();
    // An answer whose placeholder already showed keeps it a moment: the app
    // wakes when it asks to, as its event loop does.
    let mut next = app.wake_unshown();
    for _ in 0..8 {
        if bumps(&app) {
            break;
        }
        if let Some(at) = next {
            std::thread::sleep(at.saturating_duration_since(crate::time::now()));
        }
        next = app.wake_unshown();
    }
    assert!(
        bumps(&app),
        "the view's actions are published once the shell said"
    );
}

fn hosted_directory_fixture() -> App {
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(DeclaredDirectory {
            domain_provider: None,
            calls: Rc::new(Cell::new(0)),
            semantic_calls: Rc::new(Cell::new(0)),
            count: 2,
        }),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.collaboration.manager = admitted_delivery_manager();
    app.init_layout();
    app.pump_collab_presence();
    app.publish_action_surface();
    app
}

crate::testing::test_app_widget!(
    DeclaredDirectory,
    "Declared directory",
    icon = crate::widget::IconDescriptor::Glyph("D")
);

#[derive(Clone)]
struct DirectoryDomainProvider {
    target: Rc<Cell<u8>>,
    supported: Rc<Cell<bool>>,
}
impl crate::search::SearchProvider for DirectoryDomainProvider {
    fn name(&self) -> &'static str {
        "domain-provider"
    }
    fn search(&self, _: &str) -> Vec<crate::search::SearchResult> {
        vec![crate::search::SearchResult::for_action(
            "lazy",
            "Lazy",
            tilo_actions::ActionAddress::new("lazy:target", "inspect"),
        )]
    }
    fn action_directory_generation(&self) -> Option<u64> {
        Some(0)
    }
    fn action_directory_invocation_domain(&self) -> tilo_ui_base::widget::ActionDirectoryInvocationDomain {
        if !self.supported.get() {
            return tilo_ui_base::widget::ActionDirectoryInvocationDomain::Unsupported;
        }
        tilo_ui_base::widget::ActionDirectoryInvocationDomain::versioned(
            &tilo_actions::ContinuationEvidence::Draft {
                resolution_domain: tilo_actions::ContinuationKey::new(
                    "lazy-domain",
                    [vec![self.target.get()]],
                ),
                handler: tilo_actions::ContinuationKey::new("lazy-handler", [b"v1".as_slice()]),
            },
        )
    }
    fn resolve_action_resource(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<ResourceSpec> {
        if address.resource != "lazy:target" || address.action != "inspect" {
            return None;
        }
        Some(
            ResourceSpec::new("lazy:target", "lazy", "Lazy").with_action(
                ActionSpec::operation(
                    "inspect",
                    "Inspect",
                    OperationKind::View,
                    COUNTER_TYPE,
                    &if self.target.get() == 0 {
                        CounterAction::Bump
                    } else {
                        CounterAction::Reset
                    },
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::view()),
            ),
        )
    }
}

struct HostedDirectoryNotificationTransport {
    events: Rc<RefCell<Vec<CollabEvent>>>,
    before_poll: Option<Box<dyn FnOnce()>>,
}
impl tilo_ui_base::collaboration::PeerTransport for HostedDirectoryNotificationTransport {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        if let Some(change) = self.before_poll.take() {
            change();
        }
        std::mem::take(&mut *self.events.borrow_mut())
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}

tilo_test_env::scenario! {
    fn hosted_directory_revisions_follow_what_the_owner_can_see;

    phase hosted_directory_noop_model_keeps_revision_and_actual_cursor;
    {
        let mut app = hosted_directory_fixture();
        let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
        assert_eq!(page.rows.len(), 1);
        assert!(!page.next_cursor.is_empty());
        app.pump_hosted_model();
        assert_eq!(
            app.action_directory_revision(),
            page.revision,
            "a real no-op hosted sync must preserve its published directory revision"
        );
        let mut query = directory_query(&page.revision, "", 1, 8192, 8);
        query.cursor = page.next_cursor;
        let next = app.action_directory_page(query);
        assert!(!next.stale_cursor);
        assert_eq!(next.rows.len(), 1);
    }

    phase hosted_directory_sync_contract_and_hierarchy_changes_invalidate_revision;
    {
        for change in ["changed-contract", "hierarchy"] {
            let mut app = hosted_directory_fixture();
            let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
            // The change arrives from elsewhere; the hosted pass syncs it.
            app.state.doc.put(ROOT, change, true).unwrap();
            app.pump_hosted_model();
            assert_ne!(
                app.action_directory_revision(),
                page.revision,
                "fresh changed projection must never inherit old revision"
            );
            let mut query = directory_query(&page.revision, "", 1, 8192, 8);
            query.cursor = page.next_cursor;
            assert!(app.action_directory_page(query).stale_cursor);
            let current = app.action_directory_page(directory_query("", "declared:0", 8, 8192, 8));
            if change == "hierarchy" {
                assert_eq!(current.rows.len(), 2);
            } else {
                let contract = &current.rows[0].visibility.as_ref().unwrap().contract;
                assert_eq!(
                    contract.confirmation,
                    tilo_actions::ConfirmationPolicy::Confirm
                );
            }
        }
    }

    phase hosted_directory_provider_generation_change_remains_stale;
    {
        let provider = LargeDirectoryProvider::new(1);
        let catalog = provider.catalog.clone();
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CounterCard::with_directory_provider(provider)),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.collaboration.manager = admitted_delivery_manager();
        app.init_layout();
        app.pump_collab_presence();
        app.publish_action_surface();
        let old = app.action_directory_revision();
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
                .field("New record", tilo_ui_base::text_match::FieldWeight::UNRANKED),
            )
            .unwrap();
        app.pump_hosted_model();
        assert_ne!(app.action_directory_revision(), old);
    }

    phase hosted_directory_hidden_immediate_target_change_stays_stale;
    {
        let mut app = hosted_directory_fixture();
        let old = app.action_directory_revision();
        let before = tilo_actions::api::survey(&app.publication_resources());
        app.state.doc.put(ROOT, "changed-payload", true).unwrap();
        app.pump_hosted_model();
        assert_eq!(
            tilo_actions::api::survey(&app.publication_resources()),
            before,
            "public descriptors deliberately omit the changed encoded target"
        );
        assert_ne!(
            app.action_directory_revision(),
            old,
            "an identical public descriptor cannot preserve a changed actual invocation"
        );
    }

    phase hosted_directory_lazy_target_domain_versions_even_with_unchanged_search_generation;
    {
        let provider = DirectoryDomainProvider {
            target: Rc::new(Cell::new(0)),
            supported: Rc::new(Cell::new(true)),
        };
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(DeclaredDirectory {
                calls: Rc::new(Cell::new(0)),
                semantic_calls: Rc::new(Cell::new(0)),
                count: 2,
                domain_provider: Some(provider.clone()),
            }),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.collaboration.manager = admitted_delivery_manager();
        app.init_layout();
        app.pump_collab_presence();
        app.publish_action_surface();
        let old = app.action_directory_revision();
        app.pump_hosted_model();
        assert_eq!(
            app.action_directory_revision(),
            old,
            "truthful unchanged lazy domain remains usable"
        );
        let before = tilo_actions::api::survey(&app.publication_resources());
        provider.target.set(1);
        app.pump_hosted_model();
        assert_eq!(
            tilo_actions::api::survey(&app.publication_resources()),
            before
        );
        assert_ne!(
            app.action_directory_revision(),
            old,
            "same search generation cannot hide changed lazy target"
        );
        let changed = app.action_directory_revision();
        provider.supported.set(false);
        app.pump_hosted_model();
        assert_ne!(
            app.action_directory_revision(),
            changed,
            "lost domain proof refuses continuity"
        );
    }

    phase hosted_directory_prompted_domain_handler_and_unknown_changes_stay_stale;
    {
        for change in [
            "changed-prompt-domain",
            "changed-prompt-handler",
            "lost-prompt-identity",
            "unknown-domain",
            "changed-domain",
        ] {
            let mut app = hosted_directory_fixture();
            app.state.doc.put(ROOT, "prompted", true).unwrap();
            app.pump_hosted_model();
            let old = app.action_directory_revision();
            app.pump_hosted_model();
            assert_eq!(
                app.action_directory_revision(),
                old,
                "explicit stable Draft identity must progress"
            );
            let before = tilo_actions::api::survey(&app.publication_resources());
            app.state.doc.put(ROOT, change, true).unwrap();
            app.pump_hosted_model();
            assert_eq!(
                tilo_actions::api::survey(&app.publication_resources()),
                before
            );
            assert_ne!(
                app.action_directory_revision(),
                old,
                "changed {change} cannot inherit old revision"
            );
        }
    }

    phase hosted_directory_registry_epoch_and_supported_large_surface;
    {
        let mut app = hosted_directory_fixture();
        app.state.doc.put(ROOT, "wide", true).unwrap();
        app.pump_hosted_model();
        let count = tilo_actions::api::survey(&app.publication_resources()).len();
        assert!(count > 256);
        let old = app.action_directory_revision();
        app.pump_hosted_model();
        assert_eq!(app.action_directory_revision(), old);
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.pump_hosted_model();
        assert_ne!(
            app.action_directory_revision(),
            old,
            "mutable registry access is an independent epoch"
        );
    }

    phase hosted_directory_equivalent_notification_before_sync_preserves_offered_revision;
    {
        let mut app = hosted_directory_fixture();
        let events = Rc::new(RefCell::new(vec![]));
        app.collaboration.manager =
            CollaborationManager::with_peer_transport(HostedDirectoryNotificationTransport {
                events: events.clone(),
                before_poll: None,
            });
        app.publish_action_surface();
        let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
        assert_eq!(page.rows.len(), 1);
        events.borrow_mut().push(CollabEvent::Context {
            permissions: vec![],
            apps: Vec::new(),
        });
        app.pump_hosted_model();
        assert!(
            events.borrow().is_empty(),
            "actual inbound pump must drain the notification"
        );
        assert_eq!(
            app.action_directory_revision(),
            page.revision,
            "equivalent notifications must not evict the offer before model comparison begins"
        );
        let mut query = directory_query(&page.revision, "", 1, 8192, 8);
        query.cursor = page.next_cursor;
        let next = app.action_directory_page(query);
        assert!(!next.stale_cursor);
        assert_eq!(next.rows.len(), 1);
    }

    phase hosted_directory_permission_expiry_or_failure_inside_notification_cannot_restore;
    {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        };
        for clock_failure in [false, true] {
            let mut app = hosted_directory_fixture();
            let time = Arc::new(AtomicU64::new(100));
            let failed = Arc::new(AtomicBool::new(false));
            let sample_time = time.clone();
            let sample_failed = failed.clone();
            app.permission_ctx = tilo_ui_base::permissions::PermissionContext::granted(
                &tilo_ui_base::permissions::MemberId::person("fixture-user"),
                &tilo_ui_base::permissions::TreePath::root(),
                tilo_ui_base::permissions::policy::KindSet::ALL,
                Some(110),
            )
            .at_unix_time(100)
            .with_policy_time_sampler(Arc::new(move || {
                if sample_failed.load(Ordering::Acquire) {
                    Err("actual clock retired".into())
                } else {
                    Ok(sample_time.load(Ordering::Acquire))
                }
            }));
            app.invalidate_widget_resource_cache();
            let events = Rc::new(RefCell::new(vec![CollabEvent::Context {
                permissions: vec![],
                apps: Vec::new(),
            }]));
            app.collaboration.manager =
                CollaborationManager::with_peer_transport(HostedDirectoryNotificationTransport {
                    events,
                    before_poll: Some(Box::new(move || {
                        if clock_failure {
                            failed.store(true, Ordering::Release)
                        } else {
                            time.store(110, Ordering::Release)
                        }
                    })),
                });
            app.publish_action_surface();
            let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
            assert_eq!(
                page.rows.len(),
                1,
                "the original sole grant admits the actual offer"
            );
            app.pump_hosted_model();
            assert_ne!(app.action_directory_revision(), page.revision);
            assert!(!matches!(
                app.resolve_action_directory(&page.revision, "declared:0", "inspect"),
                runtime_collab::ActionDirectoryResolution::Found { .. }
            ));
        }
    }

    phase hosted_directory_hidden_target_change_with_notification_cannot_restore;
    {
        let mut app = hosted_directory_fixture();
        let events = Rc::new(RefCell::new(vec![CollabEvent::Context {
            permissions: vec![],
            apps: Vec::new(),
        }]));
        app.collaboration.manager =
            CollaborationManager::with_peer_transport(HostedDirectoryNotificationTransport {
                events,
                before_poll: None,
            });
        app.publish_action_surface();
        let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
        let before = tilo_actions::api::survey(&app.publication_resources());
        app.state.doc.put(ROOT, "changed-payload", true).unwrap();
        app.pump_hosted_model();
        assert_eq!(
            tilo_actions::api::survey(&app.publication_resources()),
            before
        );
        assert_ne!(app.action_directory_revision(), page.revision);
    }
}

/// The workspace's members' resource as its governance answers it: each person's actor with the
/// name the workspace keeps for them.
fn members_answer(rows: &[(tilo_actions::Actor, &str)]) -> serde_json::Value {
    let people: Vec<serde_json::Value> = rows
        .iter()
        .map(|(who, name)| {
            serde_json::json!({
                "actor": serde_json::to_value(who).expect("an actor encodes"),
                "name": name,
            })
        })
        .collect();
    serde_json::json!({ "people": people })
}

tilo_test_env::scenario! {
    fn the_members_resource_numbers_and_names_people;

    phase a_store_answer_names_the_members_and_a_later_one_removes_them;
    {
        use tilo_actions::Actor;
        let mut app = hosted_directory_fixture();
        let helper = tilo_ui_base::permissions::MemberId::agent("build-helper", "original-caller");
        let me = Actor::Member(app.local_author().clone());
        let helper_actor = Actor::Member(helper.clone());
        app.apply_members_answer(&members_answer(&[
            (me.clone(), "Local person"),
            (helper_actor, "Build helper"),
        ]));
        assert_eq!(
            app.presence_ctx.display_name(&helper),
            "Build helper",
            "a member's name is the one the members' resource keeps"
        );
        assert_eq!(
            app.presence_ctx.self_label().as_deref(),
            Some("Local person"),
            "the person at the window is the row the workspace names"
        );

        // The workspace renamed the agent: a later answer names it what it is called now.
        app.apply_members_answer(&members_answer(&[
            (me.clone(), "Local person"),
            (Actor::Member(helper.clone()), "Build assistant"),
        ]));
        assert_eq!(
            app.presence_ctx.display_name(&helper),
            "Build assistant",
            "a rename shows as it is read"
        );

        // The workspace removed the agent: the next complete answer has no row for it.
        app.apply_members_answer(&members_answer(&[(me, "Local person")]));
        assert!(!app.presence_ctx.names.contains_key(&helper));

        // A workspace with nobody named leaves the person's own row unnamed.
        app.apply_members_answer(&serde_json::json!({ "people": [] }));
        assert!(app.presence_ctx.self_id().is_none());
    }

    phase ten_thousand_members_are_one_answer_and_no_frame_reads_them_again;
    {
        use tilo_actions::Actor;
        let mut app = hosted_directory_fixture();
        let me = app.local_author().clone();
        let people: Vec<serde_json::Value> = (0..10_000)
            .map(|n| {
                let who = if n == 0 {
                    Actor::Member(me.clone())
                } else {
                    Actor::Member(tilo_ui_base::permissions::MemberId::person(format!("p{n:05}")))
                };
                serde_json::json!({
                    "actor": serde_json::to_value(&who).expect("an actor encodes"),
                    "name": format!("Person {n}"),
                })
            })
            .collect();
        app.apply_members_answer(&serde_json::json!({ "people": people }));
        assert_eq!(app.presence_ctx.roster().len(), 10_000);
        let asked = app.collaboration.members_request;
        for _ in 0..3 {
            app.pump_collab_presence();
            assert_eq!(
                app.collaboration.members_request, asked,
                "a frame with nothing moved asks the store for nothing"
            );
        }
    }
}

/// A counter whose view, once the count moved, asks a worker for something
/// and owes the window its reply, taking the reply up in a later sync: as
/// a viewer reads what a file it just opened carries.
struct OwingCounter {
    card: CounterCard,
    seen: i64,
    /// Lets the worker reply; the test holds the other end.
    check: Option<std::sync::mpsc::Receiver<()>>,
    replies: Option<std::sync::mpsc::Receiver<i64>>,
    worker: Rc<RefCell<Option<std::thread::JoinHandle<()>>>>,
    taken: Rc<Cell<Option<i64>>>,
}

impl Widget for OwingCounter {
    fn sync(&mut self, s: &AppModel, p: &ModelPath) {
        self.card.sync(s, p);
        if let Some(reply) = self
            .replies
            .as_ref()
            .and_then(|replies| replies.try_recv().ok())
        {
            self.taken.set(Some(reply));
        }
        let (doc, _) = tilo_ui_base::doc::document_ref(s, p);
        let count = counter_in_doc(doc);
        if count == self.seen {
            return;
        }
        self.seen = count;
        if let Some(check) = self.check.take() {
            let owed = crate::wake::WakeHandle::new().owe();
            let (reply, replies) = std::sync::mpsc::channel();
            *self.worker.borrow_mut() = Some(std::thread::spawn(move || {
                let _ = check.recv();
                let _ = reply.send(count);
                drop(owed);
            }));
            self.replies = Some(replies);
        }
    }
    fn settle(&mut self, edits: &mut tilo_ui_base::widget::Edits<'_>, p: &automerge::ObjId) {
        self.card.settle(edits, p);
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let kid = self.card.build_layout(taffy, theme, node_map);
        taffy.widget_node(taffy::Style::default(), &[kid], node_map, self)
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.card]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.card]
    }
    fn name(&self) -> &'static str {
        "OwingCounter"
    }
}

/// A person's window answers an action the broker delivered once what the
/// action caused in the app has landed: the reply its view owed is taken
/// up before the caller learns the action succeeded, and meanwhile the
/// window's own frames go on.
#[test]
fn a_delivered_action_answers_once_the_work_it_caused_has_landed() {
    let (go, check) = std::sync::mpsc::channel();
    let taken = Rc::new(Cell::new(None));
    let worker = Rc::new(RefCell::new(None));
    let scope = crate::wake::WakeScope::new();
    let widget = {
        let _entry = scope.enter();
        Box::new(OwingCounter {
            card: CounterCard::new(),
            seen: 0,
            check: Some(check),
            replies: None,
            worker: Rc::clone(&worker),
            taken: Rc::clone(&taken),
        })
    };
    let mut app = App::from_scoped_parts_with_controlled_inputs(scope, State::new(), widget);
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.init_layout();
    let sent = Rc::new(RefCell::new(vec![]));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
    let answered = |sent: &Rc<RefCell<Vec<CollabRequest>>>| {
        sent.borrow().iter().any(|request| {
            matches!(
                request,
                CollabRequest::UpdateInvocation {
                    update: tilo_actions::InvocationUpdate::Succeeded { .. },
                    ..
                }
            )
        })
    };
    let _entry = app.enter_wake_scope();
    app.invoke_published_action(
        872,
        tilo_actions::InvocationId::new(),
        "test.counter".into(),
        "bump".into(),
        None,
        &tilo_actions::Actor::Member(tilo_actions::MemberId::person("original-caller")),
        None,
    );
    assert_eq!(counter(&app), 1);
    assert!(!answered(&sent), "the reply the bump caused is still owed");
    // The window's frames go on while it is owed; none answers.
    app.sync_model(true);
    assert!(!answered(&sent));

    go.send(()).unwrap();
    // The reply has landed once its worker ended.
    worker.borrow_mut().take().unwrap().join().unwrap();
    let mut frames = 0;
    while !answered(&sent) {
        assert!(frames < 2, "the landed reply answers within two frames");
        app.sync_model(true);
        frames += 1;
    }
    assert_eq!(
        taken.get(),
        Some(1),
        "the reply was taken up before the answer"
    );
}
