//! Shared production RPC broker. Native host adapters live in `broker_services`.
pub use tilo_action_broker::collab_broker_capnp::*;

mod local_consent;
#[cfg(test)]
#[cfg(unix)]
mod local_consent_tests;
mod realtime_host;
#[cfg(test)]
#[cfg(target_os = "linux")]
pub(crate) use local_consent::LocalSessionLiveness;
pub(crate) use local_consent::LocalSessionPermit;
pub(crate) use local_consent::{LocalSessionAuthority, LocalSessionLifecycle};
pub(crate) use realtime_host::NativeRealtimeServices;
pub use realtime_host::{
    RealtimeConsentUiView, RealtimeContinuityUiView, RealtimeFabricAppBridge, RealtimeSourceUiView,
};

#[cfg(test)]
const NATIVE_CONSENT_CHILD_TEST: &str = "TILO_NATIVE_CONSENT_CHILD_TEST";
#[cfg(test)]
const NATIVE_CONSENT_CHILD_PROOF: &str = "TILO_NATIVE_CONSENT_CHILD_PROOF";
#[cfg(test)]
const NATIVE_CONSENT_FIXTURE_ROOT: &str = "TILO_NATIVE_CONSENT_FIXTURE_ROOT";

#[cfg(test)]
pub(crate) fn run_isolated_native_consent_test(test_name: &str, assertions: impl FnOnce()) {
    use std::os::unix::fs::PermissionsExt as _;

    if let Some(marker) = std::env::var_os(NATIVE_CONSENT_CHILD_TEST) {
        assert_eq!(marker, test_name, "isolated child ran the wrong exact test");
        assertions();
        let proof = std::path::PathBuf::from(
            std::env::var_os(NATIVE_CONSENT_CHILD_PROOF).expect("isolated child proof path"),
        );
        std::fs::write(&proof, format!("{test_name}\nassertions-complete\n"))
            .expect("write isolated child proof");
    } else {
        let temp = tempfile::tempdir().expect("isolated native-consent root");
        let fixture_root = temp
            .path()
            .canonicalize()
            .expect("canonical isolated native-consent root");
        let environment = fixture_root.join("environment");
        let runtime = environment.join("runtime");
        std::fs::create_dir_all(&runtime).expect("create isolated runtime");
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
            .expect("secure isolated runtime");
        let proof = temp.path().join("assertions-complete");
        let mut child =
            std::process::Command::new(std::env::current_exe().expect("current test binary"));
        child
            .args(["--exact", test_name, "--nocapture"])
            .env(NATIVE_CONSENT_CHILD_TEST, test_name)
            .env(NATIVE_CONSENT_CHILD_PROOF, &proof)
            .env(NATIVE_CONSENT_FIXTURE_ROOT, &fixture_root)
            .env("HOME", environment.join("home"))
            .env("XDG_CONFIG_HOME", environment.join("config"))
            .env("XDG_DATA_HOME", environment.join("data"))
            .env("XDG_CACHE_HOME", environment.join("cache"))
            .env("XDG_STATE_HOME", environment.join("state"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("TILO_IDENTITY_ROOT", environment.join("identity"))
            .env("TILO_IDENTITY_ROOT", environment.join("identity"))
            .env("TILO_USER_ID", "environment-owner-must-not-select-identity")
            .env("TILO_USER_ID", "environment-owner-must-not-select-identity");
        let mut child = child.spawn().expect("spawn isolated native-consent test");
        let status = bounded_command::wait_or_kill(&mut child, tilo_test_env::hang_bound())
            .expect("wait for isolated native-consent test")
            .unwrap_or_else(|| panic!("isolated native-consent test timed out: {test_name}"));
        assert!(
            status.success(),
            "isolated native-consent child failed: {status}"
        );
        assert_eq!(
            std::fs::read_to_string(&proof).expect("isolated child completion proof"),
            format!("{test_name}\nassertions-complete\n"),
            "the exact child test did not complete its assertions"
        );
    }
}

#[cfg(test)]
pub(crate) fn native_consent_fixture_root() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os(NATIVE_CONSENT_FIXTURE_ROOT).expect("fixture root"))
}

