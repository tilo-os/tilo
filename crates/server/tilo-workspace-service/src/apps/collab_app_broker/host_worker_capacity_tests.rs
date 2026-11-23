use super::*;

fn state() -> Arc<AppBrokerState> {
    let fanout = Arc::new(FanoutState::new());
    AppBrokerState::new(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        None,
        None,
    )
    .0
}
fn host_worker(record: usize) -> AdmittedJoinIdentity {
    let root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: [92; 32],
        device_label: "host".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    AdmittedJoinIdentity {
        worker_registration: Some(PrivateWorkerRegistration {
            identity: uuid::Uuid::from_u128(record as u128),
            route: CrdtRoute::from_channel_key(&format!(
                "{}#app:so.tilo.notes#document:tab:{record:016x}",
                uuid::Uuid::from_u128(7)
            )),
            certificate: certificate.clone(),
        }),
        certificate: Some(certificate),
        verified: VerifiedJoinIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [92; 32],
            capped: false,
        },
        actor_key: "host".into(),
        display_handle: None,
        role: CapRole::Owner,
        agent_id: None,
    }
}
fn retained(state: &AppBrokerState) -> usize {
    state.host_workers.lock().unwrap().len()
}
fn retain(state: &AppBrokerState, peer: u64, record: usize) -> Result<(), BrokerError> {
    let worker = host_worker(record);
    let route = worker.worker_registration.clone().unwrap().route;
    state.retain_host_worker(peer, &route, &worker)
}

tilo_test_env::scenario! {
    fn host_workers_are_kept_per_connection_and_withdrawn_with_it;

    phase host_worker_connect_disconnect_cycles_do_not_grow_the_registry;
    // Repeated worker mounts (connect, then drop every connection) never
    // grow the provenance registry: each disconnect withdraws its entry.
    {
        let state = state();
        let mut peer = 0u64;
        for _cycle in 0..20 {
            let first = peer;
            for record in 0..40 {
                peer += 1;
                retain(&state, peer, record).unwrap();
            }
            assert_eq!(retained(&state), 40);
            for connection in first + 1..=peer {
                state.remove_connection(connection);
            }
            assert_eq!(retained(&state), 0);
        }
    }

    phase host_workers_are_owned_by_their_connections_not_a_global_count;
    // Workers are counted per connection, never across them: a Cloud host
    // mounting a worker per record for every app and agent, far past any
    // former global bound, keeps every one, and closing one withdraws only
    // its own.
    {
        let state = state();
        let agents = 16;
        let records = 8 * 64; // eight hosted apps with 64 records each
        for peer in 0..(agents + records) {
            retain(&state, peer as u64 + 1, peer).unwrap();
        }
        assert_eq!(retained(&state), agents + records);
        // A reconnect of an already admitted peer with the same scope is idempotent.
        assert!(retain(&state, 1, 0).is_ok());
        assert_eq!(retained(&state), agents + records);
        state.remove_connection(1);
        assert_eq!(retained(&state), agents + records - 1);
        assert!(retain(&state, (agents + records) as u64 + 1, agents + records).is_ok());
    }
}
