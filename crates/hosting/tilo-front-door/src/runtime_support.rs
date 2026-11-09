//! A person's runtime for the tests: the real `tilo-runtime` HTTP endpoint,
//! serving every workspace behind one address and checking each grant
//! under the person's root and revocations, over an in-memory action host
//! that publishes one action, so what an agent run does shows up as History
//! records it.

use crate::tests::Runtime;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionHostError, CatalogueProvider, CatalogueResource, CatalogueRow, TestHost,
};
use tilo_runtime::remote_door::{PersonsDevices, Refusal};
use tilo_user_identity::{DeviceCert, RevocationList};

/// The tool an agent acts with, and the arguments that apply the one
/// action the host publishes in `workspace`.
pub(crate) const ACTION_TOOL: &str = "tilo_invoke";

pub(crate) fn action_arguments(workspace: &str) -> Value {
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

/// The runtime of the person whose root is `root`, on loopback.
pub(crate) async fn runtime(root: [u8; 32]) -> Runtime {
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
