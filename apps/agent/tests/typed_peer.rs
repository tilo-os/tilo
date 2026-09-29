//! Small app-side peer used by the agent's integration tests.
//!
//! The assertions in these tests predate the native collaboration service,
//! so the adapter deliberately retains their request/event-shaped driving
//! code while putting every byte on the production Cap'n Proto endpoint.

#![allow(
    dead_code,
    reason = "each integration binary uses a different adapter subset"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::WorkspaceAuthority as _;
use std::io;
use std::path::Path;
use std::time::Duration;

use ed25519_dalek::{Signer as _, SigningKey};
use tilo_actions::{ActionAddress, ActionRegistry, LocalTransport, Resource};
use tilo_agent::wire::Link;
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, DocumentMutation, ExportedAction,
    ExportedInput,
};

const TEST_ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x6f2a_5fce_f612_46ac_9228_f6a3_ace4_9a11);
const ROOT_KEY: [u8; 32] = [91; 32];
/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: Duration = Duration::from_secs(30);

pub const fn room() -> uuid::Uuid {
    TEST_ROOM
}

fn device_key(handle: &str) -> SigningKey {
    let mut seed = [0_u8; 32];
    for (index, byte) in handle.bytes().enumerate() {
        seed[index % 32] ^= byte;
    }
    seed[31] ^= 0xA5;
    SigningKey::from_bytes(&seed)
}

/// # Panics
///
/// Panics if the deterministic test certificate or presentation cannot encode.
pub fn endpoint_identity(handle: &str) -> CertifiedEndpointIdentity {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let device = device_key(handle);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: handle.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).expect("root signs test device");
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: TEST_ROOM.to_string(),
        device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &TEST_ROOM.to_string(),
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("certified join encodes");
    CertifiedEndpointIdentity::new(presentation, device)
}

pub struct TypedPeer {
    link: Option<Link>,
    path: std::path::PathBuf,
    timeout: Duration,
}

impl TypedPeer {
    pub fn connect(path: &Path) -> Self {
        Self {
            link: None,
            path: path.to_path_buf(),
            timeout: Duration::from_millis(200),
        }
    }

    /// # Errors
    ///
    /// Returns an error when `request` is not `CollabRequest::Hello`, or when
    /// the typed collaboration client fails to connect to `path`.
    pub fn announce(&mut self, path: &Path, request: &CollabRequest) -> anyhow::Result<()> {
        let CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            record_kind: _,
            handle,
        } = request
        else {
            anyhow::bail!("the first request must announce the peer route")
        };
        self.link = Some(Link::connect_person_endpoint(
            path,
            app_id,
            *cluster_id,
            record.clone(),
            handle.clone(),
            endpoint_identity(handle.as_deref().unwrap_or(app_id)),
        )?);
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when the peer has not yet announced its route via
    /// [`Self::announce`], or when the underlying typed link fails to send.
    pub fn send(&mut self, request: &CollabRequest) -> anyhow::Result<()> {
        self.link
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("peer has not announced its route"))?
            .send(request)
    }

    /// # Errors
    ///
    /// Returns an error when the peer has not yet announced its route via
    /// [`Self::announce`], or when the underlying typed link fails to
    /// receive.
    pub fn recv(&mut self) -> anyhow::Result<Option<CollabEvent>> {
        self.link
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("peer has not announced its route"))?
            .recv(self.timeout)
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout.unwrap_or(Duration::from_millis(200));
    }
}

/// # Errors
///
/// Returns an error when announcing the route fails (for a `Hello` request)
/// or when the peer's underlying send fails.
pub fn send(peer: &mut TypedPeer, request: &CollabRequest) -> io::Result<()> {
    let result = if matches!(request, CollabRequest::Hello { .. }) {
        let path = peer.path.clone();
        peer.announce(&path, request)
    } else {
        peer.send(request)
    };
    result.map_err(io::Error::other)
}

