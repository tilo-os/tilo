use super::*;
use tilo_peer_state::workspace_checkpoint::{CheckpointCommand, CheckpointRequest};

struct CheckpointDriver {
    actions: tilo_action_broker::collab_broker::AppActionBroker,
    host: std::sync::Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>,
    caller: tilo_actions::remote::Caller,
    provider: String,
    evidence: Vec<u8>,
}
impl CheckpointDriver {
    fn new(fixture: &NativeFixture) -> Self {
        let (key, certificate) =
            tilo_identity_host::broker::trusted_current_device(&fixture.room.to_string()).unwrap();
        Self::certified(fixture, &key, &certificate)
    }
    fn certified(
        fixture: &NativeFixture,
        key: &ed25519_dalek::SigningKey,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Self {
        Self::certified_with_actions(
            fixture.room,
            key,
            certificate,
            fixture.broker.app_action_broker().unwrap(),
        )
    }
    fn certified_with_actions(
        room_id: crate::shell::state::RoomId,
        key: &ed25519_dalek::SigningKey,
        certificate: &tilo_user_identity::DeviceCert,
        actions: tilo_action_broker::collab_broker::AppActionBroker,
    ) -> Self {
        use ed25519_dalek::Signer as _;
        let room = room_id.to_string();
        let surface = wait_for_membership_surface(&actions, room_id);
        assert_eq!(
            surface
                .actions
                .iter()
                .filter(|entry| entry.resource == "desktop:checkpoints")
                .count(),
            4
        );
        let evidence = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.clone(),
            device_certificate: serde_json::to_vec(certificate).unwrap(),
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        let caller = tilo_actions::remote::Caller::authenticated(
            certificate.user_public_key,
            certificate.device_public_key,
        )
        .in_room(&room)
        .in_new_execution();
        let host = actions
            .certified_local_action_host(&caller, &evidence)
            .unwrap();
        Self {
            actions,
            host,
            caller,
            provider: tilo_actions::remote::application_action_target(
                &room,
                "so.tilo.desktop",
                "desktop:checkpoints",
                "capture",
            )
            .provider,
            evidence,
        }
    }
    fn request(
        &self,
        action: &str,
        arguments: serde_json::Value,
        invocation: tilo_actions::InvocationId,
    ) -> tilo_actions::remote::InvokeRequest {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
        InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: self.provider.clone(),
                address: tilo_actions::ActionAddress::new("desktop:checkpoints", action),
            },
            argument: InvokeArgument::Json(serde_json::from_value(arguments).unwrap()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            appearance: None,
            wait_ms: None,
        }
    }
    fn invoke(
        &self,
        action: &str,
        arguments: serde_json::Value,
        invocation: tilo_actions::InvocationId,
    ) -> Result<serde_json::Value, String> {
        use tilo_actions::remote::PendingPoll;
        let mut pending = self
            .host
            .invoke(&self.caller, self.request(action, arguments, invocation));
        let deadline = tilo_test_env::hang_deadline();
        loop {
            match pending.poll_ready() {
                PendingPoll::Ready(receipt) => {
                    receipt.map_err(|e| e.to_string())?;
                    break;
                }
                PendingPoll::Abandoned => return Err("checkpoint admission abandoned".into()),
                PendingPoll::Waiting => {
                    yield_before(deadline);
                }
            }
        }
        loop {
            let observed = self
                .host
                .observe(&self.caller, &invocation)
                .ok_or("checkpoint result unavailable or currently unauthorized")?;
            match observed.update {
                InvocationUpdate::Succeeded { output, .. } => {
                    return output
                        .value
                        .ok_or_else(|| "checkpoint result missing".into());
                }
                InvocationUpdate::Failed { error, .. } => {
                    return Err(format!("{}: {}", error.code, error.message));
                }
                _ => {
                    yield_before(deadline);
                }
            }
        }
    }
}

fn yield_before(deadline: Instant) {
    assert!(
        Instant::now() < deadline,
        "checkpoint admission or service did not settle"
    );
    std::thread::yield_now();
}

