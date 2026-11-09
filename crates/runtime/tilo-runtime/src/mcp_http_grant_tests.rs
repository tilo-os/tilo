//! One endpoint for every workspace, and sessions opened by a grant
//! certificate the person certified — never by the bearer token alone.
use super::*;
use crate::remote_door::Refusal;
use ed25519_dalek::SigningKey;
use std::sync::Mutex as StdMutex;
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
    sign_device_cert, sign_revocation_entry,
};

const WORKSPACE_A: &str = "0b0c8f0e-6f4a-4f2f-9d1c-2f2b0a5e11aa";
const WORKSPACE_B: &str = "1c1d9f1f-7a5b-4f3f-8e2d-3f3c1b6f22bb";
const HOUR_NS: u64 = 3_600_000_000_000;
const TWO_HOURS_NS: u64 = 7_200_000_000_000;

/// One person's identity root, which certifies the agent grants.
struct Person(SigningKey);

impl Person {
    fn new() -> Self {
        Self(SigningKey::from_bytes(&[7; 32]))
    }

    fn root(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    /// The key one agent's grant certifies.
    fn agent_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    /// A grant certificate for `scope` that holds for an hour from now.
    fn grant(&self, key: &SigningKey, scope: AgentScope) -> DeviceCert {
        let now = tilo_clock::unix_now_ns();
        self.cert(
            key,
            DeviceCapabilities::agent_grant(scope),
            now,
            Some(now.saturating_add(HOUR_NS)),
        )
    }

    /// A certificate of `key` with `capabilities`, signed by this person.
    fn cert(
        &self,
        key: &SigningKey,
        capabilities: DeviceCapabilities,
        issued_at_ns: u64,
        expires_at_ns: Option<u64>,
    ) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: self.root(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "agent grant".into(),
            capabilities,
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &self.0).unwrap();
        cert
    }
}

/// The runtime's own store of the person's devices, as these tests give it:
/// every certificate of the person that holds now, and the revocations the
/// tests add.
struct Devices {
    root: [u8; 32],
    this_device: DeviceCert,
    revoked: StdMutex<RevocationList>,
}

fn devices_of(person: &Person) -> Arc<Devices> {
    let runtime = SigningKey::from_bytes(&[9; 32]);
    Arc::new(Devices {
        root: person.root(),
        this_device: person.cert(
            &runtime,
            DeviceCapabilities::member(),
            tilo_clock::unix_now_ns(),
            None,
        ),
        revoked: StdMutex::new(RevocationList::new()),
    })
}

impl Devices {
    fn revoke(&self, person: &Person, device: &[u8; 32]) {
        let mut entry = RevocationEntry {
            user_public_key: person.root(),
            revoked_device_public_key: *device,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &person.0).unwrap();
        self.revoked.lock().unwrap().insert(entry).unwrap();
    }
}

impl PersonsDevices for Devices {
    fn this_device(&self) -> DeviceCert {
        self.this_device.clone()
    }

    fn devices(&self) -> Vec<DeviceCert> {
        vec![self.this_device.clone()]
    }

    fn admit(&self, dialer: [u8; 32], certificate: &DeviceCert) -> Result<(), Refusal> {
        if certificate.device_public_key != dialer {
            return Err(Refusal::NotTheDialer);
        }
        if certificate.user_public_key != self.root {
            return Err(Refusal::OtherPerson);
        }
        tilo_user_identity::verify_device_cert_at(certificate, tilo_clock::unix_now_ns())
            .map_err(|error| Refusal::NotCertified(error.to_string()))?;
        if self.revoked.lock().unwrap().is_revoked(&dialer) {
            return Err(Refusal::Revoked);
        }
        Ok(())
    }

    fn revocations(&self) -> RevocationList {
        self.revoked.lock().unwrap().clone()
    }
}

/// A runtime serving every workspace behind one bearer token, with the
/// person's grants deciding who opens a session.
fn endpoint_for(devices: Arc<Devices>, joiner: Option<crate::Joiner>) -> Arc<Endpoint<StubHost>> {
    Endpoint::hosted(
        StubHost::default(),
        "owner".into(),
        None,
        Some("secret".into()),
        Some(devices),
        joiner,
    )
}

fn path(workspace: &str) -> String {
    format!("/w/{workspace}/mcp")
}

/// The challenge an opening request is told to answer.
fn challenge_of(response: &Response<Full<Bytes>>) -> [u8; 32] {
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let text = response
        .headers()
        .get(GRANT_CHALLENGE_HEADER)
        .expect("a challenge to answer")
        .to_str()
        .unwrap();
    assert_eq!(text.len(), 64, "{text}");
    assert!(
        text.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{text}"
    );
    let bytes: [u8; 32] = hex::decode(text).unwrap().try_into().unwrap();
    bytes
}