#[cfg(test)]
pub(crate) fn wait_for_local_session_worker_teardown(lifecycle: &LocalSessionLifecycle) {
    let deadline = tilo_test_env::hang_deadline();
    while lifecycle.snapshot().is_some() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        lifecycle.snapshot().is_none(),
        "weak lifecycle must stop upgrading after broker teardown"
    );
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::shell::search::providers::live_app_actions::LiveAppActionsProvider;
    use crate::shell::search::{InvocationDisposition, SearchContext, SearchProvider};
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    use automerge::{AutoCommit, ObjId};
    use std::sync::Arc;
    pub use tilo_action_broker::collab_broker_capnp::tests::DirectoryResolveMode;
    pub struct LocalDirectoryFixture {
        inner: tilo_action_broker::collab_broker_capnp::tests::LocalDirectoryFixture,
        chrome: crate::shell::chrome_state::Chrome,
    }
    impl std::ops::Deref for LocalDirectoryFixture {
        type Target = tilo_action_broker::collab_broker_capnp::tests::LocalDirectoryFixture;
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
        let mut workspace = Workspace::new(WorkspaceId(71), "Directory test room");
        workspace.set_owner(crate::shell::state::local_user_id_for_room(&workspace.room));
        tree.desktop.workspaces.push(workspace);
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
        use ed25519_dalek::{Signer as _, SigningKey};

        let root = SigningKey::from_bytes(&[211; 32]);
        let device = SigningKey::from_bytes(&[212; 32]);
        let person = principal_id_for_root(root.verifying_key().to_bytes());
        let workspace_id = {
            let mut tree = membership.write();
            let workspace = tree.active_workspace_mut().expect("fixture active room");
            // The publisher is an ordinary editor, not the room's owner.
            workspace.admit(person, crate::shell::state::CapRole::Editor);
            workspace.room.to_string()
        };
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Directory fixture device".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root).expect("signed device");
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace_id.clone(),
            device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
            proof: device
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &workspace_id,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .expect("certified publisher presentation");
        let authority = Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
            membership.clone(),
        ));
        let services = tilo_action_broker::services::HostServices {
            legacy_workspace: Some(authority.clone()),
            ..Default::default()
        };
        let mut inner =
            tilo_action_broker::collab_broker_capnp::tests::local_directory_fixture_with_authority(
                authority,
                services,
                peer,
                Some(Arc::new(vec![device.verifying_key().to_bytes()])),
            );
        assert_eq!(
            inner
                .authenticate_source(&presentation)
                .expect("admitted certified publisher"),
            crate::shell::state::CapRole::Editor,
            "production admission retains the publisher's actual room role"
        );
        LocalDirectoryFixture {
            inner,
            chrome: crate::shell::chrome_state::ChromeState::new_with_handle(membership),
        }
    }
    #[allow(
        clippy::future_not_send,
        reason = "the directory bridge is deliberately driven on the LocalSet because its Cap'n Proto state contains Rc values"
    )]
    pub async fn drain_directory_bridge(fixture: &LocalDirectoryFixture) {
        tilo_action_broker::collab_broker_capnp::tests::drain_directory_bridge(&fixture.inner)
            .await;
    }
    fn query_live_directory(
        provider: &LiveAppActionsProvider,
        chrome: &crate::shell::chrome_state::Chrome,
        doc: &AutoCommit,
        root: &ObjId,
    ) -> Vec<crate::shell::search::SearchResult> {
        let chrome = chrome.borrow();
        provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
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
                    DirectoryActionTarget {
                        cluster_id: fixture.cluster_id.clone(),
                        peer_id: fixture.peer_id,
                        revision: "directory-r1".into(),
                        address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                    },
                    None,
                    None,
                )
                .expect("bounded queue accepts one request");
            assert!(matches!(
                confirmation.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            drain_directory_bridge(&fixture).await;
            let confirmation_reply = tilo_test_env::within("LocalSet reply deadline", confirmation).await
            .expect("reply sender survives");
            assert!(matches!(
                confirmation_reply,
                Err(ActionDirectoryBridgeError::ConfirmationRequired)
            ), "unexpected confirmation reply: {confirmation_reply:?}");
            fixture.set_confirmation_required(false);

            let provider = LiveAppActionsProvider::new_with_fixture_identity(fixture.broker.clone());
            let mut doc = AutoCommit::new();
            let doc_root = automerge::ROOT.clone();
            let _same_room_actor = load_directory_result(&fixture, &provider, &doc, &doc_root).await;
            assert_eq!(fixture.query_count(), 1);

            // Re-querying the same root uses its room-derived actor and page;
            // changing `ChromeState::local_user_id` must not retarget it.
            fixture.chrome.borrow_mut().local_user_id = "unrelated-active-room-user".into();
            let room_actor_result = only_directory_result(query_live_directory(
                &provider,
                &fixture.chrome,
                &doc,
                &doc_root,
            ));
            assert_eq!(fixture.query_count(), 1);

            let begin = room_actor_result
                .on_begin_invoke
                .as_ref()
                .expect("directory result starts through an atomic broker ticket");
            let InvocationDisposition::Pending(ticket) = begin(
                &mut ui::widget::Edits::lend(&mut doc),
                &doc_root,
                &fixture.chrome,
                &tilo_actions::ActionArgs::none(),
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
                &mut ui::widget::Edits::lend(&mut doc),
                &doc_root,
                &fixture.chrome,
                &tilo_actions::ActionArgs::none(),
            )
            .expect("stale request queues");
            let InvocationDisposition::Pending(stale) = stale else {
                panic!("stale action must be asynchronous");
            };
            drain_directory_bridge(&fixture).await;
            assert!(stale().is_err(), "stale exact resolution refuses the cached row");
            fixture.set_resolve_mode(DirectoryResolveMode::Found);
            let _refreshed_after_stale = load_directory_result(&fixture, &provider, &doc, &doc_root).await;
            assert_eq!(fixture.query_count(), 2);

            fixture.set_resolve_mode(DirectoryResolveMode::NotFound);
            let revoked_source = only_directory_result(query_live_directory(
                &provider,
                &fixture.chrome,
                &doc,
                &doc_root,
            ));
            let InvocationDisposition::Pending(revoked) = revoked_source
                .on_begin_invoke
                .as_ref()
                .expect("directory ticket")(
                &mut ui::widget::Edits::lend(&mut doc),
                &doc_root,
                &fixture.chrome,
                &tilo_actions::ActionArgs::none(),
            )
            .expect("revoked request queues") else {
                panic!("revoked action must be asynchronous");
            };
            drain_directory_bridge(&fixture).await;
            assert!(revoked().is_err(), "revoked exact resolution refuses the cached row");
            fixture.set_resolve_mode(DirectoryResolveMode::Found);
            let disconnected_source = load_directory_result(&fixture, &provider, &doc, &doc_root).await;
            assert_eq!(fixture.query_count(), 3);

            fixture.disconnect_directory();
            let InvocationDisposition::Pending(disconnected) = disconnected_source
                .on_begin_invoke
                .as_ref()
                .expect("directory ticket")(
                &mut ui::widget::Edits::lend(&mut doc),
                &doc_root,
                &fixture.chrome,
                &tilo_actions::ActionArgs::none(),
            )
            .expect("disconnect request queues") else {
                panic!("disconnect action must be asynchronous");
            };
            drain_directory_bridge(&fixture).await;
            assert!(disconnected().is_err(), "disconnect invalidates the cached directory row");
            assert!(
                query_live_directory(&provider, &fixture.chrome, &doc, &doc_root).is_empty(),
                "after disconnect the provider must not render its old cached row"
            );

            fixture.reconnect_directory();
            drain_directory_bridge(&fixture).await;
            let reconnected = query_live_directory(&provider, &fixture.chrome, &doc, &doc_root);
            drop(provider);
            drop(fixture);
            assert!(
                reconnected.iter().any(|result| result.id.starts_with("directory\u{1f}")),
                "a reconnected directory is reloaded through the queue, never revived from stale cache"
            );
        }));
    }
}
