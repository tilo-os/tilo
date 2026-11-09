//! A person's host and runtime on loopback, for the door's tests and the
//! local door ([`crate::local`]): a real `WorkspaceHost`, and the real
//! `tilo-runtime` HTTP endpoint serving every workspace behind one address
//! and checking each grant under the person's root and revocations, over an
//! in-memory action host that publishes one action, so what an agent run
//! does shows up as History records it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture: what it cannot set up is the failure it reports"
)]

use crate::directory::Route;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionHostError, CatalogueProvider, CatalogueResource, CatalogueRow, TestHost,
};
use tilo_runtime::remote_door::{PersonsDevices, Refusal};
use tilo_user_identity::{DeviceCert, RevocationList};

/// A person's runtime: one `tilo-runtime` HTTP endpoint on loopback serving
/// every workspace, behind the door's token, checking grants under the
/// person's root and revocations.
pub struct Runtime {
    pub base: String,
    pub token: String,
    pub host: Arc<TestHost>,
    pub revoked: Arc<Mutex<RevocationList>>,
    /// The invitations the runtime was asked to join with.
    pub joined: Arc<Mutex<Vec<(String, String)>>>,
}

impl Runtime {
    /// Where the door reaches it.
    #[must_use]
    pub fn route(&self) -> Route {
        Route {
            base: self.base.clone(),
            token: self.token.clone(),
        }
    }
}

/// The tool an agent acts with, and the arguments that apply the one
/// action the host publishes in `workspace`.
pub const ACTION_TOOL: &str = "tilo_invoke";

pub fn action_arguments(workspace: &str) -> Value {
    json!({
        "workspace": workspace,
        "peer_id": 71,
        "revision": "1",
        "resource": "test.note:1",
        "action": tilo_resource::APPLY,
        "arguments": {"action": {"rename": {"title": "Plan"}}},
    })
}

mod note {
    use tilo_resource::{Model, resource};

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Note {
        pub title: String,
    }

    #[resource(kind = "test.note", label = "Note")]
    impl Note {
        /// Rename the note.
        #[action]
        pub fn rename(&mut self, #[param("New title")] title: String) {
            self.title = title;
        }
    }
}

fn row() -> CatalogueRow {
    let resource =
        tilo_resource::publish::<note::Note>("test.note:1", "Note", &note::Note::default());
    let descriptor = tilo_actions::door::survey(&[resource])
        .into_iter()
        .find(|descriptor| descriptor.address.action == tilo_resource::APPLY)
        .unwrap();
    CatalogueRow {
        provider: "71".into(),
        app_id: "so.tilo.test".into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: "test.note:1".into(),
            kind: "test.note".into(),
            label: "Note".into(),
            presentation: None,
        },
        action: Some(descriptor),
        has_children: false,
        visibility: None,
    }
}

/// The person's devices as the runtime knows them: their root and the
/// revocations it signed.
struct Devices {
    root: [u8; 32],
    revoked: Arc<Mutex<RevocationList>>,
}

impl PersonsDevices for Devices {
    fn this_device(&self) -> DeviceCert {
        DeviceCert {
            user_public_key: self.root,
            device_public_key: [0; 32],
            device_label: "Runtime".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        }
    }

    fn devices(&self) -> Vec<DeviceCert> {
        Vec::new()
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
        if self
            .revoked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .refuses(certificate)
        {
            return Err(Refusal::Revoked);
        }
        Ok(())
    }
}

/// The runtime of the person whose root is `root`, on loopback, answering
/// the classic tools.
#[cfg(test)]
pub(crate) async fn runtime(root: [u8; 32]) -> Runtime {
    serve_runtime(root, false).await
}