fn admit_checkpoint_reader(
    fixture: &mut NativeFixture,
    own_person: bool,
) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    use tilo_peer_state::join::{self, JoinTransportRequest, JoinTransportResponse};
    let now = std::time::SystemTime::now();
    let key = ed25519_dalek::SigningKey::from_bytes(&[116; 32]);
    let profile =
        tilo_identity_host::registry::selected_identity_profile(&fixture.room.to_string())
            .unwrap()
            .unwrap();
    let fleet = tilo_identity_host::device_fleet::DeviceFleetStore::from_env().unwrap();
    let now_ns = tilo_joined_room::testing::ns(now).unwrap();
    let role = tilo_identity_host::enrollment::EnrollmentRole::PersonalDevice;
    let token = fleet
        .mint_enrollment(
            &profile.id,
            role.preset(),
            now_ns,
            now_ns.checked_add(60_000_000_000).unwrap(),
        )
        .unwrap();
    let certificate = if own_person {
        fleet
            .complete_enrollment(
                &profile.id,
                &token,
                role,
                key.verifying_key().to_bytes(),
                "Checkpoint reader",
                now_ns,
            )
            .unwrap()
    } else {
        tilo_joined_room::testing::certificate(
            &ed25519_dalek::SigningKey::from_bytes(&[117; 32]),
            &key,
            "Remote checkpoint reader",
        )
    };
    let hint = fixture.governor.hint(iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(&fixture.certificate.device_public_key).unwrap(),
    ));
    let invitation = fixture.governor.issue(
        vec![hint],
        join::MemberRole::Editor,
        Some(certificate.user_public_key),
        None,
        tilo_joined_room::testing::ns(now)
            .unwrap()
            .checked_add(60_000_000_000)
            .unwrap(),
    );
    let JoinTransportResponse::Challenge(challenge) = fixture.governor.handle(
        certificate.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: certificate.clone(),
            request_id: [118; 32],
        }),
    ) else {
        panic!("checkpoint reader challenge refused")
    };
    assert!(matches!(
        fixture.governor.handle(
            certificate.device_public_key,
            JoinTransportRequest::Complete(join::JoinCompleteRequest {
                proof: join::sign_join_proof(challenge.challenge, &key).unwrap(),
            }),
        ),
        JoinTransportResponse::Complete(_)
    ));
    for (bytes, _) in fixture.governor.outgoing() {
        fixture
            .owner
            .receive(
                fixture.room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: fixture.certificate.device_public_key,
                },
                postcard::from_bytes(bytes).unwrap(),
                now,
            )
            .unwrap();
    }
    (key, certificate)
}

fn prepare(fixture: &NativeFixture) -> serde_json::Value {
    let (key, _) =
        tilo_identity_host::broker::trusted_current_device(&fixture.room.to_string()).unwrap();
    let declared = tilo_joined_room::ApplicationRecord::declared(
        "so.tilo.chat",
        "checkpoint-test",
        "chat.workspace",
        &["chat.workspace".into()],
    )
    .unwrap();
    fixture
        .owner
        .prepare_application_record(
            fixture.room,
            &fixture.certificate,
            &key,
            &declared,
            std::time::SystemTime::now(),
        )
        .unwrap();
    serde_json::json!([{"app":"so.tilo.chat", "record":"checkpoint-test"}])
}

#[test]
fn native_checkpoint_public_capture_inspect_find_and_name_movement() {
    let mut fixture = fixture();
    let (member_key, member) = admit_checkpoint_reader(&mut fixture, true);
    let routes = prepare(&fixture);
    let driver = CheckpointDriver::new(&fixture);
    let capture = tilo_actions::InvocationId::new();
    let first = driver
        .invoke(
            "capture",
            serde_json::json!({"name":"first", "routes":routes}),
            capture,
        )
        .unwrap();
    assert_eq!(first["id"], capture.0.to_string());
    assert_original_capture_retry(&driver, capture, &routes, &first);
    let manifest = driver
        .invoke(
            "inspect",
            serde_json::json!({"id":capture.0}),
            tilo_actions::InvocationId::new(),
        )
        .unwrap();
    assert_checkpoint_movement(&fixture, &driver, capture, &routes, &manifest);
    assert_closed_checkpoint_arguments(&driver, &routes);
    assert_checkpoint_integrity_refusals(&fixture, &driver, capture, &routes, &manifest);
    let member_driver = CheckpointDriver::certified(&fixture, &member_key, &member);
    assert!(
        member_driver
            .invoke(
                "capture",
                serde_json::json!({"name":"first", "routes":routes}),
                capture
            )
            .is_err(),
        "a different admitted caller cannot replay the original capture"
    );
    drop(member_driver);
    drop(driver);
    drop(fixture);
}

