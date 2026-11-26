// What one store command costs an app: the command an app's process sends over its
// collaboration connection, through the device's dispatch, to the workspace's real store and
// back. An app holds no store of its own, so an action that names many resources makes as many
// trips; a trip that costs milliseconds makes an import of a few hundred rows take seconds.

use crate::service_resources::ServiceResources;
use tilo_workspace_service::workspaces::fixtures;

/// How many commands the trip is measured over.
const ASKS: u32 = 100;

/// The median, 95th percentile and longest of `samples`, by nearest rank.
fn percentiles(
    mut samples: Vec<std::time::Duration>,
) -> (
    std::time::Duration,
    std::time::Duration,
    std::time::Duration,
) {
    samples.sort_unstable();
    let at = |percent: usize| {
        samples
            .get(samples.len().saturating_sub(1).saturating_mul(percent) / 100)
            .copied()
            .unwrap_or_default()
    };
    (at(50), at(95), samples.last().copied().unwrap_or_default())
}

#[allow(
    clippy::future_not_send,
    reason = "the connection tasks and its wire run on the test LocalSet"
)]
#[tokio::test(flavor = "current_thread")]
async fn a_store_command_round_trip_to_the_workspace_store_is_a_local_trip() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let directory = tempfile::tempdir().unwrap();
            let (key, cert) = fixtures::hosting_device();
            let server = fixtures::test_host(directory.path(), &key, &cert).await;
            let workspaces = server.workspaces();
            workspaces
                .offer_own_workspaces(tilo_workspace_service::workspaces::OwnWorkspaceOffer::free(
                    "tilo.test",
                ))
                .unwrap();
            let workspace = workspaces.create("Cost").await.unwrap().workspace;

            // The device's one store, as the daemon hands it to the app connections: the workspaces'
            // own, not a fixture that answers without a store at all.
            let fanout = Arc::new(FanoutState::new());
            let channels = Arc::new(Mutex::new(BrokerChannels::default()));
            let (owner, _mailbox) = crate::test_owner::writes::OwnerCommitMailbox::bounded(8);
            let publisher = PeerPublisher::with_commits(Arc::new(owner));
            let services = crate::services::ProcessServices {
                resources: Some(Arc::new(ServiceResources::of(server.host()))),
                ..Default::default()
            };
            let (state, bridge_rx) = AppBrokerState::new_configured(
                &fanout,
                channels,
                publisher,
                Some(Arc::new(workspaces.clone())),
                None,
                None,
                services,
            );
            state
                .attach_certificate_authority(Some(Arc::new(DeviceOf(cert.clone()))))
                .unwrap();
            let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
            let (client, _served, _carried) = certified_rpc(
                &state,
                &directories,
                state.next_peer_id(),
                cert.clone(),
                None,
            );

            let route = CrdtRoute::record("so.tilo.cost", workspace, "cost");
            let channel = client.record(&route.channel_key(), "");
            let command = serde_json::to_vec(&collab_types::store::wire::Command::Count {
                kind: "cost.item".to_owned(),
                filters: Vec::new(),
            })
            .unwrap();
            // The first command opens the record on the connection; the trip is measured over
            // the ones after it, which is what an action's loop makes.
            let opening = std::time::Instant::now();
            channel
                .resource_command(&command)
                .await
                .unwrap_or_else(|error| panic!("the store answered no command: {error}"));
            let first = opening.elapsed();
            let mut samples = Vec::new();
            for ask in 0..ASKS {
                let started = std::time::Instant::now();
                channel
                    .resource_command(&command)
                    .await
                    .unwrap_or_else(|error| panic!("ask {ask} was not answered: {error}"));
                samples.push(started.elapsed());
            }
            let (median, p95, longest) = percentiles(samples);
            eprintln!(
                "a store command through the daemon: first {first:?}, then median {median:?}, \
                 p95 {p95:?}, max {longest:?} over {ASKS} commands"
            );
            assert!(
                p95 < std::time::Duration::from_millis(2),
                "one store command over the connection has a p95 of {p95:?} (median {median:?}, \
                 max {longest:?}): a local wire answers in well under a millisecond, and an app \
                 makes one trip per resource its action touches"
            );
        }))
        .await;
}
