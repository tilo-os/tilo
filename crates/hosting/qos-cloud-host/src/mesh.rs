//! Explicit bilateral grants for bounded Actions; no peer, root or room mutation.
use super::*;
use qos_user_identity::machine_mesh::{MeshChallenge, MeshRequest, verify_request};
pub const MESH_ALPN: &[u8] = b"qos-cloud-mesh-actions/1";
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct MeshState {
    pub(super) challenge: Option<MeshChallenge>,
    links: BTreeMap<String, Link>,
    consumed: BTreeMap<String, (String, serde_json::Value)>,
    #[serde(default)]
    pub(super) pending_scopes: BTreeMap<String, String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Link {
    grant: MeshRequest,
    active: bool,
}
impl Store {
    pub fn mesh_inspect(&mut self, now: u64) -> Result<MeshChallenge> {
        self.ensure_reconciled()?;
        if let Some(c) = &self.state.mesh.challenge {
            if now < c.expires_at_ns {
                return Ok(c.clone());
            }
        }
        let b = self
            .state
            .bound
            .as_ref()
            .ok_or_else(|| anyhow!("workspace not enrolled"))?;
        let relay = std::env::var("QOS_CLOUD_PACKET_RELAY").or_else(|e| {
            if cfg!(test) {
                Ok("https://example.invalid".into())
            } else {
                Err(e)
            }
        })?;
        let address =
            iroh_base::EndpointAddr::new(self.action_key().public()).with_relay_url(relay.parse()?);
        let mut nonce = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let c = MeshChallenge {
            version: 1,
            machine_id: self.state.machine_id.clone(),
            room: self.state.room.clone(),
            root_public_key: b.root,
            host_certificate: b.authority.clone(),
            action_address: serde_json::to_string(&address)?,
            nonce,
            expires_at_ns: now
                .checked_add(300_000_000_000)
                .ok_or_else(|| anyhow!("clock overflow"))?,
        };
        let mut state = self.state.clone();
        state.mesh.challenge = Some(c.clone());
        self.commit(state)?;
        Ok(c)
    }
    pub fn mesh_apply(&mut self, r: &MeshRequest, now: u64) -> Result<serde_json::Value> {
        self.ensure_reconciled()?;
        let local = if r.source.machine_id == self.state.machine_id {
            &r.source
        } else if r.target.machine_id == self.state.machine_id {
            &r.target
        } else {
            bail!("mesh machine binding mismatch")
        };
        let nonce = format!("{}:{}:{}", r.kind, r.link_id, hex::encode(local.nonce));
        let hash = blake3::hash(&serde_json::to_vec(r)?).to_hex().to_string();
        if let Some((prior, result)) = self.state.mesh.consumed.get(&nonce) {
            if prior == &hash {
                if r.kind == "link"
                    && self
                        .state
                        .mesh
                        .links
                        .get(&r.link_id)
                        .is_some_and(|l| !l.active)
                {
                    bail!("mesh link revoked; replay cannot reactivate it")
                }
                return Ok(result.clone());
            }
            bail!("mesh nonce substitution")
        }
        verify_request(r, now).map_err(anyhow::Error::msg)?;
        let b = self
            .state
            .bound
            .as_ref()
            .ok_or_else(|| anyhow!("not enrolled"))?;
        if (r.kind == "link" && self.state.mesh.challenge.as_ref() != Some(local))
            || local.room != self.state.room
            || local.root_public_key != b.root
            || local.host_certificate != b.authority
        {
            bail!("mesh root or challenge mismatch")
        }
        // Parse both signed addresses now; never turn malformed persisted data into a network target.
        for c in [&r.source, &r.target] {
            let _: iroh_base::EndpointAddr = serde_json::from_str(&c.action_address)?;
        }
        let mut state = self.state.clone();
        if r.kind == "link" {
            if let Some(old) = state.mesh.links.get(&r.link_id) {
                if !old.active || !same_members(&old.grant, r) {
                    bail!("mesh link id already used")
                }
            }
            if state.mesh.links.len() >= 32 && !state.mesh.links.contains_key(&r.link_id) {
                bail!("mesh link capacity reached")
            }
            state.mesh.links.insert(
                r.link_id.clone(),
                Link {
                    grant: r.clone(),
                    active: true,
                },
            );
        } else {
            let link = state
                .mesh
                .links
                .get_mut(&r.link_id)
                .ok_or_else(|| anyhow!("mesh link not found"))?;
            if !same_members(&link.grant, r) {
                bail!("mesh unlink member substitution")
            }
            if !link.active {
                return Ok(
                    serde_json::json!({"version":1,"machine_id":self.state.machine_id,"link_id":r.link_id,"active":false}),
                );
            }
            link.active = false;
            let scopes = state
                .mesh
                .pending_scopes
                .iter()
                .filter(|(_, id)| *id == &r.link_id)
                .map(|(scope, _)| scope.clone())
                .collect::<Vec<_>>();
            for scope in scopes {
                state.pending_models.remove(&scope);
                state.mesh.pending_scopes.remove(&scope);
            }
        }
        if r.kind == "link"
            && serde_json::to_vec(&link_status(&state.mesh, &self.state.machine_id))?.len()
                > 16 * 1024
        {
            bail!(
                "mesh lifetime grant storage capacity reached; existing links and unlink remain available"
            )
        }
        if state.mesh.consumed.len() >= 1024 && r.kind == "link" {
            bail!("mesh control journal capacity reached")
        }
        let result = serde_json::json!({"version":1,"machine_id":self.state.machine_id,"link_id":r.link_id,"active":r.kind=="link"});
        state.mesh.challenge = None;
        state.mesh.consumed.insert(nonce, (hash, result.clone()));
        self.commit(state)?;
        Ok(result)
    }
    pub fn mesh_status(&self, now: u64) -> Result<serde_json::Value> {
        self.ensure_reconciled()?;
        let available = gateway::Gateway::from_environment()
            .available(&self.state.machine_id, now / 1_000_000_000);
        let links = link_status(&self.state.mesh, &self.state.machine_id);
        Ok(
            serde_json::json!({"version":1,"machine_id":self.state.machine_id,"links":links,"capabilities":{"protocol":"qos-cloud-mesh-actions/1","model_complete":available,"remote_actions":if available {vec!["model_complete"]}else{vec![]},"agent_execution":false,"private_workspace_access":false,"max_concurrent":8,"max_input_bytes":32768,"max_output_tokens":2048},"runtime_readiness":"requires_live_action_probe"}),
        )
    }
    pub(crate) fn remote_descriptors(&self) -> Result<Vec<serde_json::Value>> {
        self.ensure_reconciled()?;
        Ok(self.state.mesh.links.iter().filter(|(_,l)|l.active).map(|(id,l)| {
            let remote=if l.grant.source.machine_id==self.state.machine_id {&l.grant.target}else{&l.grant.source};
            serde_json::json!({"kind":"remote","link_id":id,"target_machine_id":remote.machine_id,"parameters":{"operation":"native_action"},"allowed_actions":l.grant.allowed_actions,"readiness":"probe_remote_catalogue","private_workspace_access":false})
        }).collect())
    }
    fn mesh_peer(&self, id: &str, machine: &str, now: u64) -> Result<MeshChallenge> {
        self.ensure_reconciled()?;
        let link = self
            .state
            .mesh
            .links
            .get(id)
            .filter(|l| l.active)
            .ok_or_else(|| anyhow!("mesh link revoked or missing"))?;
        let (local, remote) = if link.grant.source.machine_id == self.state.machine_id {
            (&link.grant.source, &link.grant.target)
        } else {
            (&link.grant.target, &link.grant.source)
        };
        let b = self
            .state
            .bound
            .as_ref()
            .ok_or_else(|| anyhow!("not enrolled"))?;
        if machine != remote.machine_id
            || local.root_public_key != b.root
            || local.host_certificate != b.authority
            || link.grant.allowed_actions != ["model_complete"]
        {
            bail!("mesh scope mismatch")
        }
        qos_user_identity::verify_device_cert_at(&remote.host_certificate, now)?;
        qos_user_identity::verify_device_cert_at(&local.host_certificate, now)?;
        if revocations(&b.revocations)?
            .is_revoked_at(&local.host_certificate.device_public_key, now)
        {
            bail!("local host revoked")
        }
        Ok(remote.clone())
    }
}
fn same_members(a: &MeshRequest, b: &MeshRequest) -> bool {
    [(&a.source, &b.source), (&a.target, &b.target)]
        .iter()
        .all(|(a, b)| {
            a.machine_id == b.machine_id
                && a.root_public_key == b.root_public_key
                && a.host_certificate == b.host_certificate
                && a.room == b.room
        })
}

/// One action attributed to an authenticated caller on a specifically admitted host.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MeshAction {
    pub version: u32,
    pub link_id: String,
    pub source_machine_id: String,
    pub target_machine_id: String,
    pub source_device_public_key: [u8; 32],
    pub request: ActionRequest,
}
impl Store {
    fn mesh_authorize(&self, cert: &DeviceCert, call: &MeshAction, now: u64) -> Result<[u8; 32]> {
        self.ensure_reconciled()?;
        if call.version != 1
            || call.target_machine_id != self.state.machine_id
            || call.request.version != 1
            || call.request.room != format!("mesh:{}", call.link_id)
            || call.request.invocation_id.is_empty()
            || call.request.invocation_id.len() > 128
        {
            bail!("invalid mesh action identity")
        }
        let peer = self.mesh_peer(&call.link_id, &call.source_machine_id, now)?;
        if &peer.host_certificate != cert {
            bail!("mesh host certificate mismatch")
        }
        if !matches!(
            call.request.operation,
            NativeOperation::ModelComplete { .. }
                | NativeOperation::Catalogue
                | NativeOperation::Status { .. }
                | NativeOperation::Cancel { .. }
        ) {
            bail!("action outside mesh grant")
        }
        Ok(blake3::derive_key(
            "qos mesh action receipt source v1",
            &serde_json::to_vec(&(
                &call.link_id,
                &call.source_machine_id,
                call.source_device_public_key,
            ))?,
        ))
    }
    fn mesh_admit(
        &mut self,
        cert: &DeviceCert,
        call: &MeshAction,
        now: u64,
    ) -> Result<(ActionReceipt, Option<model::PendingModel>)> {
        self.ensure_reconciled()?;
        let source = self.mesh_authorize(cert, call, now)?;
        let r = &call.request;
        if let NativeOperation::Status { invocation_id } = &r.operation {
            if invocation_id.is_empty() || invocation_id.len() > 128 {
                bail!("invalid status target")
            }
            let scope = format!("{}:{}", hex::encode(source), invocation_id);
            return Ok((
                ActionReceipt {
                    invocation_id: r.invocation_id.clone(),
                    status: "completed".into(),
                    error: None,
                    output: Some(serde_json::to_value(
                        self.state
                            .receipts
                            .get(&scope)
                            .map(|(_, r)| r.clone())
                            .or_else(|| {
                                self.state
                                    .pending_models
                                    .get(&scope)
                                    .map(|_| model::pending_receipt(invocation_id))
                            }),
                    )?),
                },
                None,
            ));
        }
        let scope = format!("{}:{}", hex::encode(source), r.invocation_id);
        let digest = blake3::hash(&serde_json::to_vec(r)?).to_hex().to_string();
        if let Some((old, receipt)) = self.state.receipts.get(&scope) {
            if old == &digest || old == "cancelled" {
                return Ok((receipt.clone(), None));
            }
            bail!("invocation substitution")
        }
        if self.state.pending_models.contains_key(&scope)
            && !matches!(r.operation, NativeOperation::ModelComplete { .. })
        {
            bail!("invocation substitution")
        }
        if self.state.receipts.len() >= 8192 {
            bail!("action receipt journal full")
        }
        if let NativeOperation::Cancel { invocation_id } = &r.operation {
            if invocation_id.is_empty() || invocation_id.len() > 128 {
                bail!("invalid cancellation target")
            }
            let target = format!("{}:{}", hex::encode(source), invocation_id);
            let mut state = self.state.clone();
            let mut receipt = ActionReceipt {
                invocation_id: r.invocation_id.clone(),
                status: "completed".into(),
                error: None,
                output: None,
            };
            if state.receipts.contains_key(&target) || state.pending_models.contains_key(&target) {
                receipt.status = "failed".into();
                receipt.error = Some("invocation already dispatched or terminal".into());
            } else {
                state.receipts.insert(
                    target,
                    (
                        "cancelled".into(),
                        ActionReceipt {
                            invocation_id: invocation_id.clone(),
                            status: "cancelled".into(),
                            error: None,
                            output: None,
                        },
                    ),
                );
            }
            state.receipts.insert(scope, (digest, receipt.clone()));
            self.commit(state)?;
            return Ok((receipt, None));
        }
        let NativeOperation::ModelComplete { text } = &r.operation else {
            bail!("catalogue handled by live worker")
        };
        model::validate_text(text)?;
        if self.state.pending_models.len() >= 32 && !self.state.pending_models.contains_key(&scope)
        {
            bail!("mesh worker saturated")
        }
        let receipt = self.begin_model(source, r, digest, Some(&call.link_id))?;
        Ok((receipt, self.state.pending_models.get(&scope).cloned()))
    }
}
/// Reuses native action admission IDs and receipts; the external phase never holds the state lock.
pub(crate) async fn invoke<F, Fut>(
    root: &Path,
    machine: &str,
    cert: &DeviceCert,
    call: &MeshAction,
    now: u64,
    dispatch: F,
) -> Result<ActionReceipt>
where
    F: FnOnce(model::PendingModel, String) -> Fut,
    Fut: std::future::Future<Output = gateway::Outcome>,
{
    let (receipt, pending) = Store::open(root, machine)?.mesh_admit(cert, call, now)?;
    let Some(pending) = pending else {
        return Ok(receipt);
    };
    let NativeOperation::ModelComplete { text } = &call.request.operation else {
        bail!("unsupported mesh action")
    };
    Store::open(root, machine)?.mesh_authorize(cert, call, qos_relay::clock::now_ns())?;
    let outcome = dispatch(pending.clone(), text.clone()).await;
    let mut store = Store::open(root, machine)?;
    let source = store.mesh_authorize(cert, call, qos_relay::clock::now_ns())?;
    store.finish_model_authorized(source,&call.request,&pending,outcome,Some(serde_json::json!({"execution_machine_id":machine,"source_machine_id":call.source_machine_id,"link_id":call.link_id})))
}

