//! The host's authenticated native relay loop. No renderer is constructed.
use super::*;
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use qos_relay::daemon::RelayIdentity;
use qos_relay::{
    Relay, RelayClient, RelayConfig, WorkspaceBinding,
    client::CertifiedActionEvent,
    coedit::{Inbound, RelayCoeditSession},
    transport_v2::{ActionCapability, EnvelopeV2},
};
use std::time::Duration;
const STARTUP_ACTION_BUFFER: usize = 32;
mod delivery;

#[derive(Clone, Copy)]
enum ExpectedActionReady {
    Transport { room: uuid::Uuid },
    Broker { room: uuid::Uuid, host: uuid::Uuid },
}

fn retain_startup_action(
    event: CertifiedActionEvent,
    expected: ExpectedActionReady,
    pending: &mut std::collections::VecDeque<CertifiedActionEvent>,
) -> Result<bool> {
    let expected_ack = match (&event, expected) {
        (
            CertifiedActionEvent::Control(EnvelopeV2::ActionsReady {
                room,
                capability: ActionCapability::TransportOnly,
            }),
            ExpectedActionReady::Transport { room: expected },
        ) => *room == expected,
        (
            CertifiedActionEvent::Control(EnvelopeV2::ActionsReady {
                room,
                capability: ActionCapability::DurableBroker { host },
            }),
            ExpectedActionReady::Broker {
                room: expected_room,
                host: expected_host,
            },
        ) => *room == expected_room && *host == expected_host,
        _ => false,
    };
    if expected_ack {
        return Ok(true);
    }
    if matches!(
        event,
        CertifiedActionEvent::Control(EnvelopeV2::ActionsReady { .. })
    ) {
        bail!("cloud collaboration broker registration was not acknowledged")
    }
    if pending.len() >= STARTUP_ACTION_BUFFER {
        bail!("relay action traffic exceeded the startup buffer")
    }
    pending.push_back(event);
    Ok(false)
}

async fn await_action_ready(
    session: &RelayCoeditSession,
    expected: ExpectedActionReady,
    pending: &mut std::collections::VecDeque<CertifiedActionEvent>,
    phase: &'static str,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = session
                .recv_certified_action()
                .await
                .ok_or_else(|| anyhow!("relay closed during {phase}"))?;
            if retain_startup_action(event, expected, pending)? {
                return Ok(());
            }
        }
    })
    .await
    .with_context(|| format!("relay {phase} acknowledgement timed out"))?
}

fn relay_config(root: &Path, bound: &Bound, room: &str, workspace: u64) -> Result<RelayConfig> {
    Ok(RelayConfig {
        state_root: root.join("relay"),
        identity_root: root.join("identity"),
        full_replica_status: root.join("full-replica/status.json"),
        home_peer_bundle: root.join("home-peer.toml"),
        trust_policy: qos_relay::TrustPolicy::AllowList {
            users: vec![hex::encode(bound.root)],
        },
        workspaces: vec![WorkspaceBinding {
            id: workspace,
            pin: true,
            replication: qos_relay::ReplicationIntent::FullWorkspace,
            admitted_users: vec![],
            shell_authority: Some(
                qos_peer_state::retained_proof::RetentionAuthority::new(
                    room,
                    bound.authority.clone(),
                )
                .map_err(anyhow::Error::msg)?,
            ),
            checkpoint_prefix_recovery:
                qos_relay::CheckpointPrefixRecovery::NativeOwnerDurableBaseline,
        }],
        ..Default::default()
    })
}

/// Prepare explicit Cloud HTTPS from trusted operator configuration. A missing
/// bounded owner or mandatory handler fails before Store mutation or readiness.
pub async fn serve_https(config: &Path, ready_file: &Path) -> Result<()> {
    crate::https_owner::serve::serve(config, ready_file).await
}

/// Serve one persistent workspace; enrollment transactions synchronize via the state lock.
pub async fn serve(root: &Path, machine: &str) -> Result<()> {
    let packet: iroh_base::RelayUrl = std::env::var("QOS_CLOUD_PACKET_RELAY")
        .context("QOS_CLOUD_PACKET_RELAY required")?
        .parse()?;
    if packet.scheme() != "https" {
        bail!("production packet relay must use HTTPS")
    }
    let mode = RelayMode::Custom(RelayMap::from(packet));
    let served = serve_with_mode(
        root,
        machine,
        mode,
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
        None,
    )
    .await;
    // Orderly shutdown folds the Store journal into the checkpoint.
    #[cfg(unix)]
    if let Err(error) = crate::storage::compact(root) {
        tracing::warn!(%error, "Cloud state journal compaction deferred");
    }
    served
}

async fn serve_with_mode<F>(
    root: &Path,
    machine: &str,
    mode: RelayMode,
    shutdown: F,
    ready: Option<tokio::sync::oneshot::Sender<iroh_base::EndpointAddr>>,
) -> Result<()>
where
    F: std::future::Future<Output = ()>,
{
    #[cfg(test)]
    {
        serve_with_mode_inner(root, machine, mode, shutdown, ready, None).await
    }
    #[cfg(not(test))]
    {
        serve_with_mode_inner(root, machine, mode, shutdown, ready).await
    }
}

/// Actual Source custody supplied only by a real enrolled-Store test fixture.
/// No factory, boolean qualifier or fallback after source-qualified startup error.
#[cfg(test)]
struct AgentTestSource {
    clock: qos_action_broker::peer_actions::OwnerPolicyClock,
    time: qos_action_broker::peer_actions::PolicyTime,
    source: crate::https_owner::CloudStoreSource,
}