/// What a caller that holds `cert`'s key answers `challenge` on `workspace`
/// with.
fn answer(cert: &DeviceCert, key: &SigningKey, workspace: &str, challenge: [u8; 32]) -> String {
    let proof = SignInProof::sign(
        cert.clone(),
        key,
        &agent_endpoint_audience(workspace),
        challenge,
    )
    .unwrap();
    base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        canonical::canonical_bytes(&proof).unwrap(),
    )
}

/// Open a session in `workspace`, answering the challenge the endpoint
/// issues with `cert`'s grant: the session id, or the status that refused it.
async fn open_granted(
    endpoint: &Arc<Endpoint<StubHost>>,
    workspace: &str,
    cert: &DeviceCert,
    key: &SigningKey,
) -> Result<String, StatusCode> {
    let opened = endpoint
        .dispatch(
            &Method::POST,
            &path(workspace),
            &bearer_headers(&[]),
            &initialize("granted agent"),
        )
        .await;
    let value = answer(cert, key, workspace, challenge_of(&opened));
    let opened = endpoint
        .dispatch(
            &Method::POST,
            &path(workspace),
            &bearer_headers(&[("tilo-grant", &value)]),
            &initialize("granted agent"),
        )
        .await;
    if opened.status() != StatusCode::OK {
        return Err(opened.status());
    }
    let id = opened
        .headers()
        .get(SESSION_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    Ok(id)
}

/// One request on an open session, in `workspace`.
async fn list(endpoint: &Arc<Endpoint<StubHost>>, workspace: &str, session: &str) -> StatusCode {
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}).to_string();
    endpoint
        .dispatch(
            &Method::POST,
            &path(workspace),
            &bearer_headers(&[(SESSION_HEADER, session)]),
            call.as_bytes(),
        )
        .await
        .status()
}

/// Join `workspace` through `request`, answering the challenge the endpoint
/// issues: the status, and the body as JSON (or as the text it was).
async fn join(
    endpoint: &Arc<Endpoint<StubHost>>,
    workspace: &str,
    cert: &DeviceCert,
    key: &SigningKey,
    request: &Value,
) -> Value {
    let invited = format!("/w/{workspace}/join");
    let opened = endpoint
        .dispatch(
            &Method::POST,
            &invited,
            &bearer_headers(&[]),
            request.to_string().as_bytes(),
        )
        .await;
    let value = answer(cert, key, workspace, challenge_of(&opened));
    let joined = endpoint
        .dispatch(
            &Method::POST,
            &invited,
            &bearer_headers(&[("tilo-grant", &value)]),
            request.to_string().as_bytes(),
        )
        .await;
    let status = joined.status();
    let bytes = joined.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let answer = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
    json!({"status": status.as_u16(), "body": answer})
}

#[test]
fn one_endpoint_serves_every_workspace_and_binds_sessions_to_theirs() {
    let person = Person::new();
    let devices = devices_of(&person);
    let endpoint = endpoint_for(Arc::clone(&devices), None);
    let key = Person::agent_key(11);
    let cert = person.grant(&key, AgentScope::AllWorkspaces);
    run(async {
        let a = open_granted(&endpoint, WORKSPACE_A, &cert, &key)
            .await
            .unwrap();
        let b = open_granted(&endpoint, WORKSPACE_B, &cert, &key)
            .await
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(endpoint.session_count(), 2);
        assert_eq!(list(&endpoint, WORKSPACE_A, &a).await, StatusCode::OK);
        assert_eq!(list(&endpoint, WORKSPACE_B, &b).await, StatusCode::OK);
        // A session is bound to the workspace it was opened in.
        assert_eq!(
            list(&endpoint, WORKSPACE_B, &a).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            list(&endpoint, WORKSPACE_A, &b).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(endpoint.session_count(), 2);
    });
}

#[test]
fn opening_a_session_needs_a_proof_and_gets_a_fresh_challenge() {
    let person = Person::new();
    let endpoint = endpoint_for(devices_of(&person), None);
    run(async {
        let first = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[]),
                &initialize("agent"),
            )
            .await;
        let second = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[]),
                &initialize("agent"),
            )
            .await;
        assert_ne!(challenge_of(&first), challenge_of(&second));
        // Without the token there is no challenge either: the door first.
        let bare = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &HeaderMap::new(),
                &initialize("agent"),
            )
            .await;
        assert_eq!(bare.status(), StatusCode::UNAUTHORIZED);
        assert!(bare.headers().get(GRANT_CHALLENGE_HEADER).is_none());
        assert_eq!(endpoint.session_count(), 0);
    });
}