fn assert_original_capture_retry(
    driver: &CheckpointDriver,
    capture: tilo_actions::InvocationId,
    routes: &serde_json::Value,
    first: &serde_json::Value,
) {
    assert_eq!(
        driver
            .invoke(
                "capture",
                serde_json::json!({"name":"first", "routes":routes}),
                capture
            )
            .unwrap(),
        *first
    );
    assert!(
        driver
            .invoke(
                "capture",
                serde_json::json!({"name":"changed", "routes":routes}),
                capture
            )
            .is_err()
    );
}

fn assert_checkpoint_movement(
    fixture: &NativeFixture,
    driver: &CheckpointDriver,
    capture: tilo_actions::InvocationId,
    routes: &serde_json::Value,
    manifest: &serde_json::Value,
) {
    assert_eq!(manifest["entries"].as_array().unwrap().len(), 1);
    assert!(manifest.get("documents").is_none());
    assert_eq!(
        manifest["author"],
        serde_json::json!(fixture.certificate.user_public_key)
    );
    driver
        .invoke(
            "name-bind",
            serde_json::json!({"name":"moving", "id":capture.0}),
            tilo_actions::InvocationId::new(),
        )
        .unwrap();
    let second_id = tilo_actions::InvocationId::new();
    let second = driver
        .invoke(
            "capture",
            serde_json::json!({"name":"second", "routes":routes}),
            second_id,
        )
        .unwrap();
    driver
        .invoke(
            "name-bind",
            serde_json::json!({"name":"moving", "id":second_id.0}),
            tilo_actions::InvocationId::new(),
        )
        .unwrap();
    assert_eq!(
        driver
            .invoke(
                "find",
                serde_json::json!({"name":"moving"}),
                tilo_actions::InvocationId::new()
            )
            .unwrap(),
        second
    );
    assert_eq!(
        driver
            .invoke(
                "inspect",
                serde_json::json!({"id":capture.0}),
                tilo_actions::InvocationId::new()
            )
            .unwrap(),
        *manifest
    );
}

fn assert_closed_checkpoint_arguments(driver: &CheckpointDriver, routes: &serde_json::Value) {
    for field in ["caller", "signing_key", "room", "command"] {
        let mut arguments = serde_json::json!({"name":"forged", "routes":routes});
        arguments[field] = "forged".into();
        assert!(
            driver
                .invoke("capture", arguments, tilo_actions::InvocationId::new())
                .is_err()
        );
    }
}

fn assert_checkpoint_integrity_refusals(
    fixture: &NativeFixture,
    driver: &CheckpointDriver,
    capture: tilo_actions::InvocationId,
    routes: &serde_json::Value,
    manifest: &serde_json::Value,
) {
    let inspect = tilo_actions::InvocationId::new();
    let find = tilo_actions::InvocationId::new();
    driver
        .invoke("inspect", serde_json::json!({"id":capture.0}), inspect)
        .unwrap();
    driver
        .invoke("find", serde_json::json!({"name":"first"}), find)
        .unwrap();
    let content: [u8; 32] =
        serde_json::from_value(manifest["entries"][0]["content"].clone()).unwrap();
    let address = blake3::Hash::from_bytes(content).to_hex().to_string();
    let block = fixture
        .path
        .parent()
        .unwrap()
        .join("content")
        .join(address.get(..2).unwrap())
        .join(address);
    let retained = std::fs::read(&block).unwrap();
    std::fs::remove_file(&block).unwrap();
    for invocation in [inspect, find, capture] {
        assert!(
            driver.host.observe(&driver.caller, &invocation).is_none(),
            "cached success must not disclose unavailable retained content"
        );
    }
    assert!(
        driver
            .invoke("inspect", serde_json::json!({"id":capture.0}), inspect)
            .is_err()
    );
    assert!(
        driver
            .invoke("find", serde_json::json!({"name":"first"}), find)
            .is_err()
    );
    assert!(
        driver
            .invoke(
                "inspect",
                serde_json::json!({"id":capture.0}),
                tilo_actions::InvocationId::new()
            )
            .is_err()
    );
    assert!(
        driver
            .invoke(
                "capture",
                serde_json::json!({"name":"first", "routes":routes}),
                capture
            )
            .is_err()
    );
    std::fs::write(&block, b"corrupt retained content").unwrap();
    assert!(
        driver
            .invoke(
                "find",
                serde_json::json!({"name":"first"}),
                tilo_actions::InvocationId::new()
            )
            .is_err()
    );
    std::fs::write(block, retained).unwrap();
}