#[cfg(test)]
async fn serve_with_test_source<F>(
    root: &Path,
    machine: &str,
    mode: RelayMode,
    shutdown: F,
    ready: Option<tokio::sync::oneshot::Sender<iroh_base::EndpointAddr>>,
    source: AgentTestSource,
) -> Result<()>
where
    F: std::future::Future<Output = ()>,
{
    serve_with_mode_inner(root, machine, mode, shutdown, ready, Some(source)).await
}

async fn serve_with_mode_inner<F>(
    root: &Path,
    machine: &str,
    mode: RelayMode,
    shutdown: F,
    ready: Option<tokio::sync::oneshot::Sender<iroh_base::EndpointAddr>>,
    #[cfg(test)] test_source: Option<AgentTestSource>,
) -> Result<()>
where
    F: std::future::Future<Output = ()>,
{
    tokio::pin!(shutdown);
    std::fs::create_dir_all(root)?;
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("daemon.lock"))?;
    lease
        .try_lock()
        .context("another cloud host is already serving this state directory")?;
    let (bound, key, room) = loop {
        let store = Store::open(root, machine)?;
        if let Some(b) = &store.state.bound {
            break (b.clone(), store.key(), store.state.room.clone());
        }
        drop(store);
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    let workspace = qos_relay::room_workspace_id(&room)?;
    let config = relay_config(root, &bound, &room, workspace)?;
    let (access_updates, mut access_update_requests) = tokio::sync::mpsc::channel(64);
    let join_service =
        crate::collaboration::CloudJoinService::new(root, machine, access_updates.clone());
    let relay = Relay::start_with_relay_mode_and_protocol(
        config,
        RelayIdentity {
            user_public_key: bound.root,
            device_signing_key: key.clone(),
            device_cert: bound.authority.clone(),
            revocations: revocations(&bound.revocations)?,
        },
        mode.clone(),
        qos_relay::workspace_join::WORKSPACE_JOIN_ALPN,
        qos_relay::workspace_join::WorkspaceJoinProtocol::new(join_service),
    )
    .await?;
    let relay_result: Result<()> = async {
        let mut last_access = Store::open(root, machine)?
            .peer()?
            .governed_access(web_time::SystemTime::now())
            .map_err(anyhow::Error::msg)?;
        relay
            .replace_governed_workspace_access(workspace, true, last_access.clone(), root.join("relay"))
            .await?;
        if !matches!(mode, RelayMode::Disabled) {
            tokio::time::timeout(Duration::from_secs(45), relay.endpoint.online())
                .await
                .context("relay offline")?;
        }
        let action_router = crate::action_server::start_with_access_updates(
            root,
            machine,
            mode.clone(),
            access_updates.clone(),
        )
        .await?;
        let owner_result: Result<()> = async {
            {
                let mut store = Store::open(root, machine)?;
                let (before, mut peer) = store.peer_transition_at(qos_relay::clock::now_ns())?;
                let mut state = store.state.clone();
                let messages = durable_bootstrap_messages(
                    &mut peer, &store.key(), web_time::SystemTime::now(), store.next_generation()?,
                )?;
                state
                    .outbox
                    .extend(live_projection_from_bootstrap(&before, &peer, &messages)?);
                state.outbox.extend(baseline_publications(messages)?);
                state
                    .bound
                    .as_mut()
                    .ok_or_else(|| anyhow!("workspace missing"))?
                    .peer = peer.durable().map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
                store.commit(state)?;
            }
            let endpoint = Endpoint::builder(presets::Minimal)
                .relay_mode(mode)
                .bind()
                .await?;
            let client = RelayClient::connect_v2_to_enrolled_home_peer(
                endpoint,
                relay.node_addr(),
                bound.root,
                bound.authority.device_public_key,
                bound.authority.clone(),
                key,
                revocations(&bound.revocations)?,
            )
            .await?;
            let session = RelayCoeditSession::join(client, workspace, bound.authority.device_public_key)?;
            let session_result: Result<()> = async {
                let action_room: uuid::Uuid = room.parse()?;
                let broker_host = action_room;
                let mut pending_actions = std::collections::VecDeque::new();
                session.enable_actions(action_room)?;
                await_action_ready(
                    &session,
                    ExpectedActionReady::Transport { room: action_room },
                    &mut pending_actions,
                    "action enable",
                )
                .await?;
                session.register_action_broker(action_room, broker_host)?;
                await_action_ready(
                    &session,
                    ExpectedActionReady::Broker {
                        room: action_room,
                        host: broker_host,
                    },
                    &mut pending_actions,
                    "broker registration",
                )
                .await?;
                tracing::info!(
                    event = "collaboration_broker_ready",
                    room = %action_room,
                    host = %broker_host,
                    device = %hex::encode(bound.authority.device_public_key),
                    protocol = "tilo-relay/2",
                    result = "ready",
                );
                #[cfg(not(test))]
                let mut agents = crate::agent_runtime::CloudAgentRuntime::start(root, machine, session.supports_agent_reads_v1()).await.map_err(anyhow::Error::msg)?;
                #[cfg(test)]
                let mut agents = match test_source {
                    Some(source) => crate::agent_runtime::CloudAgentRuntime::start_with_policy_time(
                        root, machine, session.supports_agent_reads_v1(), source.clock, source.time, source.source,
                    ).await,
                    None => crate::agent_runtime::CloudAgentRuntime::start(root, machine, session.supports_agent_reads_v1()).await,
                }.map_err(anyhow::Error::msg)?;
                let runtime_result: Result<()> = async {
                if let Some(ready) = ready {
                    let _ = ready.send(relay.node_addr());
                }
                let mut tick = tokio::time::interval(Duration::from_millis(250));
                loop {
                    tokio::select! {
                     _=&mut shutdown=>break,
                     outbound=agents.outbound.recv()=>match outbound {
                        Some(message @ (NeighbourMsg::AppPresenceV2 { .. } | NeighbourMsg::ScrollbackRequest { .. }))=>session.publish_presence(postcard::to_stdvec(&message)?)?,
                        Some(_)=>bail!("worker bypassed the durable document outbox"),
                        None=>bail!("agent runtime owner stopped"),
                     },
                     update=access_update_requests.recv()=>if let Some(update)=update {
                        // The queue is only a reconciliation signal. Always read the newest
                        // durable projection so an older admission cannot re-add a member
                        // removed while the request waited.
                        let access=Store::open(root,machine)
                            .and_then(|store| store.peer()?.governed_access(web_time::SystemTime::now()).map_err(anyhow::Error::msg));
                        let result=match access {
                            Ok(access)=>{
                                let result=relay.replace_governed_workspace_access(workspace,true,access.clone(),root.join("relay")).await.map_err(|error|error.to_string());
                                if result.is_ok(){last_access=access;}
                                result
                            },
                            Err(error)=>{
                                let _=update.completion.send(Err(error.to_string()));
                                return Err(error);
                            },
                        };
                        let _=update.completion.send(result);
                     },
                     _=tick.tick()=>{
                      let (access,outbox)={let mut store=Store::open(root,machine)?;
                        let outbox=delivery::RelayTickBatch::prepare(&mut store)?;
                        let access=store.peer()?.governed_access(web_time::SystemTime::now()).map_err(anyhow::Error::msg)?;
                        (access,outbox)};
                      // Never hold the cross-process state lock across a network await.
                      if access!=last_access{
                        relay.replace_governed_workspace_access(workspace,true,access.clone(),root.join("relay")).await?;
                        last_access=access;
                      }
                      let sent=outbox.publish(|publication|publish(&session,publication))?;
                      if sent>0{
                        let mut store=Store::open(root,machine)?;
                        outbox.retire(&mut store,sent)?;
                      }
                     },
                     incoming=session.recv()=>match incoming {
                      None=>bail!("native relay connection closed"),
                      Some(Inbound::Ready{..})=>{},
                      Some(incoming)=>{
                       let (origin,body)=match incoming {Inbound::Delta{author,data}=>(TransportOrigin::RelayDelta(author),data),Inbound::Presence{author,payload}=>(TransportOrigin::UntrustedPresence(author),payload),_=>continue};
                       if body.len()>8*1024*1024 {continue}
                       let mut store=Store::open(root,machine)?;
                       if let Ok(message)=postcard::from_bytes::<NeighbourMsg>(&body){
                        // Rejection of untrusted input is scoped to that message, never process termination.
                        match store.receive(origin,message,qos_relay::clock::now_ns()) {
                            Ok(()) => {},
                            Err(crate::ReceiveError::Rejected(error)) => tracing::debug!(%error, "state message rejected"),
                            Err(crate::ReceiveError::Owner(error)) => return Err(error),
                        }
                       }
                      }
                     },
                     action=async {
                        if let Some(action)=pending_actions.pop_front(){Some(action)}else{session.recv_certified_action().await}
                     }=>match action {
                      Some(CertifiedActionEvent::Delivered(evidence))=>{
                       let response=match agents.handle_certified(&evidence).await {
                          Ok(Some(response))=>response,
                          Ok(None)=>crate::broker::handle(root,machine,evidence.certificate(),evidence.action(),qos_relay::clock::now_ns()).await?,
                          Err(error)=>{tracing::debug!(%error,"private agent action rejected or observation unavailable");qos_relay::peer_actions::ActionMessage::Rejected {code:"agent_control_unavailable".into()}},
                       };
                       // A successful membership mutation is not observable until the relay
                       // has replaced its live exact-device access from durable Store state.
                       let access=Store::open(root,machine)?.peer()?.governed_access(web_time::SystemTime::now()).map_err(anyhow::Error::msg)?;
                       if access!=last_access {
                         relay.replace_governed_workspace_access(workspace,true,access.clone(),root.join("relay")).await?;
                         last_access=access;
                       }
                       let reply=crate::broker::reply(evidence.action(),response);
                       if !reply.validate(){bail!("cloud collaboration broker produced an invalid response")}
                       session.send_action(reply)?;
                      },
                      Some(CertifiedActionEvent::Control(EnvelopeV2::ActionsReady{room,capability:ActionCapability::DurableBroker{host}}))
                        if room==action_room && host==broker_host=>{},
                      Some(CertifiedActionEvent::Control(EnvelopeV2::ActionsReady{..}))=>
                        bail!("cloud collaboration broker registration was not acknowledged"),
                      Some(CertifiedActionEvent::Control(_))=>{},
                      None=>bail!("cloud collaboration broker connection closed"),
                     }
                    }
                }
                Ok(())
                }.await;
                let agent_close = agents.close().await.map_err(anyhow::Error::msg);
                runtime_result.and(agent_close)
            }.await;
            session.close().await;
            session_result
        }.await;
        let shutdown_result = action_router.shutdown().await;
        owner_result.and(shutdown_result.map_err(anyhow::Error::from))
    }.await;
    relay.shutdown().await;
    relay_result
}
fn publish(session: &RelayCoeditSession, publication: &StoredPublication) -> Result<()> {
    let body = postcard::to_stdvec(&publication.message)?;
    match publication.kind {
        PublicationKind::Checkpoint => {
            let (channel, _) = qos_peer_state::transport::retention(&publication.message, true)
                .ok_or_else(|| anyhow!("checkpoint publication has no retained channel"))?;
            session.publish_checkpoint(channel, body)?
        }
        PublicationKind::Delta => {
            let (channel, _) = qos_peer_state::transport::retention(&publication.message, false)
                .ok_or_else(|| anyhow!("delta publication has no retained channel"))?;
            session.publish_retained_delta(channel, body)?
        }
        PublicationKind::Presence => session.publish_presence(body)?,
    }
    Ok(())
}

