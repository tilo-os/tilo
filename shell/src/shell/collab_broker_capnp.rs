//! Shared production RPC broker. Native host adapters live in broker_services.
pub use qos_action_broker::collab_broker_capnp::*;

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::shell::search::providers::live_app_actions::LiveAppActionsProvider;
    use crate::shell::search::{InvocationDisposition, SearchContext, SearchProvider};
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    use automerge::{AutoCommit, ObjId};
    pub use qos_action_broker::collab_broker_capnp::tests::DirectoryResolveMode;
    use std::sync::Arc;
    use std::time::Duration;
    pub struct LocalDirectoryFixture {
        inner: qos_action_broker::collab_broker_capnp::tests::LocalDirectoryFixture,
        chrome: crate::shell::chrome_state::Chrome,
    }
    impl std::ops::Deref for LocalDirectoryFixture {
        type Target = qos_action_broker::collab_broker_capnp::tests::LocalDirectoryFixture;
        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl std::ops::DerefMut for LocalDirectoryFixture {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }
    fn local_directory_fixture() -> LocalDirectoryFixture {
        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::test_default(WorkspaceId(71), "Directory test room");
        workspace.owner = Some(crate::shell::state::local_user_id_for_room(&workspace.room));
        tree.workspaces.push(workspace);
        tree.set_active(WorkspaceId(71));
        local_directory_fixture_with_membership(tree.shared())
    }
    pub fn local_directory_fixture_with_membership(
        membership: WorkspaceTreeHandle,
    ) -> LocalDirectoryFixture {
        fixture(membership, Some(71))
    }
    pub fn local_directory_fixture_with_first_allocated_peer(
        membership: WorkspaceTreeHandle,
    ) -> LocalDirectoryFixture {
        fixture(membership, None)
    }
    fn fixture(membership: WorkspaceTreeHandle, peer: Option<u64>) -> LocalDirectoryFixture {
        let authority = Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
            membership.clone(),
        ));
        let services = qos_action_broker::services::HostServices {
            legacy_workspace: Some(authority.clone()),
            ..Default::default()
        };
        LocalDirectoryFixture {
            inner: qos_action_broker::collab_broker_capnp::tests::local_directory_fixture_with_authority(authority, services, peer, None),
            chrome: crate::shell::chrome_state::ChromeState::new_with_handle(membership),
        }
    }
    pub async fn drain_directory_bridge(fixture: &LocalDirectoryFixture) {
        qos_action_broker::collab_broker_capnp::tests::drain_directory_bridge(&fixture.inner).await;
    }
    fn query_live_directory(
        provider: &LiveAppActionsProvider,
        chrome: &crate::shell::chrome_state::Chrome,
        doc: &AutoCommit,
        root: &ObjId,
    ) -> Vec<crate::shell::search::SearchResult> {
        let chrome = chrome.borrow();
        provider.query(SearchContext {
            needle: "open",
            wanted: &[],
            chrome: &chrome,
            doc,
            doc_root: root,
        })
    }

    fn only_directory_result(
        results: Vec<crate::shell::search::SearchResult>,
    ) -> crate::shell::search::SearchResult {
        results
            .into_iter()
            .find(|result| result.id.starts_with("directory\u{1f}"))
            .expect("one directory action result")
    }

    #[allow(
        clippy::future_not_send,
        reason = "the test helper awaits only LocalSet-bound directory work holding Rc-backed Cap'n Proto state"
    )]
    async fn load_directory_result(
        fixture: &LocalDirectoryFixture,
        provider: &LiveAppActionsProvider,
        doc: &AutoCommit,
        root: &ObjId,
    ) -> crate::shell::search::SearchResult {
        let initial = query_live_directory(provider, &fixture.chrome, doc, root);
        assert!(
            initial
                .iter()
                .all(|result| !result.id.starts_with("directory\u{1f}")),
            "first provider query must not synchronously fabricate a directory row"
        );
        drain_directory_bridge(fixture).await;
        only_directory_result(query_live_directory(provider, &fixture.chrome, doc, root))
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one LocalSet lifecycle test deliberately keeps every queue/cache transition in temporal order"
    )]
    fn localset_directory_queue_drives_pending_start_refusal_actor_cache_and_invalidation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread test runtime");
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let mut fixture = local_directory_fixture();
            let queue = fixture.broker.action_directory_request_queue();
            let room = crate::shell::state::RoomId::parse(&fixture.cluster_id)
                .expect("fixture route is a room id");
            let directory_actor = fixture.chrome.borrow().local_user_for_room(&room);

            // A request remains pending until the LocalSet drains it; no shell
            // caller is allowed to block the compositor waiting for an RPC.
            fixture.set_confirmation_required(true);
            let mut confirmation = queue
                .try_start_resolved(
                    BrokerActor::person(&directory_actor),
                    &fixture.cluster_id,
                    fixture.peer_id,
                    "directory-r1",
                    "workspace:atlas",
                    "open",
                    None,
                    None,
                )
                .expect("bounded queue accepts one request");
            assert!(matches!(
                confirmation.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            drain_directory_bridge(&fixture).await;
            let confirmation_reply = tokio::time::timeout(
                Duration::from_millis(100),
                confirmation,
            )
            .await
            .expect("LocalSet reply deadline")
            .expect("reply sender survives");
            assert!(matches!(
                confirmation_reply,
                Err(ActionDirectoryBridgeError::ConfirmationRequired)
            ), "unexpected confirmation reply: {confirmation_reply:?}");
            fixture.set_confirmation_required(false);

            let provider = LiveAppActionsProvider::new(fixture.broker.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT.clone();
            let _same_room_actor = load_directory_result(&fixture, &provider, &doc, &root).await;
            assert_eq!(fixture.query_count(), 1);

            // Re-querying the same root uses its room-derived actor and page;
            // changing `ChromeState::local_user_id` must not retarget it.
            fixture.chrome.borrow_mut().local_user_id = "unrelated-active-room-user".into();
            let room_actor_result = only_directory_result(query_live_directory(
                &provider,
                &fixture.chrome,
                &doc,
                &root,
            ));
            assert_eq!(fixture.query_count(), 1);

            let begin = room_actor_result
                .on_begin_invoke
                .as_ref()
                .expect("directory result starts through an atomic broker ticket");
            let InvocationDisposition::Pending(ticket) = begin(
                &mut doc,
                &root,
                &fixture.chrome,
                &qos_actions::ActionArgs::none(),
            )
            .expect("directory invocation request enqueues") else {
                panic!("directory action must not claim accepted before LocalSet admission");
            };
            assert!(ticket().expect("pending ticket").is_none());
            drain_directory_bridge(&fixture).await;
            let accepted = ticket()
                .expect("accepted ticket")
                .expect("LocalSet accepted directory action");
            assert!(accepted.broker_invocation_id.is_some());
            assert_eq!(
                fixture.take_invocation_actor().expect("admitted invocation reaches application stream"),
                directory_actor
            );

            // Stale and revoked exact resolutions both invalidate the page
            // captured by the provider, then force a fresh bounded query.
            fixture.set_resolve_mode(DirectoryResolveMode::Stale);
            let stale = room_actor_result
                .on_begin_invoke
                .as_ref()
                .expect("directory ticket")(
                &mut doc,
                &root,
                &fixture.chrome,
                &qos_actions::ActionArgs::none(),
            )
            .expect("stale request queues");
            let InvocationDisposition::Pending(stale) = stale else {
                panic!("stale action must be asynchronous");
            };
            drain_directory_bridge(&fixture).await;
            assert!(stale().is_err(), "stale exact resolution refuses the cached row");
            fixture.set_resolve_mode(DirectoryResolveMode::Found);
            let _refreshed_after_stale = load_directory_result(&fixture, &provider, &doc, &root).await;
            assert_eq!(fixture.query_count(), 2);

            fixture.set_resolve_mode(DirectoryResolveMode::NotFound);
            let revoked_source = only_directory_result(query_live_directory(
                &provider,
                &fixture.chrome,
                &doc,
                &root,
            ));
            let InvocationDisposition::Pending(revoked) = revoked_source
                .on_begin_invoke
                .as_ref()
                .expect("directory ticket")(
                &mut doc,
                &root,
                &fixture.chrome,
                &qos_actions::ActionArgs::none(),
            )
            .expect("revoked request queues") else {
                panic!("revoked action must be asynchronous");
            };
            drain_directory_bridge(&fixture).await;
            assert!(revoked().is_err(), "revoked exact resolution refuses the cached row");
            fixture.set_resolve_mode(DirectoryResolveMode::Found);
            let disconnected_source = load_directory_result(&fixture, &provider, &doc, &root).await;
            assert_eq!(fixture.query_count(), 3);

            fixture.disconnect_directory();
            let InvocationDisposition::Pending(disconnected) = disconnected_source
                .on_begin_invoke
                .as_ref()
                .expect("directory ticket")(
                &mut doc,
                &root,
                &fixture.chrome,
                &qos_actions::ActionArgs::none(),
            )
            .expect("disconnect request queues") else {
                panic!("disconnect action must be asynchronous");
            };
            drain_directory_bridge(&fixture).await;
            assert!(disconnected().is_err(), "disconnect invalidates the cached directory row");
            assert!(
                query_live_directory(&provider, &fixture.chrome, &doc, &root).is_empty(),
                "after disconnect the provider must not render its old cached row"
            );

            fixture.reconnect_directory();
            drain_directory_bridge(&fixture).await;
            let reconnected = query_live_directory(&provider, &fixture.chrome, &doc, &root);
            assert!(
                reconnected.iter().any(|result| result.id.starts_with("directory\u{1f}")),
                "a reconnected directory is reloaded through the queue, never revived from stale cache"
            );
        }));
    }
}