#[test]
fn a_proof_for_another_workspace_answers_no_challenge_here() {
    let person = Person::new();
    let endpoint = endpoint_for(devices_of(&person), None);
    let key = Person::agent_key(11);
    let cert = person.grant(&key, AgentScope::AllWorkspaces);
    run(async {
        let opened = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[]),
                &initialize("agent"),
            )
            .await;
        let challenge = challenge_of(&opened);
        // Signed for B's endpoint, answered at A's.
        let value = answer(&cert, &key, WORKSPACE_B, challenge);
        let refused = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[("tilo-grant", &value)]),
                &initialize("agent"),
            )
            .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_eq!(endpoint.session_count(), 0);
    });
}

#[test]
fn a_challenge_answers_one_request_whatever_its_outcome() {
    let person = Person::new();
    let endpoint = endpoint_for(devices_of(&person), None);
    let key = Person::agent_key(11);
    let cert = person.grant(&key, AgentScope::AllWorkspaces);
    run(async {
        let opened = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[]),
                &initialize("agent"),
            )
            .await;
        let value = answer(&cert, &key, WORKSPACE_A, challenge_of(&opened));
        let first = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[("tilo-grant", &value)]),
                &initialize("agent"),
            )
            .await;
        assert_eq!(first.status(), StatusCode::OK);
        let replayed = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[("tilo-grant", &value)]),
                &initialize("agent"),
            )
            .await;
        assert_eq!(replayed.status(), StatusCode::FORBIDDEN);
        assert_eq!(endpoint.session_count(), 1);
    });
}

#[test]
fn an_unknown_or_expired_challenge_is_refused() {
    let person = Person::new();
    let endpoint = endpoint_for(devices_of(&person), None);
    let key = Person::agent_key(11);
    let cert = person.grant(&key, AgentScope::AllWorkspaces);
    run(async {
        // Never issued here.
        let invented = answer(&cert, &key, WORKSPACE_A, [3; 32]);
        let refused = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[("tilo-grant", &invented)]),
                &initialize("agent"),
            )
            .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        // Issued, but not answered in time.
        let opened = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[]),
                &initialize("agent"),
            )
            .await;
        let challenge = challenge_of(&opened);
        let value = answer(&cert, &key, WORKSPACE_A, challenge);
        let stale = Instant::now()
            .checked_sub(GRANT_CHALLENGE_LIFETIME + Duration::from_secs(1))
            .unwrap();
        endpoint
            .challenges
            .0
            .lock()
            .unwrap()
            .insert(challenge, stale);
        let refused = endpoint
            .dispatch(
                &Method::POST,
                &path(WORKSPACE_A),
                &bearer_headers(&[("tilo-grant", &value)]),
                &initialize("agent"),
            )
            .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_eq!(endpoint.session_count(), 0);
    });
}

#[test]
fn only_a_holding_grant_of_this_person_that_reaches_the_workspace_opens_a_session() {
    let person = Person::new();
    let devices = devices_of(&person);
    let endpoint = endpoint_for(Arc::clone(&devices), None);
    let key = Person::agent_key(11);
    let now = tilo_clock::unix_now_ns();
    let expired = person.cert(
        &key,
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        now.saturating_sub(TWO_HOURS_NS),
        Some(now.saturating_sub(HOUR_NS)),
    );
    let elsewhere = person.grant(&key, AgentScope::parse(WORKSPACE_B).unwrap());
    let no_grant = person.cert(&key, DeviceCapabilities::member(), now, None);
    let revoked_key = Person::agent_key(12);
    let revoked = person.grant(&revoked_key, AgentScope::AllWorkspaces);
    devices.revoke(&person, &revoked_key.verifying_key().to_bytes());
    let other_person = Person(SigningKey::from_bytes(&[6; 32]));
    let foreign = other_person.grant(&key, AgentScope::AllWorkspaces);
    run(async {
        for (what, cert, key) in [
            ("expired", &expired, &key),
            ("reaches another workspace", &elsewhere, &key),
            ("not an agent's grant", &no_grant, &key),
            ("revoked", &revoked, &revoked_key),
            ("another person's", &foreign, &key),
        ] {
            assert_eq!(
                open_granted(&endpoint, WORKSPACE_A, cert, key).await,
                Err(StatusCode::FORBIDDEN),
                "{what}"
            );
        }
        assert_eq!(endpoint.session_count(), 0);
        // The grant that reaches it opens it.
        let reaches = person.grant(&key, AgentScope::AllWorkspaces);
        assert!(
            open_granted(&endpoint, WORKSPACE_A, &reaches, &key)
                .await
                .is_ok()
        );
    });
}