/// The runtime of the person whose root is `root`, on loopback: answering the
/// agent tools (`tilo_ls`, `tilo_do`, …) when `agent`, else the classic ones.
///
/// # Panics
/// Loopback cannot be bound.
pub async fn serve_runtime(root: [u8; 32], agent: bool) -> Runtime {
    let host = Arc::new(
        TestHost::new()
            .with_catalogue(|caller, query| {
                let workspace = caller
                    .workspace()
                    .ok_or(ActionHostError::NotPermitted)?
                    .to_owned();
                let mut page =
                    tilo_actions::remote::CataloguePage::from_rows("1".into(), vec![row()], &query);
                page.providers = vec![CatalogueProvider {
                    provider: "71".into(),
                    app_id: "so.tilo.test".into(),
                    workspace,
                    record_id: String::new(),
                    browsable: true,
                }];
                Ok(page)
            })
            .completing(|_| tilo_actions::ActionOutput::default()),
    );
    let revoked = Arc::new(Mutex::new(RevocationList::new()));
    let joins: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let token = format!("door-token-{}", hex::encode(&root[..4]));
    let joiner: tilo_runtime::Joiner = {
        let joins = Arc::clone(&joins);
        Arc::new(move |invitation: &str| {
            let join = tilo_workspace_join::decode_and_verify_invitation_text(
                invitation,
                Some(root),
                &RevocationList::new(),
                tilo_clock::unix_now_ns(),
            )
            .map_err(std::io::Error::other)?;
            let workspace = join.grant().room.clone();
            joins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((workspace.clone(), invitation.to_owned()));
            Ok(workspace)
        })
    };
    let endpoint = tilo_runtime::mcp_http::Endpoint::hosted(
        Arc::clone(&host),
        "owner".to_owned(),
        None,
        Some(token.clone()),
        Some(Arc::new(Devices {
            root,
            revoked: Arc::clone(&revoked),
        })),
        Some(joiner),
    );
    if agent {
        let served = Arc::clone(&host);
        let resolve: tilo_runtime::mcp_http::SessionResolver<Arc<TestHost>> =
            Arc::new(move |workspace: &str| {
                let (host, workspace) = (Arc::clone(&served), workspace.to_owned());
                let factory: tilo_runtime::mcp::SessionFactory<Arc<TestHost>> =
                    Arc::new(move || {
                        Ok(tilo_runtime::mcp::McpServer::for_person(
                            Arc::clone(&host),
                            workspace.clone(),
                            "owner",
                        ))
                    });
                Ok(factory)
            });
        let _ = endpoint.serve_workspaces(resolve);
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let endpoint = Arc::clone(&endpoint);
            tokio::spawn(async move {
                let service = service_fn(move |request| {
                    let endpoint = Arc::clone(&endpoint);
                    async move { Ok::<_, std::convert::Infallible>(endpoint.respond(request).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    Runtime {
        base,
        token,
        host,
        revoked,
        joined: joins,
    }
}

/// A device of the person: its key and the certificate their root gave it.
pub struct Device {
    pub key: ed25519_dalek::SigningKey,
    pub cert: DeviceCert,
}

/// A member device of the person whose root is `root`, its key from `seed`.
///
/// # Panics
/// The root cannot sign the certificate.
#[must_use]
pub fn enrol(root: &ed25519_dalek::SigningKey, seed: u8, label: &str) -> Device {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 200,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("the root signs its device");
    Device { key, cert }
}

/// The person's host, serving on a fresh endpoint until shut down.
pub struct Served {
    pub host: tilo_room_host::host::WorkspaceHost,
    router: iroh::protocol::Router,
    pub device: Device,
    _state: tempfile::TempDir,
}

/// Serve a host of the person whose root is `root`.
///
/// # Panics
/// The host cannot be opened or bound.
pub async fn serve(root: &ed25519_dalek::SigningKey) -> Served {
    use tilo_room_host::host::{HostConfig, HostIdentity, HostMode, WorkspaceHost};
    let device = enrol(root, 11, "Home node");
    let state = tempfile::tempdir().expect("a scratch directory");
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.key.to_bytes()))
        .bind()
        .await
        .expect("loopback binds");
    let host = WorkspaceHost::open(
        HostConfig::new(state.path().join("host"), HostMode::Device),
        HostIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: device.key.clone(),
            device_cert: device.cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
        None,
    )
    .await
    .expect("the host opens");
    let mut router = iroh::protocol::Router::builder(endpoint);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    Served {
        host,
        router: router.spawn(),
        device,
        _state: state,
    }
}

impl Served {
    /// Stop serving.
    pub async fn shutdown(self) {
        let _ = self.router.shutdown().await;
        self.host.shutdown().await;
    }
}