/// # Panics
///
/// Panics if the underlying typed link errors, which in these tests means
/// the collaboration broker has gone away.
pub fn recv(peer: &mut TypedPeer) -> Option<CollabEvent> {
    peer.recv()
        .expect("receive from typed collaboration service")
}

pub fn document_mutation(event: &CollabEvent) -> Option<DocumentMutation> {
    event.document_mutation()?.ok()
}

/// Serialize the same resource descriptors the toolkit publishes for a
/// windowed application.
///
/// # Panics
///
/// Panics if a production action descriptor cannot be serialized for the
/// broker wire, which is an invalid test fixture.
pub fn exported_actions(resources: &[Resource]) -> Vec<ExportedAction> {
    tilo_actions::door::survey(resources)
        .into_iter()
        .map(|descriptor| ExportedAction {
            resource: descriptor.address.resource,
            action: descriptor.address.action,
            resource_kind: descriptor.resource_kind,
            resource_label: descriptor.resource_label,
            action_label: descriptor.action_label,
            description: descriptor.description,
            operation_kind: descriptor.kind.as_str().to_string(),
            params_json: serde_json::to_string(&descriptor.params).unwrap(),
            contract_json: serde_json::to_string(&descriptor.contract).unwrap(),
            view: descriptor.view,
            input: match descriptor.input {
                tilo_actions::InputSpec::None => ExportedInput::None,
                tilo_actions::InputSpec::Text {
                    prompt,
                    placeholder,
                    collects,
                } => ExportedInput::Text {
                    prompt,
                    placeholder,
                    wants_content: collects == tilo_actions::Collects::Content,
                },
            },
        })
        .collect()
}

/// Replace this peer's broker catalogue with a production resource tree.
///
/// # Panics
///
/// Panics if the typed test peer cannot publish to its collaboration broker.
pub fn publish_actions(peer: &mut TypedPeer, app_id: &str, resources: &[Resource]) {
    let actions = exported_actions(resources);
    let expected: Vec<_> = actions
        .iter()
        .map(|action| (action.resource.clone(), action.action.clone()))
        .collect();
    peer.send(&CollabRequest::PublishActions {
        app_id: app_id.to_string(),
        actions,
    })
    .unwrap();

    // Publishing is one-way; ask the action host until it offers what
    // was published before a test launches an agent that plans against it.
    let deadline = tilo_test_env::hang_deadline();
    let mut deferred = Vec::new();
    let mut correlation = 0_u64;
    while ui::time::now() < deadline {
        correlation = correlation.wrapping_add(1);
        let link = peer
            .link
            .as_mut()
            .expect("peer announced before publishing actions");
        link.send(&CollabRequest::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
                correlation,
                query: tilo_actions::remote::CatalogueQuery {
                    provider: None,
                    max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                    ..tilo_actions::remote::CatalogueQuery::default()
                },
            },
        })
        .expect("ask the action host for its catalogue");
        let answered = ui::time::now()
            .checked_add(Duration::from_millis(500))
            .expect("half a second fits in Instant");
        while ui::time::now() < answered {
            match link
                .recv(Duration::from_millis(100))
                .expect("receive the action host's answer")
            {
                Some(CollabEvent::RemoteAction {
                    message:
                        tilo_actions::remote::RemoteActionMessage::CatalogueResponse {
                            correlation: answer,
                            result: Ok(catalogue),
                        },
                }) if answer == correlation => {
                    let offered = expected.iter().all(|(resource, action)| {
                        catalogue.rows.iter().any(|row| {
                            row.app_id == app_id
                                && row.action.as_ref().is_some_and(|action_row| {
                                    action_row.address.resource == resource.as_str()
                                        && action_row.address.action == action.as_str()
                                })
                        })
                    });
                    if offered {
                        link.defer(deferred);
                        return;
                    }
                    break;
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
    }
    peer.link
        .as_mut()
        .expect("peer remains announced")
        .defer(deferred);
    panic!("broker did not publish {app_id}'s action catalogue in time");
}

/// A document invocation as the broker delivers it to the app host.
pub struct DocumentInvocation<'a> {
    pub id: u64,
    pub address: ActionAddress,
    pub argument: Option<&'a str>,
    pub actor: &'a str,
    /// The invocation's own provenance (the calling run's intent group),
    /// recorded with the change exactly as the toolkit host records it.
    pub context: Option<&'a tilo_actions::ActionContext>,
}