// Helpers keep the durable outbox authoritative while transport queues are bounded.
fn checkpoint_outbox(messages: &[StoredPublication]) -> Vec<StoredPublication> {
    // Each retained checkpoint is a full authoritative snapshot. Keep its newest
    // version, preserving order and every incremental/non-checkpoint message.
    // The matching latest governance update remains ahead of its app snapshots.
    let mut latest = std::collections::BTreeMap::new();
    for (index, publication) in messages.iter().enumerate() {
        if publication.kind == PublicationKind::Checkpoint {
            if let Some((channel, _)) =
                qos_peer_state::transport::retention(&publication.message, true)
            {
                latest.insert(channel, index);
            }
        }
    }
    messages
        .iter()
        .enumerate()
        .filter(|(index, publication)| match publication.kind {
            PublicationKind::Checkpoint => {
                qos_peer_state::transport::retention(&publication.message, true)
                    .is_none_or(|(channel, _)| latest.get(&channel) == Some(index))
            }
            PublicationKind::Delta | PublicationKind::Presence => true,
        })
        .map(|(_, publication)| publication.clone())
        .collect()
}
fn publish_batch(
    messages: &[StoredPublication],
    mut send: impl FnMut(&StoredPublication) -> Result<()>,
) -> Result<usize> {
    let mut sent = 0;
    for message in messages.iter().take(16) {
        match send(message) {
            Ok(()) => sent += 1,
            // RelayClient currently uses one error for Full and Closed. Retain
            // unsent bytes and retry on the next tick; an actually closed stream
            // is handled by the receive branch. Never treat queue pressure as
            // a reason to restart the host and disconnect every browser.
            Err(error)
                if matches!(
                    error.downcast_ref::<qos_relay::coedit::CoeditError>(),
                    Some(qos_relay::coedit::CoeditError::Disconnected)
                ) =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(sent)
}
#[cfg(test)]
mod publication_tests {
    use super::*;

    async fn connected_session(
        relay: &Relay,
        root: [u8; 32],
        home: [u8; 32],
        cert: DeviceCert,
        key: SigningKey,
        workspace: u64,
    ) -> RelayCoeditSession {
        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let client = RelayClient::connect_v2_to_enrolled_home_peer(
            endpoint,
            relay.node_addr(),
            root,
            home,
            cert.clone(),
            key,
            RevocationList::new(),
        )
        .await
        .unwrap();
        let session = RelayCoeditSession::join(client, workspace, cert.device_public_key).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(session.recv().await, Some(Inbound::Ready { .. })) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        session
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enrolled_daemon_registers_durable_broker_before_serving() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("tilo_relay=debug,qos_cloud_host=info")
            .with_test_writer()
            .try_init();
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[81; 32]);
        let device = SigningKey::from_bytes(&[82; 32]);
        let mut store = Store::open(dir.path(), "runtime-startup").unwrap();
        let enrollment = crate::tests::approved(store.inspect(10).unwrap(), &root, &device);
        store.apply(&enrollment, 11).unwrap();
        let mut peer = store.peer().unwrap();
        peer.select_admission_v5().unwrap();
        let mut state = store.state.clone();
        state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
        store.commit(state).unwrap();
        let control = uuid::Uuid::new_v4();
        let caller = store
            .agent_caller(device.verifying_key().to_bytes())
            .unwrap();
        store.activate_private_agent_ledger(&caller).unwrap();
        let (caller, request) = crate::agent_owner_tests::prepare(
            &store,
            device.verifying_key().to_bytes(),
            &control.to_string(),
        );
        store.commit_agent_request(&request, &caller).unwrap();
        let caller = store
            .agent_caller(device.verifying_key().to_bytes())
            .unwrap();
        store.activate_private_agent_runtime_v2(&caller).unwrap();
        let person = caller.certificate().clone();
        let host = store.peer().unwrap().authority().device_public_key;
        let room: uuid::Uuid = store.state.room.parse().unwrap();
        let workspace = qos_relay::room_workspace_id(&store.state.room).unwrap();
        let person_root = root.verifying_key().to_bytes();
        drop(store);

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let root = dir.path().to_path_buf();
        let mut daemon = tokio::spawn(async move {
            let result = serve_with_mode(
                &root,
                "runtime-startup",
                RelayMode::Disabled,
                async move {
                    let _ = stop_rx.await;
                },
                Some(ready_tx),
            )
            .await;
            if let Err(error) = &result {
                eprintln!("actual daemon stopped: {error:#}");
            }
            result
        });

        let address = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                ready = ready_rx => ready.expect("daemon readiness channel closed"),
                exited = &mut daemon => panic!(
                    "daemon exited before authenticated relay-v2 broker registration: {exited:?}"
                ),
            }
        })
        .await
        .expect("daemon must finish authenticated relay-v2 broker registration");
        assert!(
            dir.path().join("actions/journal").exists(),
            "real runtime worker broker must be installed before serving"
        );
        // Real mutually authenticated relay delivery, never a constructed CertifiedAction.
        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let client = RelayClient::connect_v2_to_enrolled_home_peer(
            endpoint,
            address,
            person_root,
            host,
            person,
            device,
            RevocationList::new(),
        )
        .await
        .unwrap();
        let client = RelayCoeditSession::join(client, workspace, caller.device()).unwrap();
        let client = std::sync::Arc::new(client);
        let state_client = client.clone();
        let state_drain = tokio::spawn(async move { while state_client.recv().await.is_some() {} });
        assert!(client.supports_agent_reads_v1());
        client.enable_actions(room).unwrap();
        let mut pending = std::collections::VecDeque::new();
        await_action_ready(
            &client,
            ExpectedActionReady::Transport { room },
            &mut pending,
            "test client",
        )
        .await
        .unwrap();
        client.enable_broker_client(room).unwrap();
        async fn query(
            client: &RelayCoeditSession,
            room: uuid::Uuid,
            source: [u8; 32],
            host: [u8; 32],
            message: qos_relay::peer_actions::ActionMessage,
        ) -> qos_relay::peer_actions::ActionMessage {
            let request_id = uuid::Uuid::new_v4();
            client
                .send_action(qos_relay::peer_actions::DirectedAction {
                    room,
                    request_id,
                    source,
                    target: host,
                    message,
                })
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match client.recv_action().await {
                        Some(
                            EnvelopeV2::Delivered { action, .. }
                            | EnvelopeV2::CertifiedDelivered { action, .. },
                        ) if action.request_id == request_id => {
                            assert_eq!(action.source, host);
                            assert_eq!(action.target, source);
                            return action.message;
                        }
                        other => panic!("unexpected actual client action event: {other:?}"),
                    }
                }
            })
            .await
            .unwrap()
        }
        use qos_relay::peer_actions::ActionMessage as A;
        let catalogue = query(
            &client,
            room,
            caller.device(),
            host,
            A::CatalogueRefresh { scope: None },
        )
        .await;
        let A::BrokerCatalogue { catalogue, .. } = catalogue else {
            panic!("native catalogue missing: {catalogue:?}")
        };
        let binding: qos_relay::agent_reads::AgentReadBinding =
            serde_json::from_value(catalogue["agentReadsV1"].clone())
                .expect("Store-backed read provider must survive missing executable runtime");
        assert!(binding.spawn.is_none() && binding.message.is_none());
        for request in [
            qos_agent_control_protocol::AgentRequest::Capabilities { version: 1 },
            qos_agent_control_protocol::AgentRequest::List {
                version: 1,
                cursor: None,
            },
        ] {
            let response = query(
                &client,
                room,
                caller.device(),
                host,
                A::AgentReadV1 {
                    host: room,
                    invocation: uuid::Uuid::new_v4(),
                    request,
                },
            )
            .await;
            let A::AgentReadResultV1 { response, .. } = response else {
                panic!("canonical private read missing: {response:?}")
            };
            match response {
                qos_agent_control_protocol::AgentResponse::Capabilities {
                    capabilities, ..
                } => assert!(!capabilities.spawn && !capabilities.message && capabilities.list),
                qos_agent_control_protocol::AgentResponse::List { agents, .. } => {
                    assert_eq!(agents.len(), 1);
                    assert_eq!(agents[0].description, "secret task never published")
                }
                other => panic!("wrong private read: {other:?}"),
            }
        }
        for _ in 0..2 {
            let observed = query(
                &client,
                room,
                caller.device(),
                host,
                A::Status {
                    invocation: qos_actions::InvocationId(control),
                },
            )
            .await;
            assert!(
                matches!(
                    observed,
                    A::Observation {
                        uncertain: true,
                        update: None,
                        ..
                    }
                ),
                "legacy admission became success or lost retained status: {observed:?}"
            );
        }
        client.close().await;
        state_drain.await.unwrap();
        let store = Store::open(dir.path(), "runtime-startup").unwrap();
        let Some(qos_peer_state::owner_commit::PrivateAgentState::V2(ledger)) =
            &store.state.private_agent_ledger
        else {
            panic!("floor changed")
        };
        assert!(
            ledger.facts.is_empty(),
            "reads/status reconstructed execution"
        );
        drop(store);
        let _ = stop_tx.send(());
        tokio::time::timeout(Duration::from_secs(10), daemon)
            .await
            .expect("daemon must shut down its real endpoints")
            .expect("daemon task panicked")
            .expect("daemon startup or shutdown failed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_relay_delivers_signed_snapshot_as_live_retained_delta() {
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[74; 32]);
        let browser = SigningKey::from_bytes(&[75; 32]);
        let mut store = Store::open(dir.path(), "real-snapshot-delta").unwrap();
        let enrollment = crate::tests::approved(store.inspect(10).unwrap(), &root, &browser);
        store.apply(&enrollment, 11).unwrap();
        let bound = store.state.bound.as_ref().unwrap().clone();
        let workspace = qos_relay::room_workspace_id(&store.state.room).unwrap();
        let config = relay_config(dir.path(), &bound, &store.state.room, workspace).unwrap();
        let relay = Relay::start_with_relay_mode(
            config,
            RelayIdentity {
                user_public_key: bound.root,
                device_signing_key: store.key(),
                device_cert: bound.authority.clone(),
                revocations: RevocationList::new(),
            },
            RelayMode::Disabled,
        )
        .await
        .unwrap();
        relay
            .replace_governed_workspace_access(
                workspace,
                true,
                store
                    .peer_at(11)
                    .unwrap()
                    .governed_access(now_time(11))
                    .unwrap(),
                dir.path().join("relay"),
            )
            .await
            .unwrap();
        let publisher = connected_session(
            &relay,
            bound.root,
            bound.authority.device_public_key,
            bound.authority.clone(),
            store.key(),
            workspace,
        )
        .await;
        let observer = connected_session(
            &relay,
            bound.root,
            bound.authority.device_public_key,
            enrollment.browser_certificate.clone(),
            browser,
            workspace,
        )
        .await;
        let mut receiver = StatePeer::new(
            &store.state.room,
            bound.authority.clone(),
            vec![bound.authority.clone()],
            RevocationList::new(),
            now_time(12),
        )
        .unwrap();
        for publication in &store.state.outbox {
            publish(&publisher, publication).unwrap();
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while receiver.rows().len() < 2
                || receiver.rows().iter().any(|row| {
                    !receiver
                        .durable_covers_current(&row.app_id, &row.record_id)
                        .unwrap()
                })
            {
                let Inbound::Delta { author, data } = observer.recv().await.unwrap() else {
                    continue;
                };
                receiver
                    .receive_from(
                        TransportOrigin::RetainedRelayDelta(author),
                        postcard::from_bytes(&data).unwrap(),
                        now_time(12),
                    )
                    .unwrap();
            }
        })
        .await
        .unwrap();

        let tail_start = store.state.outbox.len();
        store
            .action(
                enrollment.browser_certificate.device_public_key,
                &ActionRequest {
                    version: 1,
                    room: store.state.room.clone(),
                    invocation_id: "real-relay-writer".into(),
                    operation: NativeOperation::WriterInsert {
                        text: "through retained delta".into(),
                    },
                },
                13,
            )
            .unwrap();
        let tail = &store.state.outbox[tail_start..];
        assert!(tail.iter().any(|publication| {
            publication.kind == PublicationKind::Delta
                && matches!(
                    publication.message,
                    NeighbourMsg::AppDocument { ref envelope, .. }
                        if matches!(
                            collab_types::DocumentMutation::decode(&envelope.payload),
                            Ok(collab_types::DocumentMutation::Snapshot(_))
                        )
                )
        }));
        for publication in tail {
            publish(&publisher, publication).unwrap();
        }
        let expected = store.peer_at(13).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while receiver.rows() != expected.rows()
                || receiver.rows().iter().any(|row| {
                    !receiver
                        .durable_covers_current(&row.app_id, &row.record_id)
                        .unwrap()
                })
            {
                let Inbound::Delta { author, data } = observer.recv().await.unwrap() else {
                    continue;
                };
                receiver
                    .receive_from(
                        TransportOrigin::RetainedRelayDelta(author),
                        postcard::from_bytes(&data).unwrap(),
                        now_time(14),
                    )
                    .unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(receiver.pending_count(), 0);
        assert_eq!(receiver.incomplete_reason(), None);
        publisher.close().await;
        observer.close().await;
        relay.shutdown().await;
    }

    #[test]
    fn cloud_relay_uses_the_qps_retention_authority() {
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[83; 32]);
        let device = SigningKey::from_bytes(&[84; 32]);
        let mut store = Store::open(dir.path(), "retention-config").unwrap();
        let enrollment = crate::tests::approved(store.inspect(10).unwrap(), &root, &device);
        store.apply(&enrollment, 11).unwrap();
        let bound = store.state.bound.as_ref().unwrap();
        let workspace = qos_relay::room_workspace_id(&store.state.room).unwrap();
        let config = relay_config(dir.path(), bound, &store.state.room, workspace).unwrap();
        let binding = &config.workspaces[0];
        assert!(binding.shell_authority.is_some());
        assert_eq!(
            binding.checkpoint_prefix_recovery,
            qos_relay::CheckpointPrefixRecovery::NativeOwnerDurableBaseline
        );
        let authority = binding.shell_authority.as_ref().unwrap();
        let batch = checkpoint_outbox(&store.state.outbox);
        let (room_message, snapshots) = batch.split_first().unwrap();
        let (room_channel, _) =
            qos_peer_state::transport::retention(&room_message.message, true).unwrap();
        let room_bytes = postcard::to_stdvec(&room_message.message).unwrap();
        let verified_room = qos_peer_state::retained_proof::verify_room(
            authority,
            bound.authority.device_public_key,
            room_channel,
            &room_bytes,
            qos_peer_state::retained_proof::RoomInput::Checkpoint,
            None,
        )
        .expect("the actual Cloud batch must carry a durable room proof");
        assert_eq!(snapshots.len(), 2);
        for snapshot in snapshots {
            let (channel, _) =
                qos_peer_state::transport::retention(&snapshot.message, true).unwrap();
            let bytes = postcard::to_stdvec(&snapshot.message).unwrap();
            let NeighbourMsg::AppDocument { envelope, .. } = &snapshot.message else {
                panic!("Cloud bootstrap must retain only governed app snapshots after the room");
            };
            qos_peer_state::retained_proof::verify_snapshot(
                &verified_room,
                *envelope.author.as_bytes(),
                channel,
                &bytes,
            )
            .expect("the actual Cloud snapshot must pair with its durable receipt");
        }
    }

    #[test]
    fn startup_ack_demux_retains_early_delivery_and_rejects_wrong_authority() {
        let room = uuid::Uuid::from_u128(0x51);
        let host = uuid::Uuid::from_u128(0x52);
        let action = qos_relay::peer_actions::DirectedAction {
            room,
            request_id: uuid::Uuid::from_u128(0x53),
            source: [1; 32],
            target: [2; 32],
            message: qos_relay::peer_actions::ActionMessage::CatalogueRefresh { scope: None },
        };
        let mut pending = std::collections::VecDeque::new();
        assert!(
            !retain_startup_action(
                CertifiedActionEvent::Control(EnvelopeV2::Delivered {
                    action: action.clone(),
                    source_user: [3; 32],
                }),
                ExpectedActionReady::Transport { room },
                &mut pending,
            )
            .unwrap()
        );
        assert!(
            retain_startup_action(
                CertifiedActionEvent::Control(EnvelopeV2::ActionsReady {
                    room,
                    capability: ActionCapability::TransportOnly,
                }),
                ExpectedActionReady::Transport { room },
                &mut pending,
            )
            .unwrap()
        );
        assert!(matches!(
            pending.pop_front(),
            Some(CertifiedActionEvent::Control(EnvelopeV2::Delivered {
                action: retained,
                ..
            })) if retained == action
        ));
        assert!(
            retain_startup_action(
                CertifiedActionEvent::Control(EnvelopeV2::ActionsReady {
                    room,
                    capability: ActionCapability::DurableBroker {
                        host: uuid::Uuid::from_u128(0x54),
                    },
                }),
                ExpectedActionReady::Broker { room, host },
                &mut pending,
            )
            .unwrap_err()
            .to_string()
            .contains("not acknowledged")
        );
    }

    fn room(value: u8) -> NeighbourMsg {
        NeighbourMsg::SyncResp {
            changes: vec![vec![value]],
            room: "test-room".into(),
        }
    }
    #[test]
    fn repeated_governor_checkpoints_do_not_flood_transport() {
        let messages = (0..=255)
            .map(|value| StoredPublication::baseline(room(value)).unwrap())
            .collect::<Vec<_>>();
        let compact = checkpoint_outbox(&messages);
        assert_eq!(compact.len(), 1);
        assert_eq!(
            postcard::to_stdvec(&compact[0].message).unwrap(),
            postcard::to_stdvec(&room(255)).unwrap()
        );
    }
    #[test]
    fn compaction_preserves_every_governance_delta_between_newer_checkpoints() {
        let messages = vec![
            StoredPublication::baseline(room(1)).unwrap(),
            StoredPublication::mutation(room(2)).unwrap(),
            StoredPublication::mutation(room(3)).unwrap(),
            StoredPublication::baseline(room(4)).unwrap(),
        ];
        let compact = checkpoint_outbox(&messages);
        assert_eq!(compact.len(), 3);
        assert_eq!(compact[0].kind, PublicationKind::Delta);
        assert_eq!(compact[1].kind, PublicationKind::Delta);
        assert_eq!(compact[2].kind, PublicationKind::Checkpoint);
    }
    #[test]
    fn live_incremental_plus_governance_delta_does_not_require_replayed_snapshot() {
        use automerge::transaction::Transactable;
        use qos_actions::Action;
        let root = SigningKey::from_bytes(&[72; 32]);
        let key = SigningKey::from_bytes(&[73; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "live app delta".into(),
            capabilities: qos_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        qos_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let now = web_time::SystemTime::now();
        let room = "78787878-7878-4787-8787-787878787878";
        let mut peer = StatePeer::new(
            room,
            cert.clone(),
            vec![cert.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.install_seed(initial_state(room, &cert, &key).unwrap(), &key, now)
            .unwrap();
        let initial = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
        let mut receiver = StatePeer::new(
            room,
            cert.clone(),
            vec![cert.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        for message in initial {
            receiver
                .receive_from(
                    TransportOrigin::RelayDelta(cert.device_public_key),
                    message,
                    now,
                )
                .unwrap();
        }
        let before = peer.clone();
        let app = "dev.qos.word-processor";
        let record = "tab:0000000000000001";
        let mut doc = automerge::AutoCommit::load(&peer.document(app, record).unwrap()).unwrap();
        let heads = doc.base_heads();
        let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
        let block = qdoc::model::blocks_in_order(&doc, &model)[0].clone();
        word_processor::actions::WordProcessorAction::SpliceText {
            region: word_processor::actions::RegionKind::Body,
            block,
            at: 0,
            del: 0,
            ins: "live".into(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();
        let incremental = peer
            .publish_incremental(app, record, doc.save_after(&heads), &key, now)
            .unwrap();
        let prepared = peer
            .durable_mutation_messages(&before, &key, now, 2)
            .unwrap();
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(cert.device_public_key),
                incremental,
                now,
            )
            .unwrap();
        for message in prepared {
            receiver
                .receive_from(
                    TransportOrigin::RetainedRelayDelta(cert.device_public_key),
                    message,
                    now,
                )
                .unwrap();
        }
        assert_eq!(
            receiver.document_heads(app, record),
            peer.document_heads(app, record)
        );
        assert_eq!(receiver.pending_count(), 0);
        assert_eq!(receiver.incomplete_reason(), None);
        for row in receiver.rows() {
            assert!(
                receiver
                    .durable_covers_current(&row.app_id, &row.record_id)
                    .unwrap(),
                "live retained snapshots must satisfy the new durable receipt for {}:{}",
                row.app_id,
                row.record_id,
            );
        }
    }
    #[test]
    fn full_transport_queue_leaves_unsent_work_for_retry() {
        let messages = vec![room(1), room(2), room(3)]
            .into_iter()
            .map(|message| StoredPublication::baseline(message).unwrap())
            .collect::<Vec<_>>();
        let mut sent = 0;
        let count = publish_batch(&messages, |_| {
            if sent == 2 {
                return Err(qos_relay::coedit::CoeditError::Disconnected.into());
            }
            sent += 1;
            Ok(())
        })
        .expect("backpressure must not terminate the host");
        assert_eq!(count, 2);
        assert_eq!(messages.len() - count, 1);
    }
    #[test]
    fn large_queue_yields_between_bounded_batches() {
        let messages = (0..=255)
            .map(|value| StoredPublication::baseline(room(value)).unwrap())
            .collect::<Vec<_>>();
        assert!(publish_batch(&messages, |_| Ok(())).unwrap() <= 16);
    }
    #[test]
    fn compacted_checkpoints_keep_governance_and_real_document_heads_consistent() {
        use automerge::transaction::Transactable;
        use qos_actions::Action;
        let root = SigningKey::from_bytes(&[70; 32]);
        let key = SigningKey::from_bytes(&[71; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "publication test".into(),
            capabilities: qos_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        qos_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let now = web_time::SystemTime::now();
        let room = "77777777-7777-4777-8777-777777777777";
        let mut peer = StatePeer::new(
            room,
            cert.clone(),
            vec![cert.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.install_seed(initial_state(room, &cert, &key).unwrap(), &key, now)
            .unwrap();
        let first = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
        let app = "dev.qos.word-processor";
        let record = "tab:0000000000000001";
        let mut doc = automerge::AutoCommit::load(&peer.document(app, record).unwrap()).unwrap();
        let before = doc.base_heads();
        let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
        let block = qdoc::model::blocks_in_order(&doc, &model)[0].clone();
        word_processor::actions::WordProcessorAction::SpliceText {
            region: word_processor::actions::RegionKind::Body,
            block,
            at: 0,
            del: 0,
            ins: "retained".into(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();
        peer.publish_incremental(app, record, doc.save_after(&before), &key, now)
            .unwrap();
        let last = peer.durable_bootstrap_messages(&key, now, 2).unwrap();
        for start in 0..first.len() {
            let mut queue = baseline_publications(first[start..].to_vec()).unwrap();
            queue.extend(baseline_publications(last.clone()).unwrap());
            let compact = checkpoint_outbox(&queue);
            assert_eq!(compact.len(), last.len());
            let mut receiver = StatePeer::new(
                room,
                cert.clone(),
                vec![cert.clone()],
                RevocationList::new(),
                now,
            )
            .unwrap();
            for message in compact {
                receiver
                    .receive_from(
                        TransportOrigin::RelayDelta(cert.device_public_key),
                        message.message,
                        now,
                    )
                    .unwrap();
            }
            assert_eq!(receiver.pending_count(), 0);
            assert_eq!(
                serde_json::to_value(receiver.rows()).unwrap(),
                serde_json::to_value(peer.rows()).unwrap()
            );
        }
    }
    #[test]
    fn serialization_or_other_errors_are_not_hidden_as_backpressure() {
        assert!(
            publish_batch(&[StoredPublication::baseline(room(1)).unwrap()], |_| Err(
                anyhow!("invalid encoding")
            ))
            .is_err()
        );
    }
}

#[cfg(all(test, unix))]
mod durability_runtime_tests {
    use super::*;
    #[test]
    fn durability_receive_owner_failure_stops_actual_runtime_after_rejected_input_survives() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root_key = SigningKey::from_bytes(&[7; 32]);
        let device = SigningKey::from_bytes(&[8; 32]);
        let mut store = Store::open(dir.path(), "durability-runtime").unwrap();
        let enrollment = crate::tests::approved(store.inspect(10).unwrap(), &root_key, &device);
        store.apply(&enrollment, 11).unwrap();
        let before_rows = store.peer().unwrap().rows();
        let workspace = qos_relay::room_workspace_id(&store.state.room).unwrap();
        drop(store);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let path = dir.path().to_path_buf();
        let (mut daemon, browser, message) = runtime.block_on(async {
            let daemon = tokio::spawn(async move {
                serve_with_mode(
                    &path,
                    "durability-runtime",
                    RelayMode::Disabled,
                    async {
                        let _ = stop_rx.await;
                    },
                    Some(ready_tx),
                )
                .await
            });
            let address = tokio::time::timeout(Duration::from_secs(10), ready_rx)
                .await
                .unwrap()
                .unwrap();
            let endpoint = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .unwrap();
            let client = RelayClient::connect_v2_to_enrolled_home_peer(
                endpoint,
                address,
                enrollment.host_certificate.user_public_key,
                enrollment.host_certificate.device_public_key,
                enrollment.browser_certificate.clone(),
                device.clone(),
                RevocationList::new(),
            )
            .await
            .unwrap();
            let browser =
                RelayCoeditSession::join(client, workspace, device.verifying_key().to_bytes())
                    .unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                while !matches!(browser.recv().await, Some(Inbound::Ready { .. })) {}
                loop {
                    let store = Store::open(dir.path(), "durability-runtime").unwrap();
                    let empty = store.state.outbox.is_empty();
                    drop(store);
                    if empty {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap();
            let message = crate::durability_tests::document_message(
                &Store::open(dir.path(), "durability-runtime").unwrap(),
                &device,
            );
            // A bad signature reaches the Store's receive validation, not just the decoder.
            let mut bad = message.clone();
            if let NeighbourMsg::AppDocument { envelope, .. } = &mut bad {
                envelope.signature = collab_types::Signature::from_bytes([0; 64]);
            }
            let publication = StoredPublication::mutation(bad).unwrap();
            publish(&browser, &publication).unwrap();
            tokio::time::sleep(Duration::from_millis(350)).await;
            assert!(
                !daemon.is_finished(),
                "untrusted message rejection killed the runtime"
            );
            (daemon, browser, message)
        });
        let result = crate::storage::test_support::with_save_failure(
            dir.path(),
            atomic_file::test_support::Checkpoint::BeforeCommit,
            || {
                runtime.block_on(async {
                    publish(&browser, &StoredPublication::mutation(message).unwrap()).unwrap();
                    tokio::time::timeout(Duration::from_secs(2), &mut daemon).await
                })
            },
        );
        let _ = stop_tx.send(());
        let stopped_for_owner_failure =
            matches!(&result, Ok(Ok(Err(error))) if crate::is_store_failure(error));
        runtime.block_on(async {
            browser.close().await;
            if result.is_err() {
                let _ = tokio::time::timeout(Duration::from_secs(5), daemon).await;
            }
        });
        let recovered = Store::open(dir.path(), "durability-runtime").unwrap();
        assert_ne!(
            recovered.peer().unwrap().rows(),
            before_rows,
            "fault did not reach accepted Store mutation"
        );
        assert!(
            stopped_for_owner_failure,
            "runtime swallowed owner storage failure: {result:?}"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "runtime/agent_process_tests.rs"]
mod agent_process_tests;
