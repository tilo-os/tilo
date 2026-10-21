// Only compiled by the existing native test-support feature. Synthetic keys
// still enter through the production signed admission verifier, never a context
// constructor or a bypass of certificate/current-execution policy.
pub struct CertifiedPersonFixture {
    pub broker: crate::collab_broker::AppActionBroker,
    pub context: CertifiedPersonContext,
    pub caller: tilo_actions::remote::Caller,
    pub host: tilo_identity_host::custody::HostIdentity,
    pub origin: crate::peer_actions::PolicyOrigin,
    pub current: Arc<std::sync::atomic::AtomicBool>,
    _evidence_env: tilo_test_env::ScopedEnv,
    _evidence_home: tilo_test_env::TestHome,
}
struct FixturePersonAuthority {
    certificate: tilo_user_identity::DeviceCert,
    room: uuid::Uuid,
    time: crate::peer_actions::PolicyTime,
    current: Arc<std::sync::atomic::AtomicBool>,
}
impl crate::peer_actions::CertificateAuthority for FixturePersonAuthority {
    fn policy_time(&self) -> Result<Option<crate::peer_actions::PolicyTime>, String> {
        Ok(Some(self.time.clone()))
    }
    fn authorize(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if self.current.load(Ordering::Acquire)
            && room == self.room
            && certificate == &self.certificate
        {
            Ok(())
        } else {
            Err("synthetic current certificate revoked".into())
        }
    }
}
pub fn certified_person_fixture() -> CertifiedPersonFixture {
    certified_person_fixture_for_seeds(141, 142)
}
/// Distinct synthetic people, still admitted by the original signed verifier.
pub fn certified_person_fixture_for_seeds(
    root_seed: u8,
    device_seed: u8,
) -> CertifiedPersonFixture {
    let evidence_home = tilo_test_env::TestHome::new().unwrap();
    let identity_paths =
        tilo_user_identity::IdentityPaths::under(evidence_home.root().join("system-identity"));
    let mut evidence_env = tilo_test_env::ScopedEnv::new();
    evidence_env
        .set(
            "TILO_IDENTITY_ROOT",
            evidence_home.root().join("system-identity"),
        )
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &evidence_home.config().join("tilo/identity"),
        &identity_paths,
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[root_seed; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[device_seed; 32]);
    let clock = crate::peer_actions::OwnerPolicyClock::new().unwrap();
    let origin = clock.new_origin();
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Synthetic native messaging device".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: clock.now_ns().unwrap(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let room = collab_types::person_room(&certificate.user_public_key);
    let mut workspace = crate::test_support::Workspace::in_room(
        crate::test_support::WorkspaceId(71),
        tilo_desktop_model::RoomId::parse(&room.to_string()).unwrap(),
        "Synthetic personal room",
    );
    workspace.set_owner(hex::encode(certificate.user_public_key));
    let mut tree = crate::test_support::WorkspaceTree::default();
    tree.workspaces.push(workspace);
    tree.persist_path = Some(evidence_home.root().join("workspace.json"));
    tree.set_active(crate::test_support::WorkspaceId(71));
    let fanout = Arc::new(FanoutState::new());
    let (state, _) = CapnpBrokerState::new(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.shared()),
        None,
    );
    let current = Arc::new(std::sync::atomic::AtomicBool::new(true));
    assert!(
        state
            .certificate_authority
            .set(Arc::new(FixturePersonAuthority {
                certificate: certificate.clone(),
                room,
                time: origin.capture().unwrap(),
                current: current.clone(),
            }))
            .is_ok()
    );
    let caller = tilo_actions::remote::Caller::authenticated(
        certificate.user_public_key,
        certificate.device_public_key,
    )
    .in_room(room.to_string())
    .in_new_execution();
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.to_string(),
        device_certificate: serde_json::to_vec(&certificate).unwrap(),
        proof: ed25519_dalek::Signer::sign(
            &device,
            &runtime_identity::collaboration_join_signing_bytes(&room.to_string()),
        )
        .to_bytes()
        .to_vec(),
    }
    .encode()
    .unwrap();
    let context = state
        .certified_person_context(&caller, &presentation)
        .unwrap();
    CertifiedPersonFixture {
        broker: crate::collab_broker::AppActionBroker::from_state_for_test(state),
        context,
        caller,
        origin,
        current,
        _evidence_env: evidence_env,
        _evidence_home: evidence_home,
        host: tilo_identity_host::custody::HostIdentity {
            device_cert: certificate,
            device_signing_key: device,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        },
    }
}