/// Resolve and execute a document invocation through an application's real
/// action registry, broadcasting the resulting CRDT delta like the toolkit
/// host. The caller owns when to send the returned terminal update, allowing
/// concurrency tests to interleave another participant first.
///
/// # Panics
///
/// Panics if a successful document action produces no CRDT delta or that
/// delta cannot be sent through the typed test peer.
pub fn service_document_invocation(
    peer: &mut TypedPeer,
    replica: &mut automerge::AutoCommit,
    resources: &[Resource],
    register: fn(&mut ActionRegistry),
    invocation: DocumentInvocation<'_>,
) -> tilo_actions::InvocationUpdate {
    let DocumentInvocation {
        id,
        address,
        argument,
        actor,
        context,
    } = invocation;
    let provenance = tilo_actions::door::InvocationProvenance {
        actor: actor.as_bytes(),
        ambient: context,
    };
    let mut registry = ActionRegistry::new();
    register(&mut registry);
    let mut transport = LocalTransport::new();
    let before = replica.get_heads();
    let result = if let Some(json) =
        argument.and_then(|value| value.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
    {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
            .map_err(|error| tilo_actions::DoorError::InvalidArguments(error.to_string()))
            .and_then(|arguments| {
                tilo_actions::door::invoke_json_confirmed(
                    replica,
                    resources,
                    &address,
                    &arguments,
                    provenance,
                    &registry,
                    &mut transport,
                )
            })
    } else {
        tilo_actions::door::invoke_confirmed(
            replica,
            resources,
            &address,
            argument,
            provenance,
            &registry,
            &mut transport,
        )
    };
    let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
    match result {
        Ok(_) => {
            let delta = replica.save_after(&before);
            assert!(!delta.is_empty(), "the invoked document action was a no-op");
            peer.send(&CollabRequest::document_incremental(delta))
                .unwrap();
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            }
        }
        Err(error) => tilo_actions::InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                code: "tilo.test_app_host".into(),
                message: error.to_string(),
                details: None,
                retryable: false,
            },
        },
    }
}

/// # Panics
///
/// Panics if the typed collaboration service cannot bind `path`.
pub fn spawn(path: &Path) -> shell::shell::collab_broker::BrokerHandle {
    spawn_with_agent_actions(
        path,
        &[
            ("so.tilo.word-processor", "align-center"),
            ("so.tilo.word-processor", "delete"),
            ("so.tilo.word-processor", "insert-table"),
            ("so.tilo.word-processor", "place-image"),
            ("dev.tilo.word-processor", "align-center"),
            ("dev.tilo.word-processor", "delete"),
            ("dev.tilo.word-processor", "insert-table"),
            ("dev.tilo.word-processor", "place-image"),
            ("dev.tilo.photos", "rotate-right"),
            ("dev.tilo.photos", "export-edited-content"),
            ("so.tilo.word-processor", "align-center"),
            ("so.tilo.word-processor", "delete"),
            ("so.tilo.word-processor", "insert-table"),
            ("so.tilo.word-processor", "place-image"),
            ("so.tilo.photos", "rotate-right"),
            ("so.tilo.photos", "export-edited-content"),
        ],
    )
}