#[test]
fn revoking_the_grant_after_open_drops_its_session() {
    let person = Person::new();
    let devices = devices_of(&person);
    let endpoint = endpoint_for(Arc::clone(&devices), None);
    let key = Person::agent_key(11);
    let cert = person.grant(&key, AgentScope::AllWorkspaces);
    run(async {
        let session = open_granted(&endpoint, WORKSPACE_A, &cert, &key)
            .await
            .unwrap();
        assert_eq!(list(&endpoint, WORKSPACE_A, &session).await, StatusCode::OK);
        devices.revoke(&person, &key.verifying_key().to_bytes());
        assert_eq!(
            list(&endpoint, WORKSPACE_A, &session).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(endpoint.session_count(), 0);
        // The door initializes again, and is refused there.
        assert_eq!(
            open_granted(&endpoint, WORKSPACE_A, &cert, &key).await,
            Err(StatusCode::FORBIDDEN)
        );
    });
}

#[test]
fn the_bearer_token_alone_reaches_no_workspace() {
    let person = Person::new();
    let endpoint = endpoint_for(devices_of(&person), None);
    let key = Person::agent_key(11);
    let cert = person.grant(&key, AgentScope::AllWorkspaces);
    run(async {
        let refused = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &bearer_headers(&[]),
                &initialize("agent"),
            )
            .await;
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);
        let health = endpoint
            .dispatch(&Method::GET, "/health", &HeaderMap::new(), b"")
            .await;
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(endpoint.session_count(), 0);
        assert!(
            open_granted(&endpoint, WORKSPACE_A, &cert, &key)
                .await
                .is_ok()
        );
    });
}

#[test]
fn a_workspace_path_must_name_one_canonical_workspace() {
    let person = Person::new();
    let endpoint = endpoint_for(devices_of(&person), None);
    let upper = WORKSPACE_A.to_uppercase();
    run(async {
        for route in [
            "/w//mcp".to_owned(),
            "/w/00000000-0000-0000-0000-000000000000/mcp".to_owned(),
            format!("/w/{upper}/mcp"),
            format!("/w/{WORKSPACE_A}"),
            format!("/w/{WORKSPACE_A}/other"),
            format!("/w/{WORKSPACE_A}/mcp/x"),
        ] {
            let response = endpoint
                .dispatch(
                    &Method::POST,
                    &route,
                    &bearer_headers(&[]),
                    &initialize("agent"),
                )
                .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{route}");
        }
    });
}

#[test]
fn joining_needs_a_grant_that_makes_workspaces() {
    let person = Person::new();
    let invitations = Arc::new(StdMutex::new(Vec::new()));
    let recorded = Arc::clone(&invitations);
    let joiner: crate::Joiner = Arc::new(move |invitation: &str| {
        recorded.lock().unwrap().push(invitation.to_owned());
        Ok(WORKSPACE_A.to_owned())
    });
    let endpoint = endpoint_for(devices_of(&person), Some(joiner));
    let key = Person::agent_key(11);
    let all = person.grant(&key, AgentScope::AllWorkspaces);
    let one = person.grant(&key, AgentScope::parse(WORKSPACE_A).unwrap());
    let request = json!({"invitation": "an invitation"});
    run(async {
        let reply = join(&endpoint, WORKSPACE_A, &all, &key, &request).await;
        assert_eq!(reply["status"], json!(200), "{reply}");
        assert_eq!(reply["body"]["workspace"], WORKSPACE_A);
        let asked = invitations.lock().unwrap().clone();
        assert_eq!(asked, ["an invitation"]);
        // One workspace's grant may not make one.
        let refused = join(&endpoint, WORKSPACE_A, &one, &key, &request).await;
        assert_eq!(refused["status"], json!(403), "{refused}");
        assert_eq!(invitations.lock().unwrap().len(), 1);
        // A join without a proof is told to answer a challenge instead.
        let opened = endpoint
            .dispatch(
                &Method::POST,
                &format!("/w/{WORKSPACE_A}/join"),
                &bearer_headers(&[]),
                request.to_string().as_bytes(),
            )
            .await;
        challenge_of(&opened);
        assert_eq!(invitations.lock().unwrap().len(), 1);
    });
}