use iroh::protocol::{AcceptError, ProtocolHandler};
use qos_relay::{
    auth::{self, Hello, HelloAck, HelloAck2},
    framing::{read_frame, write_frame},
};
#[derive(Debug)]
pub(crate) struct Handler {
    pub root: PathBuf,
    pub machine: String,
    pub slots: tokio::sync::Semaphore,
    pub gateway: gateway::Gateway,
}
impl ProtocolHandler for Handler {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let _slot = self
            .slots
            .try_acquire()
            .map_err(|e| AcceptError::from_err(std::io::Error::other(e.to_string())))?;
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.exchange(&connection),
        )
        .await
        .map_err(|e| AcceptError::from_err(std::io::Error::other(e.to_string())))?
        .map_err(|e| AcceptError::from_err(std::io::Error::other(e.to_string())))
    }
}
impl Handler {
    async fn exchange(&self, connection: &iroh::endpoint::Connection) -> Result<()> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let hello: Hello = postcard::from_bytes(&read_frame(&mut recv, 8192).await?)?;
        let (cert, key) = {
            let store = Store::open(&self.root, &self.machine)?;
            // Pin exact certificate AND endpoint from an active bilateral grant. No blanket root trust.
            let admitted = store.state.mesh.links.iter().any(|(id, l)| {
                let machine = if l.grant.source.machine_id == self.machine {
                    &l.grant.target.machine_id
                } else {
                    &l.grant.source.machine_id
                };
                store
                    .mesh_peer(id, machine, qos_relay::clock::now_ns())
                    .is_ok_and(|p| {
                        p.host_certificate == hello.cert
                            && serde_json::from_str::<iroh_base::EndpointAddr>(&p.action_address)
                                .is_ok_and(|a| a.id == connection.remote_id())
                    })
            });
            if !admitted {
                bail!("mesh host not admitted")
            }
            (
                store
                    .state
                    .bound
                    .as_ref()
                    .ok_or_else(|| anyhow!("not enrolled"))?
                    .authority
                    .clone(),
                store.key(),
            )
        };
        let mut nonce = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        write_frame(
            &mut send,
            &postcard::to_stdvec(&auth::build_hello_ack(
                auth::AuthDomain::LegacyQos,
                cert,
                &key,
                nonce,
                &hello.client_nonce,
            ))?,
        )
        .await?;
        let ack2: HelloAck2 = postcard::from_bytes(&read_frame(&mut recv, 8192).await?)?;
        auth::verify_hello_ack2(
            auth::AuthDomain::LegacyQos,
            &ack2,
            &hello.cert.device_public_key,
            &nonce,
        )?;
        let call: MeshAction = serde_json::from_slice(&read_frame(&mut recv, 64 * 1024).await?)?;
        let result = match self.respond(&hello.cert, &call).await {
            Ok(receipt) => receipt,
            Err(error) if is_store_failure(&error) => return Err(error),
            Err(error) => ActionReceipt {
                invocation_id: call.request.invocation_id.clone(),
                status: "rejected".into(),
                error: Some(error.to_string()),
                output: None,
            },
        };
        write_frame(&mut send, &serde_json::to_vec(&result)?).await?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }
    async fn respond(&self, cert: &DeviceCert, call: &MeshAction) -> Result<ActionReceipt> {
        let now = qos_relay::clock::now_ns();
        {
            Store::open(&self.root, &self.machine)?.mesh_authorize(cert, call, now)?;
        }
        if matches!(call.request.operation, NativeOperation::Catalogue) {
            let operations = if self.gateway.available(&self.machine, now / 1_000_000_000) {
                vec![
                    serde_json::json!({"kind":"model_complete","max_input_bytes":32768,"max_output_tokens":2048}),
                ]
            } else {
                vec![]
            };
            return Ok(ActionReceipt {
                invocation_id: call.request.invocation_id.clone(),
                status: "completed".into(),
                error: None,
                output: Some(
                    serde_json::json!({"operations":operations,"execution_machine_id":self.machine,"link_id":call.link_id,"agent_execution":false}),
                ),
            });
        }
        if matches!(
            call.request.operation,
            NativeOperation::ModelComplete { .. }
        ) && !self.gateway.available(&self.machine, now / 1_000_000_000)
        {
            bail!("model action unavailable on target")
        }
        invoke(
            &self.root,
            &self.machine,
            cert,
            call,
            now,
            |pending, text| async move {
                self.gateway
                    .complete(&self.machine, &pending.gateway_id, &text)
                    .await
            },
        )
        .await
    }
}
/// Forward from the existing authenticated browser Action service to the exact linked host.
pub(crate) async fn forward(
    root: &Path,
    machine: &str,
    endpoint: &iroh::Endpoint,
    source: [u8; 32],
    request: &ActionRequest,
) -> Result<ActionReceipt> {
    let NativeOperation::Remote {
        link_id,
        target_machine_id,
        operation,
    } = &request.operation
    else {
        bail!("not remote")
    };
    let (peer, key, cert, call) = {
        let store = Store::open(root, machine)?;
        if request.version != 1
            || request.room != store.state.room
            || request.invocation_id.is_empty()
            || request.invocation_id.len() > 128
        {
            bail!("invalid action identity")
        }
        store.authorize_model(source, qos_relay::clock::now_ns())?;
        if !matches!(
            operation.as_ref(),
            NativeOperation::ModelComplete { .. }
                | NativeOperation::Catalogue
                | NativeOperation::Status { .. }
                | NativeOperation::Cancel { .. }
        ) {
            bail!("action outside mesh grant")
        }
        if let NativeOperation::ModelComplete { text } = operation.as_ref() {
            model::validate_text(text)?;
        }
        let peer = store.mesh_peer(link_id, target_machine_id, qos_relay::clock::now_ns())?;
        let call = MeshAction {
            version: 1,
            link_id: link_id.clone(),
            source_machine_id: machine.into(),
            target_machine_id: target_machine_id.clone(),
            source_device_public_key: source,
            request: ActionRequest {
                version: 1,
                room: format!("mesh:{link_id}"),
                invocation_id: request.invocation_id.clone(),
                operation: *operation.clone(),
            },
        };
        (
            peer,
            store.key(),
            store
                .state
                .bound
                .as_ref()
                .ok_or_else(|| anyhow!("not enrolled"))?
                .authority
                .clone(),
            call,
        )
    };
    let connection = endpoint
        .connect(
            serde_json::from_str::<iroh_base::EndpointAddr>(&peer.action_address)?,
            MESH_ALPN,
        )
        .await?;
    let result = exchange(&connection, &key, cert, &peer, &call).await;
    connection.close(0u32.into(), b"done");
    // Revocation during an in-flight call prevents delivery of its result.
    {
        let store = Store::open(root, machine)?;
        store.authorize_model(source, qos_relay::clock::now_ns())?;
        store.mesh_peer(link_id, target_machine_id, qos_relay::clock::now_ns())?;
    }
    result
}
async fn exchange(
    connection: &iroh::endpoint::Connection,
    key: &SigningKey,
    cert: DeviceCert,
    peer: &MeshChallenge,
    call: &MeshAction,
) -> Result<ActionReceipt> {
    let (mut send, mut recv) = connection.open_bi().await?;
    let mut nonce = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    write_frame(
        &mut send,
        &postcard::to_stdvec(&Hello {
            cert,
            client_nonce: nonce,
        })?,
    )
    .await?;
    let ack: HelloAck = postcard::from_bytes(&read_frame(&mut recv, 8192).await?)?;
    if ack.cert != peer.host_certificate {
        bail!("target host certificate substituted")
    }
    auth::verify_pinned_host_ack(
        auth::AuthDomain::LegacyQos,
        &ack,
        &RevocationList::new(),
        qos_relay::clock::now_ns(),
        &nonce,
    )?;
    write_frame(
        &mut send,
        &postcard::to_stdvec(&auth::build_hello_ack2(
            auth::AuthDomain::LegacyQos,
            key,
            &ack.server_nonce,
        ))?,
    )
    .await?;
    write_frame(&mut send, &serde_json::to_vec(call)?).await?;
    let receipt: ActionReceipt = serde_json::from_slice(&read_frame(&mut recv, 128 * 1024).await?)?;
    if receipt.invocation_id != call.request.invocation_id
        || !matches!(
            receipt.status.as_str(),
            "completed" | "failed" | "rejected" | "pending" | "cancelled"
        )
    {
        bail!("invalid remote receipt")
    }
    send.finish()?;
    Ok(receipt)
}

fn link_status(state: &MeshState, machine: &str) -> Vec<serde_json::Value> {
    state.links.iter().map(|(id,l)| {
        let remote=if l.grant.source.machine_id==machine {&l.grant.target}else{&l.grant.source};
        let mut value=serde_json::json!({"link_id":id,"peer_machine_id":remote.machine_id,"active":l.active,"allowed_actions":l.grant.allowed_actions,"peer_readiness":"unknown"});
        value["grant_json"]=serde_json::to_string(&l.grant).expect("mesh grant serializes").into();
        value
    }).collect()
}
