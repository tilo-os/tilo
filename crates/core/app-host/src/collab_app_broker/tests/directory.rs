#![allow(
    clippy::similar_names,
    reason = "sk/pk and owner/editor pairings are the conventional, intentional names for a keypair and a two-role fixture"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::panic_in_result_fn,
    clippy::arithmetic_side_effects,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::wildcard_imports,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    clippy::large_futures,
    clippy::await_holding_lock,
    clippy::await_holding_refcell_ref,
    reason = "test scenarios and fixtures: a failed unwrap is the assertion, and each scenario keeps its setup, guards and awaits in one readable body"
)]

use super::*;

tilo_test_env::scenario! {
    fn the_local_directory_bounds_its_wire_and_cleans_up_after_a_connection;

    phase connection_exit_cleanup_is_idempotent_and_preserves_survivors_and_replacements;
    {
        let fixture = local_directory_fixture();
        let survivor = fixture.peer_id + 1;
        let replacement = survivor + 1;
        let survivor_root = ActionDirectoryRoot {
            peer_id: survivor,
            app_id: fixture.route.app_id.clone(),
            cluster_id: fixture.cluster_id.clone(),
            document_id: "survivor".into(),
        };
        fixture
            .state
            .action_directory_roots
            .lock()
            .expect("directory roots")
            .insert(survivor, survivor_root);
        fixture.directories.directories.borrow_mut().insert(
            survivor,
            RegisteredActionDirectory {
                registration: Rc::new(uuid::Uuid::new_v4()),
                pagination: directory_pagination::DirectoryPagination::default(),
                publishing_person: Some("fixture-owner".into()),
                app_id: fixture.route.app_id.clone(),
                route: fixture.route.clone(),
                directory: Rc::clone(&fixture.directory),
            },
        );
        for peer_id in [fixture.peer_id, survivor] {
            fixture
                .state
                .publishers.surfaces
                .lock()
                .expect("published surfaces")
                .insert(
                    peer_id,
                    PublishedSurface {
                        peer_id,
                        app_id: fixture.route.app_id.clone(),
                        route: fixture.route.clone(),
                        actions: Vec::new(),
                        live: true,
                    },
                );
        }

        drop(ConnectionExit {
            state: Arc::clone(&fixture.state),
            directories: Rc::clone(&fixture.directories),
            peer_id: fixture.peer_id,
        });
        drop(ConnectionLease {
            local_media: None,
            state: Arc::clone(&fixture.state),
            directories: Rc::clone(&fixture.directories),
            peer_id: fixture.peer_id,
        });

        assert!(
            !fixture
                .state
                .publishers.surfaces
                .lock()
                .expect("published surfaces")
                .contains_key(&fixture.peer_id)
        );
        assert!(
            !fixture
                .directories
                .directories
                .borrow()
                .contains_key(&fixture.peer_id)
        );
        assert!(
            fixture
                .state
                .publishers.surfaces
                .lock()
                .expect("published surfaces")
                .contains_key(&survivor)
        );
        assert!(
            fixture
                .directories
                .directories
                .borrow()
                .contains_key(&survivor)
        );

        fixture
            .state
            .action_directory_roots
            .lock()
            .expect("directory roots")
            .insert(
                replacement,
                ActionDirectoryRoot {
                    peer_id: replacement,
                    app_id: fixture.route.app_id.clone(),
                    cluster_id: fixture.cluster_id.clone(),
                    document_id: "replacement".into(),
                },
            );
        fixture.state.refresh_action_directory_root_snapshot();
        let peer_ids = fixture
            .state
            .action_directory_roots()
            .into_iter()
            .map(|root| root.peer_id)
            .collect::<std::collections::HashSet<_>>();
        assert!(!peer_ids.contains(&fixture.peer_id));
        assert!(peer_ids.contains(&survivor));
        assert!(peer_ids.contains(&replacement));
    }

    phase directory_queue_refuses_oversized_query_text_and_cursor_before_contacting_the_app;
    {
        let fixture = local_directory_fixture();
        let queue = fixture.broker.action_directory_request_queue();
        let mut oversized_text = runtime_collab::ActionDirectoryQuery {
            peer_id: fixture.peer_id,
            revision: String::new(),
            cursor: Vec::new(),
            needle: "x".repeat(DIRECTORY_MAX_QUERY_NEEDLE_BYTES.saturating_add(1)),
            parent_resource: String::new(),
            max_results: 1,
            max_bytes: 1,
            max_work: 1,
            max_depth: 1,
            resource_name: String::new(),
        };
        assert!(matches!(
            queue.try_query_pending(
                BrokerActor::person("actor-a"),
                &fixture.cluster_id,
                oversized_text.clone()
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        oversized_text.needle.clear();
        oversized_text.cursor = vec![0; DIRECTORY_MAX_CURSOR_BYTES.saturating_add(1)];
        assert!(matches!(
            queue.try_query_pending(
                BrokerActor::person("actor-a"),
                &fixture.cluster_id,
                oversized_text
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        assert!(matches!(
            queue.try_resolve_pending(
                BrokerActor::person("actor-a"),
                "x".repeat(DIRECTORY_MAX_IDENTIFIER_BYTES.saturating_add(1)),
                fixture.peer_id,
                "directory-r1",
                "workspace:atlas",
                "open",
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        assert_eq!(
            fixture.directory_state.borrow().query_count,
            0,
            "invalid local requests are rejected before a directory capability is contacted"
        );
    }

    phase published_semantic_identity_retains_exact_directory_address_and_refuses_bad_wire;
    {
        let fixture = local_directory_fixture();
        let mut row = fixture.directory_state.borrow().row();
        let identity = runtime_collab::ExportedResourceSemanticIdentity {
            kind: "workspace".into(),
            key: "source-workspace:atlas".into(),
        };
        row.descriptor.as_mut().unwrap().semantic_identity = Some(identity);
        let action = published_directory_action(&row).unwrap();
        assert_eq!(action.resource, row.resource);
        assert_eq!(action.action, row.action.as_deref().unwrap());
        assert_eq!(
            action.semantic_identity,
            tilo_actions::ResourceSemanticIdentity::new("workspace", "source-workspace:atlas")
        );

        let decode = |kind: &str, key: &str| {
            let mut descriptor = row.descriptor.clone().unwrap();
            descriptor.resource_kind = "chat.agent".into();
            descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
                kind: kind.into(),
                key: key.into(),
            });
            descriptor.check()
        };
        assert_eq!(decode("chat.agent", "chat.person:scribe"), Ok(()));
        assert!(decode("chat.person", "chat.person:scribe").is_err());
        assert!(decode("chat.agent", "key\nsecret").is_err());
        assert!(decode("chat.agent", &"x".repeat(4097)).is_err());
        row.descriptor
            .as_mut()
            .unwrap()
            .semantic_identity
            .as_mut()
            .unwrap()
            .kind = "person".into();
        assert!(published_directory_action(&row).is_err());
    }

    phase directory_wire_rows_refuse_control_framing_in_identifiers_before_policy_or_invocation;
    {
        let fixture = local_directory_fixture();
        let mut row = fixture.directory_state.borrow().row();
        row.resource = "workspace:atlas\u{1f}admin".into();
        assert!(
            permits_directory_row(
                &fixture.state,
                &fixture.cluster_id,
                &BrokerActor::person("actor-a"),
                &fixture.route.app_id,
                None,
                &row,
            )
            .is_err(),
            "a resource cannot inject the shell result-id separator"
        );
        let mut row = fixture.directory_state.borrow().row();
        row.action = Some("open\nadmin".into());
        assert!(
            permits_directory_row(
                &fixture.state,
                &fixture.cluster_id,
                &BrokerActor::person("actor-a"),
                &fixture.route.app_id,
                None,
                &row,
            )
            .is_err(),
            "control characters cannot turn one wire action into several shell fields"
        );
    }

    phase retained_directory_root_pages_reach_exact_bound_and_tail_without_query_sorting;
    {
        let fixture = local_directory_fixture();
        {
            let mut roots = fixture
                .state
                .action_directory_roots
                .lock()
                .expect("directory roots");
            for peer_id in 1..=129_u64 {
                roots.insert(
                    peer_id,
                    ActionDirectoryRoot {
                        peer_id,
                        app_id: "so.tilo.test".into(),
                        cluster_id: fixture.cluster_id.clone(),
                        document_id: format!("root-{peer_id}"),
                    },
                );
            }
            drop(roots);
        }
        fixture.state.refresh_action_directory_root_snapshot();
        let first = fixture.state.action_directory_root_page(None, 128);
        assert_eq!(first.roots.len(), 128);
        assert_eq!(first.next_after_peer, Some(128));
        let second = fixture
            .state
            .action_directory_root_page(first.next_after_peer, 128);
        assert_eq!(second.roots.len(), 1);
        assert_eq!(second.roots[0].peer_id, 129);
        assert_eq!(second.next_after_peer, None);
        let repeat = fixture.state.action_directory_root_page(None, 1);
        assert_eq!(repeat.roots[0].peer_id, 1);
        assert_eq!(repeat.generation, second.generation);
        fixture.state.remove_connection(129);
        let after_disconnect = fixture.state.action_directory_root_page(Some(128), 1);
        assert!(after_disconnect.roots.is_empty());
        assert!(after_disconnect.generation > second.generation);
    }
}

/// The visibility contract is the permission that discloses a container
/// row. One the broker cannot decode refuses the page: the row is never
/// listed without its check, and nothing on that page is listed past it.
#[tokio::test(flavor = "current_thread")]
async fn a_row_whose_visibility_cannot_be_decoded_is_not_listed() {
    use tilo_actions::remote::{Caller, CatalogueQuery};
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            let caller = Caller::local_person("fixture-owner")
                .in_workspace(fixture.cluster_id.clone())
                .in_new_execution();
            let query = CatalogueQuery {
                provider: Some(fixture.peer_id.to_string()),
                max_results: 8,
                ..CatalogueQuery::default()
            };
            let list = || async {
                fixture
                    .drive(fixture.state.action_query(&caller, &query))
                    .await
            };
            let listed = list().await.expect("a well-formed directory lists");
            assert_eq!(
                listed.rows.len(),
                1,
                "the fixture row is visible to its owner"
            );

            let row = fixture.directory_state.borrow().row();
            fixture
                .directories
                .directories
                .borrow_mut()
                .get_mut(&fixture.peer_id)
                .expect("fixture directory")
                .directory = Rc::new(UndecodableVisibilityDirectory { row });
            let refused = list().await;
            assert!(refused.is_err(), "{refused:?}");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_call_made_while_the_directory_is_republished_is_answered_from_the_next_one() {
    use tilo_actions::remote::Caller;
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            let caller = Caller::local_person("fixture-owner")
                .in_workspace(fixture.cluster_id.clone())
                .in_new_execution();
            let actor = fixture.state.caller_actor(&caller);
            // The app answers two asks as it does while it publishes a change, then its new
            // directory answers.
            fixture.directory_state.borrow_mut().publishing = 2;
            let query = runtime_collab::ActionDirectoryQuery {
                peer_id: fixture.peer_id,
                revision: String::new(),
                cursor: Vec::new(),
                needle: String::new(),
                parent_resource: String::new(),
                max_results: 32,
                max_bytes: AppBrokerState::DIRECTORY_MAX_BYTES,
                max_work: 128,
                max_depth: 4,
                resource_name: String::new(),
            };
            let page = tilo_test_env::within(
                "the listing made while the app republishes",
                fixture.state.query_action_directory_for_actor(
                    &fixture.directories,
                    &actor,
                    &fixture.cluster_id,
                    &query,
                ),
            )
            .await
            .expect("the listing is answered from the app's next directory");
            assert_eq!(page.rows.len(), 1);
            assert_eq!(
                fixture.query_count(),
                3,
                "two answers from the app publishing, then the directory"
            );

            fixture.directory_state.borrow_mut().publishing = 1;
            let resolved = tilo_test_env::within(
                "the resolution made while the app republishes",
                fixture.state.resolve_action_directory_for_actor(
                    &fixture.directories,
                    &actor,
                    &fixture.cluster_id,
                    fixture.peer_id,
                    ("directory-r1", "workspace:atlas", "open"),
                    false,
                ),
            )
            .await
            .expect("the resolution is answered from the app's next directory");
            assert!(matches!(
                resolved,
                runtime_collab::ActionDirectoryResolution::Found { .. }
            ));
            assert_eq!(fixture.resolve_count(), 2);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn directory_fixture_preserves_scoped_uuid_through_actual_completion() {
    use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut fixture = local_directory_fixture();
            let caller = Caller::local_person("fixture-owner")
                .in_workspace(fixture.cluster_id.clone())
                .in_new_execution();
            let request = InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: fixture.peer_id.to_string(),
                    address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: Some("directory-r1".into()),
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            let pending = fixture.state.action_invoke(&caller, None, None, &request);
            let admitted = fixture.drive(pending).await;
            admitted.expect("full-envelope consumer supports scoped invocation UUIDs");
            let (job, delivered) = fixture.take_invocation_identity().expect("actual delivery");
            assert_eq!(delivered, request.invocation);
            assert_ne!(delivered.0, uuid::Uuid::from_u128(u128::from(job)));
            fixture
                .complete_invocation(
                    job,
                    &tilo_actions::InvocationUpdate::Succeeded {
                        invocation: delivered,
                        output: tilo_actions::ActionOutput::default(),
                    },
                )
                .await
                .expect("actual peer-bound completion accepts the delivered UUID");
            assert!(fixture.take_invocation_identity().is_none());
        })
        .await;
}

tilo_test_env::scenario! {
    fn the_dispatch_listing_follows_each_callers_authority;

    phase a_row_is_listed_to_its_owner_and_never_in_another_workspace;
    {
        let ListingFixture {
            state,
            tree,
            cluster,
            ..
        } = listing_state("same");
        state
            .publishers.surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:owner"));
        let mut other = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId::from_u128(30),
            "Other catalogue workspace",
        );
        other.policy.set_owner("same");
        let other_cluster = other.id().to_string();
        tree.lock().unwrap().workspaces.push(other);
        assert!(lists(&state, "same", &cluster, "files.entry:owner"));
        assert!(!lists(&state, "same", &other_cluster, "files.entry:owner"));
    }

    phase a_changed_authority_is_read_on_the_next_listing;
    {
        let ListingFixture {
            state,
            tree,
            cluster,
            ..
        } = listing_state("owner");
        state
            .publishers.surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:owner"));
        assert!(lists(&state, "owner", &cluster, "files.entry:owner"));

        {
            let mut tree = tree.lock().unwrap();
            tree.workspaces[0].policy.set_owner("replacement");
            tree.workspaces[0].policy.dismiss("owner");
        }
        assert!(!lists(&state, "owner", &cluster, "files.entry:owner"));
    }

    phase a_listing_whose_authority_fails_lists_nothing;
    {
        let ListingFixture {
            state,
            failed,
            cluster,
            ..
        } = listing_state("owner");
        state
            .publishers.surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:owner"));
        assert!(
            lists(&state, "owner", &cluster, "files.entry:owner"),
            "the current authorized listing is available"
        );
        failed.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(!lists(&state, "owner", &cluster, "files.entry:owner"));
    }
}

/// Whether the dispatch's listing offers `person`, in `workspace`, an action of `resource`.
fn lists(state: &AppBrokerState, person: &str, workspace: &str, resource: &str) -> bool {
    let caller = tilo_actions::remote::Caller::local_person(person).in_workspace(workspace);
    state
        .eager_catalogue(&caller, super::super::action_provider::RowScope::All)
        .0
        .iter()
        .filter(|row| row.action.is_some())
        .any(|row| row.resource.id == resource)
}