fn assert_checkpoint_admission_refused(
    host: &std::sync::Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>,
    driver: &CheckpointDriver,
    request: tilo_actions::remote::InvokeRequest,
) {
    use tilo_actions::remote::PendingPoll;
    let mut pending = host.invoke(&driver.caller, request);
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match pending.poll_ready() {
            PendingPoll::Ready(receipt) => {
                assert!(
                    receipt.is_err(),
                    "unauthorized replay must not acknowledge a retained result"
                );
                return;
            }
            PendingPoll::Abandoned => panic!("checkpoint refusal abandoned"),
            PendingPoll::Waiting => yield_before(deadline),
        }
    }
}

#[test]
fn native_checkpoint_agent_cannot_replay_or_observe_person_success_or_failure() {
    let fixture = fixture();
    let routes = prepare(&fixture);
    let driver = CheckpointDriver::new(&fixture);
    let capture = tilo_actions::InvocationId::new();
    let requests = [
        driver.request(
            "capture",
            serde_json::json!({"name":"private", "routes":routes}),
            capture,
        ),
        driver.request(
            "name-bind",
            serde_json::json!({"name":"private-alias", "id":capture.0}),
            tilo_actions::InvocationId::new(),
        ),
        driver.request(
            "inspect",
            serde_json::json!({"id":capture.0}),
            tilo_actions::InvocationId::new(),
        ),
        driver.request(
            "find",
            serde_json::json!({"name":"private-alias"}),
            tilo_actions::InvocationId::new(),
        ),
        driver.request(
            "find",
            serde_json::json!({"name":"private-missing"}),
            tilo_actions::InvocationId::new(),
        ),
    ];
    for (index, request) in requests.iter().enumerate() {
        let tilo_actions::remote::InvokeArgument::Json(arguments) = &request.argument else {
            panic!("checkpoint request must be JSON")
        };
        let result = driver.invoke(
            &request.target.address.action,
            serde_json::to_value(arguments).unwrap(),
            request.invocation,
        );
        assert_eq!(result.is_ok(), index < 4);
        assert!(
            driver
                .host
                .observe(&driver.caller, &request.invocation)
                .is_some()
        );
    }
    let agent = driver
        .actions
        .certified_local_agent_host(&driver.caller, &driver.evidence, "checkpoint-agent")
        .unwrap();
    for request in requests {
        assert!(
            agent.observe(&driver.caller, &request.invocation).is_none(),
            "agent must not receive a person's private success or error"
        );
        assert_checkpoint_admission_refused(&agent, &driver, request.clone());
        assert!(
            agent.observe(&driver.caller, &request.invocation).is_none(),
            "refused replay must not expose the original terminal result"
        );
        assert!(
            driver
                .host
                .observe(&driver.caller, &request.invocation)
                .is_some(),
            "agent refusal must not replace the person's original result"
        );
    }
    drop(agent);
    drop(driver);
    drop(fixture);
}

struct CheckpointReaderAuthority {
    native: NativeCertificates,
    room: uuid::Uuid,
    member: tilo_user_identity::DeviceCert,
}
impl tilo_action_broker::peer_actions::CertificateAuthority for CheckpointReaderAuthority {
    fn policy_time(&self) -> Result<Option<tilo_action_broker::peer_actions::PolicyTime>, String> {
        self.native.policy_time()
    }
    fn authorize_with_policy_time(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
        time: &tilo_action_broker::peer_actions::PolicyTime,
    ) -> Result<(), String> {
        if room == self.room && certificate == &self.member {
            tilo_user_identity::verify_device_cert_at(
                certificate,
                time.now_ns().map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())
        } else {
            self.native
                .authorize_with_policy_time(room, certificate, time)
        }
    }
    fn authorize(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if room == self.room && certificate == &self.member {
            Ok(())
        } else {
            self.native.authorize(room, certificate)
        }
    }
}