#[test]
fn an_invitation_for_another_workspace_or_a_refused_join_is_refused() {
    let person = Person::new();
    let elsewhere: crate::Joiner = Arc::new(|_: &str| Ok(WORKSPACE_B.to_owned()));
    let endpoint = endpoint_for(devices_of(&person), Some(elsewhere));
    let key = Person::agent_key(11);
    let all = person.grant(&key, AgentScope::AllWorkspaces);
    let request = json!({"invitation": "an invitation"});
    run(async {
        let refused = join(&endpoint, WORKSPACE_A, &all, &key, &request).await;
        assert_eq!(refused["status"], json!(403), "{refused}");
        assert!(!endpoint.serves(WORKSPACE_A));
    });

    let failing: crate::Joiner =
        Arc::new(|_: &str| Err(std::io::Error::other("no such invitation")));
    let endpoint = endpoint_for(devices_of(&person), Some(failing));
    run(async {
        let refused = join(&endpoint, WORKSPACE_A, &all, &key, &request).await;
        assert_eq!(refused["status"], json!(403), "{refused}");
        assert_eq!(refused["body"], json!("no such invitation"));
    });
}

#[test]
fn joining_a_workspace_already_here_answers_it() {
    let person = Person::new();
    let calls = Arc::new(StdMutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let joiner: crate::Joiner = Arc::new(move |invitation: &str| {
        let mut calls = recorded.lock().unwrap();
        calls.push(invitation.to_owned());
        // The first join enrolls; the host then refuses the second, as an
        // already-enrolled workspace.
        if calls.len() == 1 {
            Ok(WORKSPACE_A.to_owned())
        } else {
            Err(std::io::Error::other("already enrolled"))
        }
    });
    let endpoint = endpoint_for(devices_of(&person), Some(joiner));
    let key = Person::agent_key(11);
    let cert = person.grant(&key, AgentScope::AllWorkspaces);
    let request = json!({"invitation": "an invitation"});
    run(async {
        let reply = join(&endpoint, WORKSPACE_A, &cert, &key, &request).await;
        assert_eq!(reply["status"], json!(200), "{reply}");
        let again = join(&endpoint, WORKSPACE_A, &cert, &key, &request).await;
        assert_eq!(again["status"], json!(200), "{again}");
        assert_eq!(again["body"]["workspace"], WORKSPACE_A);
        assert_eq!(calls.lock().unwrap().len(), 2);
    });
}

#[test]
fn a_joined_workspace_is_served_by_the_next_initialize() {
    let person = Person::new();
    let enrolled = Arc::new(StdMutex::new(Vec::<String>::new()));
    let enlisted = Arc::clone(&enrolled);
    let joiner: crate::Joiner = Arc::new(move |_: &str| {
        enlisted.lock().unwrap().push(WORKSPACE_B.to_owned());
        Ok(WORKSPACE_B.to_owned())
    });
    let endpoint = endpoint_for(devices_of(&person), Some(joiner));
    let admitted = Arc::clone(&enrolled);
    endpoint
        .serve_workspaces(Arc::new(move |workspace: &str| {
            if !admitted
                .lock()
                .unwrap()
                .iter()
                .any(|joined| joined == workspace)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "no such workspace here",
                ));
            }
            let workspace = workspace.to_owned();
            Ok(Arc::new(move || {
                Ok(McpServer::new(
                    StubHost::default(),
                    workspace.clone(),
                    "owner".into(),
                ))
            }))
        }))
        .unwrap();
    let key = Person::agent_key(11);
    let cert = person.grant(&key, AgentScope::AllWorkspaces);
    run(async {
        // Before the join, this runtime serves no such workspace.
        assert_eq!(
            open_granted(&endpoint, WORKSPACE_B, &cert, &key).await,
            Err(StatusCode::SERVICE_UNAVAILABLE)
        );
        let reply = join(
            &endpoint,
            WORKSPACE_B,
            &cert,
            &key,
            &json!({"invitation": "an invitation"}),
        )
        .await;
        assert_eq!(reply["status"], json!(200), "{reply}");
        assert!(
            open_granted(&endpoint, WORKSPACE_B, &cert, &key)
                .await
                .is_ok()
        );
    });
}

#[test]
fn grants_required_is_read_from_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token");
    std::fs::write(&token, "synthetic-token\n").unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(ADDRESS_ENV, "127.0.0.1:7393");
    env.set(TOKEN_FILE_ENV, &token);
    env.remove(GRANTS_ENV);
    let config = Config::from_env().unwrap().expect("an endpoint");
    assert_eq!(config.grants, Grants::Optional);
    env.set(GRANTS_ENV, "required");
    let config = Config::from_env().unwrap().expect("an endpoint");
    assert_eq!(config.grants, Grants::Required);
    env.set(GRANTS_ENV, "sometimes");
    assert!(Config::from_env().is_err());
}