/// Spawn the production broker harness with precisely the action grants a
/// scenario reviews. Document-write authority is separate so a denied agent
/// can still write a human-readable refusal without discovering the denied
/// action.
///
/// # Panics
///
/// Panics if the deterministic test room cannot be configured or its native
/// collaboration broker cannot bind `path`.
#[allow(
    clippy::too_many_lines,
    reason = "builds the whole agent test room in one place"
)]
pub fn spawn_with_agent_actions(
    path: &Path,
    actions: &[(&str, &str)],
) -> shell::shell::collab_broker::BrokerHandle {
    let parent = path.parent().expect("typed broker socket has a parent");
    shell::shell::collab_broker_capnp::tests::secure_socket_parent(parent)
        .expect("secure typed broker fixture root");
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let collab_types::Principal::Person(principal) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        unreachable!("person key creates a person principal")
    };
    let mut workspace = shell::shell::state::Workspace::new(
        shell::shell::state::WorkspaceId(1),
        "Agent integration room",
    );
    workspace.room = shell::shell::state::RoomId::parse(&TEST_ROOM.to_string()).unwrap();
    workspace.set_owner(principal.clone());
    workspace
        .grant_as(
            &principal,
            ui::permissions::CapOverride::membership(
                ui::permissions::Subject::agent("Scribe"),
                workspace.room.to_string(),
                ui::permissions::Verb::Write,
            ),
        )
        .expect("the harness owner admits the test agent");
    workspace
        .grant_as(
            &principal,
            ui::permissions::CapOverride::grant(
                ui::permissions::Subject::agent("Scribe"),
                ui::permissions::Scope::ResourceType(collab_types::ArtifactKind::Note),
                ui::permissions::Verb::Write,
            ),
        )
        .expect("the harness owner lets the test agent answer in documents");
    for (app, action) in actions {
        workspace
            .grant_as(
                &principal,
                ui::permissions::CapOverride::grant(
                    ui::permissions::Subject::agent("Scribe"),
                    ui::permissions::Scope::Action {
                        app: (*app).to_string(),
                        action: (*action).to_string(),
                    },
                    ui::permissions::Verb::Write,
                ),
            )
            .expect("the harness owner grants a reviewed test action");
    }
    // App fixtures use the same agent-only endpoint as Scribe. Device
    // enrollment, room membership, and exact app authority remain separate.
    for host in ["quinten", "photos", "files"] {
        workspace
            .grant_as(
                &principal,
                ui::permissions::CapOverride::membership(
                    ui::permissions::Subject::agent(host),
                    workspace.room.to_string(),
                    ui::permissions::Verb::Write,
                ),
            )
            .expect("the harness owner admits an app fixture");
    }
    // Integration fixtures exercise the canonical routes and the dev.tilo test
    // routes. Grant each observed route explicitly: the production capability
    // check deliberately does not alias app authority.
    for (host, app) in [
        ("quinten", "dev.tilo.word-processor"),
        ("quinten", "so.tilo.word-processor"),
        ("photos", "dev.tilo.photos"),
        ("photos", "so.tilo.photos"),
        ("files", "so.tilo.file-explorer"),
    ] {
        workspace
            .grant_as(
                &principal,
                ui::permissions::CapOverride::grant(
                    ui::permissions::Subject::agent(host),
                    ui::permissions::Scope::App(app.to_string()),
                    ui::permissions::Verb::Write,
                ),
            )
            .expect("the harness owner grants an app fixture its own surface");
    }
    let mut tree = shell::shell::state::WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let trusted_devices = ["quinten", "Scribe", "files", "photos"]
        .map(|handle| device_key(handle).verifying_key().to_bytes())
        .into_iter()
        .collect();
    let broker = shell::shell::collab_broker::spawn_native_harness(
        path.to_path_buf(),
        tree.shared(),
        trusted_devices,
    )
    .expect("bind the typed collaboration service");
    // The fixture apps read the catalogue to see their own publication land,
    // as an app declaring `[actions] query` may.
    broker
        .app_action_broker()
        .expect("the harness serves actions")
        .set_app_action_access(std::sync::Arc::new(|_: &str| {
            shell::shell::app_action_access::ActionAccess {
                query: true,
                invoke: false,
            }
        }));
    broker
}