fn checkpoint_revocable_operations(
    routes: &serde_json::Value,
) -> [(&'static str, serde_json::Value, tilo_actions::InvocationId); 4] {
    let capture = tilo_actions::InvocationId::new();
    [
        (
            "capture",
            serde_json::json!({"name":"revocable", "routes":routes}),
            capture,
        ),
        (
            "name-bind",
            serde_json::json!({"name":"revocable-alias", "id":capture.0}),
            tilo_actions::InvocationId::new(),
        ),
        (
            "inspect",
            serde_json::json!({"id":capture.0}),
            tilo_actions::InvocationId::new(),
        ),
        (
            "find",
            serde_json::json!({"name":"revocable-alias"}),
            tilo_actions::InvocationId::new(),
        ),
    ]
}

fn checkpoint_reader_services(
    tree: crate::shell::state::WorkspaceTreeHandle,
    room: crate::shell::state::RoomId,
    member: &tilo_user_identity::DeviceCert,
) -> tilo_action_broker::services::HostServices {
    // Only certificate admission is explicit: checkpoint storage, custody,
    // membership, policy and the local monotonic clock remain native.
    let mut services = super::super::native_services(Some(tree), None, None, None);
    services.native_certificates = Some(std::sync::Arc::new(CheckpointReaderAuthority {
        native: NativeCertificates::default(),
        room: uuid::Uuid::parse_str(&room.to_string()).unwrap(),
        member: member.clone(),
    }));
    services
}

fn checkpoint_revocation_grant(
    room: crate::shell::state::RoomId,
    owner: &tilo_user_identity::DeviceCert,
    member: &tilo_user_identity::DeviceCert,
    revoke_write: bool,
) -> tilo_action_policy::Grant {
    let path = tilo_action_policy::TreePath::room(room.to_string());
    tilo_action_policy::Grant::new(
        "checkpoint-broker-revoked",
        tilo_action_policy::Subject::person(hex::encode(member.user_public_key)),
        if revoke_write {
            path
        } else {
            path.app("so.tilo.chat")
        },
        if revoke_write {
            tilo_action_policy::KindSet::UNDOABLE
        } else {
            tilo_action_policy::KindSet::READ
        },
        tilo_action_policy::Answer::Deny,
        tilo_action_policy::Subject::person(hex::encode(owner.user_public_key)),
    )
}

#[test]
fn native_checkpoint_broker_rechecks_current_read_and_room_write_on_invoke_and_observe() {
    for revoke_write in [false, true] {
        let mut fixture = fixture();
        let (member_key, member) = admit_checkpoint_reader(&mut fixture, false);
        assert_ne!(member.user_public_key, fixture.certificate.user_public_key);
        let tree = native_workspace_owner::test_tree_of(&fixture.owner);
        let services = checkpoint_reader_services(tree.clone(), fixture.room, &member);
        let routes = prepare(&fixture);
        drop(fixture.broker);
        let broker = tilo_action_broker::collab_broker::spawn_with_services(
            fixture.home.runtime().join("checkpoint-reader.sock"),
            Some(std::sync::Arc::new(super::super::NativeWorkspaceAuthority(
                tree,
            ))),
            tilo_action_broker::collab_outbound::PeerPublisher::disabled(),
            None,
            services,
            std::sync::Arc::new(std::sync::Mutex::new(
                tilo_action_broker::collab_broker::BrokerChannels::default(),
            )),
            std::sync::Arc::new(tilo_action_broker::collab_broker::FanoutState::new()),
        )
        .unwrap();
        let driver = CheckpointDriver::certified_with_actions(
            fixture.room,
            &member_key,
            &member,
            broker.app_action_broker().unwrap(),
        );
        let operations = checkpoint_revocable_operations(&routes);
        for (action, arguments, invocation) in &operations {
            driver
                .invoke(action, arguments.clone(), *invocation)
                .unwrap();
            assert!(driver.host.observe(&driver.caller, invocation).is_some());
        }
        let grant =
            checkpoint_revocation_grant(fixture.room, &fixture.certificate, &member, revoke_write);
        let job = driver
            .actions
            .change_workspace_membership(
                &fixture.room.to_string(),
                &WorkspaceMembershipCommand::Grant { grant },
                &fixture.certificate,
            )
            .unwrap();
        let completion = job
            .completion
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        assert!(completion.code.is_empty(), "{}", completion.message);
        for (index, (action, arguments, invocation)) in operations.iter().enumerate() {
            if revoke_write && index >= 2 {
                assert!(
                    driver.host.observe(&driver.caller, invocation).is_some(),
                    "read-only results do not require room naming rights"
                );
                driver
                    .invoke(action, arguments.clone(), *invocation)
                    .unwrap();
                driver
                    .invoke(action, arguments.clone(), tilo_actions::InvocationId::new())
                    .unwrap();
                continue;
            }
            assert!(
                driver.host.observe(&driver.caller, invocation).is_none(),
                "{action} observation must recheck revoked rights"
            );
            assert_checkpoint_admission_refused(
                &driver.host,
                &driver,
                driver.request(action, arguments.clone(), *invocation),
            );
            assert!(
                driver
                    .invoke(action, arguments.clone(), tilo_actions::InvocationId::new())
                    .is_err(),
                "fresh {action} must recheck revoked rights"
            );
            assert!(driver.host.observe(&driver.caller, invocation).is_none());
        }
        drop(driver);
        drop(broker);
        drop(fixture.owner);
        drop(fixture.home);
    }
}

#[test]
fn native_checkpoint_service_rechecks_current_source_rights_for_reads_and_original_retry() {
    use tilo_action_broker::services::WorkspaceMemberships as _;
    let mut fixture = fixture();
    let (_, member) = admit_checkpoint_reader(&mut fixture, false);
    let routes = prepare(&fixture);
    let driver = CheckpointDriver::new(&fixture);
    let capture = tilo_actions::InvocationId::new();
    driver
        .invoke(
            "capture",
            serde_json::json!({"name":"first", "routes":routes}),
            capture,
        )
        .unwrap();
    let service = super::super::NativeOwnerCommitService::new(
        native_workspace_owner::test_tree_of(&fixture.owner),
        None,
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let request = CheckpointRequest {
        room: fixture.room.to_string(),
        invocation: uuid::Uuid::new_v4(),
        caller: member.clone(),
        command: CheckpointCommand::Capture {
            name: "member".into(),
            routes: serde_json::from_value(routes).unwrap(),
        },
    };
    let original = runtime
        .block_on(service.checkpoint(request.clone()))
        .unwrap();
    assert_eq!(
        runtime
            .block_on(service.checkpoint(request.clone()))
            .unwrap(),
        original
    );
    service
        .checkpoint_result_access(&request.room, &member, request.invocation, true)
        .unwrap();
    let actions = fixture.broker.app_action_broker().unwrap();
    let person = tilo_action_policy::Subject::person(hex::encode(member.user_public_key));
    let deny = tilo_action_policy::Grant::new(
        "checkpoint-source-revoked",
        person,
        tilo_action_policy::TreePath::room(fixture.room.to_string()).app("so.tilo.chat"),
        tilo_action_policy::KindSet::READ,
        tilo_action_policy::Answer::Deny,
        tilo_action_policy::Subject::person(hex::encode(fixture.certificate.user_public_key)),
    );
    let job = actions
        .change_workspace_membership(
            &request.room,
            &WorkspaceMembershipCommand::Grant { grant: deny },
            &fixture.certificate,
        )
        .unwrap();
    let completion = job
        .completion
        .recv_timeout(tilo_test_env::hang_bound())
        .unwrap();
    assert!(completion.code.is_empty(), "{}", completion.message);
    assert!(
        runtime
            .block_on(service.checkpoint(request.clone()))
            .is_err(),
        "original retry must recheck source Read"
    );
    assert!(
        service
            .checkpoint_result_access(&request.room, &member, request.invocation, true)
            .is_err()
    );
    for command in [
        CheckpointCommand::Inspect { id: capture.0 },
        CheckpointCommand::Find {
            name: "first".into(),
        },
        CheckpointCommand::NameBind {
            name: "denied".into(),
            id: capture.0,
        },
    ] {
        let mut read = request.clone();
        read.invocation = uuid::Uuid::new_v4();
        read.command = command;
        assert!(runtime.block_on(service.checkpoint(read)).is_err());
    }
    drop(actions);
    drop(runtime);
    drop(service);
    drop(driver);
    drop(fixture);
}

#[test]
fn native_checkpoint_broker_reopen_retries_original_capture_and_rejects_stale_admission() {
    let mut fixture = fixture();
    let (member_key, member) = admit_checkpoint_reader(&mut fixture, true);
    let routes = prepare(&fixture);
    let driver = CheckpointDriver::certified(&fixture, &member_key, &member);
    let invocation = tilo_actions::InvocationId::new();
    let original = driver
        .invoke(
            "capture",
            serde_json::json!({"name":"reopen", "routes":routes}),
            invocation,
        )
        .unwrap();
    let failed = tilo_actions::InvocationId::new();
    let failed_arguments = serde_json::json!({"name":"initially-missing"});
    let original_error = driver
        .invoke("find", failed_arguments.clone(), failed)
        .unwrap_err();
    assert!(matches!(
        driver.host.observe(&driver.caller, &failed).unwrap().update,
        InvocationUpdate::Failed { .. }
    ));
    driver
        .invoke(
            "name-bind",
            serde_json::json!({"name":"initially-missing", "id":invocation.0}),
            tilo_actions::InvocationId::new(),
        )
        .unwrap();
    assert_eq!(
        driver
            .invoke("find", failed_arguments.clone(), failed)
            .unwrap_err(),
        original_error,
        "an exact failed retry must not rerun after the missing name is bound"
    );
    drop(driver);
    drop(fixture.broker);
    crate::shell::persist::wait_for_saves();
    drop(fixture.owner);
    let mut stored = crate::shell::persist::load_tree_from(&fixture.path).unwrap();
    stored.persist_path = Some(fixture.path.clone());
    let tree = stored.shared();
    fixture.owner = native_workspace_owner::open(tree.clone()).unwrap();
    fixture.broker =
        collab_broker::spawn_native(fixture.home.runtime().join("reopened.sock"), &None, tree)
            .unwrap();
    let mut driver = CheckpointDriver::certified(&fixture, &member_key, &member);
    assert_eq!(
        driver
            .invoke(
                "capture",
                serde_json::json!({"name":"reopen", "routes":routes}),
                invocation
            )
            .unwrap(),
        original
    );
    assert_eq!(
        driver.invoke("find", failed_arguments, failed).unwrap_err(),
        original_error,
        "reopening must retain the original failure rather than rerun find"
    );
    assert!(matches!(
        driver.host.observe(&driver.caller, &failed).unwrap().update,
        InvocationUpdate::Failed { .. }
    ));
    assert_eq!(
        driver
            .invoke(
                "find",
                serde_json::json!({"name":"initially-missing"}),
                tilo_actions::InvocationId::new()
            )
            .unwrap(),
        original,
        "a fresh invocation sees the name that was bound after the failed invocation"
    );
    assert_checkpoint_provider_retry_identity(
        &mut driver,
        fixture.room,
        invocation,
        &routes,
        &original,
        (failed, &original_error),
    );
    assert_checkpoint_forged_evidence_refused(&driver, fixture.room, member, &member_key);
    drop(driver);
    drop(fixture);
}

fn assert_checkpoint_forged_evidence_refused(
    driver: &CheckpointDriver,
    room: crate::shell::state::RoomId,
    member: tilo_user_identity::DeviceCert,
    member_key: &ed25519_dalek::SigningKey,
) {
    use ed25519_dalek::Signer as _;
    // A caller cannot obtain the certified host with altered certificate evidence.
    let mut forged = member;
    forged.device_label.push_str(" substituted");
    let room = room.to_string();
    let evidence = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.clone(),
        device_certificate: serde_json::to_vec(&forged).unwrap(),
        proof: member_key
            .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    assert!(
        driver
            .actions
            .certified_local_action_host(&driver.caller, &evidence)
            .is_err()
    );
}

fn assert_checkpoint_provider_retry_identity(
    driver: &mut CheckpointDriver,
    room: crate::shell::state::RoomId,
    invocation: tilo_actions::InvocationId,
    routes: &serde_json::Value,
    original: &serde_json::Value,
    failure: (tilo_actions::InvocationId, &str),
) {
    let (failed, original_error) = failure;
    // Both provider spellings resolve to this service, but changing the
    // spelling is not an exact retry of a retained invocation.
    let canonical_provider = driver.provider.clone();
    driver.provider = wait_for_membership_surface(&driver.actions, room)
        .peer_id
        .to_string();
    assert_ne!(driver.provider, canonical_provider);
    assert_checkpoint_admission_refused(
        &driver.host,
        driver,
        driver.request(
            "capture",
            serde_json::json!({"name":"reopen", "routes":routes}),
            invocation,
        ),
    );
    assert_checkpoint_admission_refused(
        &driver.host,
        driver,
        driver.request(
            "find",
            serde_json::json!({"name":"initially-missing"}),
            failed,
        ),
    );
    assert_eq!(
        driver
            .invoke(
                "find",
                serde_json::json!({"name":"initially-missing"}),
                tilo_actions::InvocationId::new(),
            )
            .unwrap(),
        *original,
        "the alternate provider must work for a fresh invocation"
    );
    driver.provider = canonical_provider;
    assert_eq!(
        driver
            .invoke(
                "capture",
                serde_json::json!({"name":"reopen", "routes":routes}),
                invocation,
            )
            .unwrap(),
        *original,
        "a mismatched retry must not replace the original capture"
    );
    assert_eq!(
        driver
            .invoke(
                "find",
                serde_json::json!({"name":"initially-missing"}),
                failed
            )
            .unwrap_err(),
        original_error,
        "a mismatched retry must not replace the original failure"
    );
}

#[test]
fn native_checkpoint_service_refuses_remote_governed_room_without_substituting_local_keys() {
    use tilo_action_broker::services::WorkspaceMemberships as _;
    let (_directory, owner, room, _, _) = native_workspace_owner::test_enrolled_owner();
    let certificate = owner
        .with_current(room, |view| {
            assert_ne!(view.device(), view.governor());
            Ok(view.device().clone())
        })
        .unwrap();
    let service = super::super::NativeOwnerCommitService::new(
        native_workspace_owner::test_tree_of(&owner),
        None,
    );
    let request = CheckpointRequest {
        room: room.to_string(),
        invocation: uuid::Uuid::new_v4(),
        caller: certificate,
        command: CheckpointCommand::Find {
            name: "remote".into(),
        },
    };
    let error = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(service.checkpoint(request))
        .unwrap_err();
    drop(service);
    assert!(error.contains("actual governing owner"), "{error}");
}

#[test]
fn native_checkpoint_service_refuses_bad_sources_and_missing_custody() {
    use tilo_action_broker::services::WorkspaceMemberships as _;
    let fixture = fixture();
    let service = super::super::NativeOwnerCommitService::new(
        native_workspace_owner::test_tree_of(&fixture.owner),
        None,
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let request = CheckpointRequest {
        room: fixture.room.to_string(),
        invocation: uuid::Uuid::new_v4(),
        caller: fixture.certificate.clone(),
        command: CheckpointCommand::Capture {
            name: "bounded".into(),
            routes: Vec::new(),
        },
    };
    assert!(
        runtime
            .block_on(service.checkpoint(request.clone()))
            .is_err()
    );
    let mut oversized = request.clone();
    oversized.command = CheckpointCommand::Capture {
        name: "too-many".into(),
        routes: (0..33)
            .map(
                |index| tilo_peer_state::workspace_checkpoint::CheckpointRoute {
                    app: "so.tilo.chat".into(),
                    record: format!("record-{index:02}"),
                },
            )
            .collect(),
    };
    assert!(runtime.block_on(service.checkpoint(oversized)).is_err());
    let mut missing = request;
    missing.command = CheckpointCommand::Capture {
        name: "missing".into(),
        routes: vec![tilo_peer_state::workspace_checkpoint::CheckpointRoute {
            app: "so.tilo.chat".into(),
            record: "absent".into(),
        }],
    };
    assert!(
        runtime
            .block_on(service.checkpoint(missing.clone()))
            .is_err()
    );
    let routes = prepare(&fixture);
    let driver = CheckpointDriver::new(&fixture);
    let capture = tilo_actions::InvocationId::new();
    driver
        .invoke(
            "capture",
            serde_json::json!({"name":"custody", "routes":routes}),
            capture,
        )
        .unwrap();
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    std::fs::remove_file(paths.device_cert_for_user(&fixture.certificate.user_public_key)).unwrap();
    assert!(driver.host.observe(&driver.caller, &capture).is_none());
    assert!(
        driver
            .invoke(
                "capture",
                serde_json::json!({"name":"custody", "routes":routes}),
                capture
            )
            .is_err()
    );
    drop(driver);
    let error = runtime.block_on(service.checkpoint(missing)).unwrap_err();
    drop(runtime);
    drop(service);
    assert!(error.contains("custody unavailable"), "{error}");
    drop(fixture);
}
