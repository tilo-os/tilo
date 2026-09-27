//! A provider-owned host can serve an explicitly enrolled cross-user room
//! without weakening per-workspace membership.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use tilo_relay::{Envelope, Relay, RelayClient, RelayConfig, TrustPolicy, WorkspaceBinding};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn provider_host_serves_only_current_cross_user_members() {
    // Phase `pinned_provider_host_serves_only_enrolled_cross_user_devices`.
    {
        let provider = TestUser::fresh("Tilo hosting");
        let alice = TestUser::fresh("Alice");
        let bob = TestUser::fresh("Bob");
        let charlie = TestUser::fresh("Charlie");
        let host = provider.enrol("host-1", DeviceCapabilities::host());
        let alice_device = alice.enrol("alice-laptop", DeviceCapabilities::primary());
        let bob_device = bob.enrol("bob-laptop", DeviceCapabilities::primary());
        // Even a host-capable tenant certificate is not federation authority and
        // cannot bypass this workspace's explicit membership.
        let charlie_device = charlie.enrol("charlie-host", DeviceCapabilities::host());
        let state = tempfile::tempdir().unwrap();
        let alice_key = alice_device.device_pubkey();
        let bob_key = bob_device.device_pubkey();
        let cfg = RelayConfig {
            state_root: state.path().to_path_buf(),
            trust_policy: TrustPolicy::AllowAny,
            workspaces: vec![WorkspaceBinding {
                id: 77,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: Vec::new(),
                shell_authority: None,
                checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
            }],
            ..RelayConfig::default()
        };
        let relay = Relay::start(
            cfg,
            tilo_relay::daemon::RelayIdentity {
                user_public_key: provider.user_pk,
                device_signing_key: host.sk,
                device_cert: host.cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        relay
            .set_workspace_membership(
                77,
                true,
                vec![alice_key, bob_key],
                state.path().to_path_buf(),
            )
            .await
            .unwrap();
        let addr = relay.node_addr();

        let alice_client = RelayClient::connect_to_pinned_host(
            client_endpoint().await,
            addr.clone(),
            alice_device.cert,
            alice_device.sk,
            RevocationList::new(),
        )
        .await
        .expect("Alice accepts the pinned provider host");
        let bob_client = RelayClient::connect_to_pinned_host(
            client_endpoint().await,
            addr.clone(),
            bob_device.cert,
            bob_device.sk,
            RevocationList::new(),
        )
        .await
        .expect("Bob accepts the same pinned provider host");
        for client in [&alice_client, &bob_client] {
            client
                .send(Envelope::Subscribe { workspace_id: 77 })
                .unwrap();
            assert!(matches!(
                tilo_test_env::within("a message on client", client.recv()).await,
                Some(Envelope::Ready {
                    workspace_id: 77,
                    ..
                })
            ));
        }
        alice_client
            .send(Envelope::Delta {
                workspace_id: 77,
                author_device_public_key: alice_key,
                authored_at_ns: 1,
                delta: b"shared".to_vec(),
                signature: [0; 64],
            })
            .unwrap();
        assert!(matches!(
        tilo_test_env::within("a message on bob_client", bob_client.recv()).await,
        Some(Envelope::Delta { author_device_public_key, .. }) if author_device_public_key == alice_key
            ));

        let charlie_client = RelayClient::connect_to_pinned_host(
            client_endpoint().await,
            addr,
            charlie_device.cert,
            charlie_device.sk,
            RevocationList::new(),
        )
        .await
        .expect("connection trust is separate from workspace membership");
        charlie_client
            .send(Envelope::Subscribe { workspace_id: 77 })
            .unwrap();
        assert!(matches!(
            tilo_test_env::within("a message on charlie_client", charlie_client.recv()).await,
            Some(Envelope::NotAuthorised {
                workspace_id: 77,
                ..
            })
        ));
    }
    // Phase `provider_membership_removal_closes_a_live_tenant_session`.
    {
        let provider = TestUser::fresh("Provider");
        let tenant = TestUser::fresh("Tenant");
        let host = provider.enrol("host", DeviceCapabilities::host());
        let device = tenant.enrol("laptop", DeviceCapabilities::primary());
        let state = tempfile::tempdir().unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                trust_policy: TrustPolicy::AllowAny,
                workspaces: vec![WorkspaceBinding {
                    id: 78,
                    pin: true,
                    replication: tilo_relay::ReplicationIntent::FullWorkspace,
                    admitted_users: Vec::new(),
                    shell_authority: None,
                    checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
                }],
                ..RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: provider.user_pk,
                device_signing_key: host.sk,
                device_cert: host.cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        relay
            .set_workspace_membership(
                78,
                true,
                vec![device.device_pubkey()],
                state.path().to_path_buf(),
            )
            .await
            .unwrap();
        let client = RelayClient::connect_to_pinned_host(
            client_endpoint().await,
            relay.node_addr(),
            device.cert,
            device.sk,
            RevocationList::new(),
        )
        .await
        .unwrap();
        client
            .send(Envelope::Subscribe { workspace_id: 78 })
            .unwrap();
        assert!(matches!(
            tilo_test_env::within("a message on client", client.recv()).await,
            Some(Envelope::Ready { .. })
        ));

        let store = tilo_relay::store::WorkspaceStore::open(state.path(), 78, true).unwrap();
        store.save_membership(&[]).unwrap();
        assert!(
            tilo_test_env::within("client to close", client.recv())
                .await
                .is_none()
        );
    }
}
