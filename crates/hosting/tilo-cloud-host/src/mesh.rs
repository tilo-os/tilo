//! Explicit bilateral grants for bounded Actions; no peer, root or room mutation.
use super::*;
use tilo_user_identity::machine_mesh::{MeshChallenge, MeshRequest, verify_request};
pub const MESH_ALPN: &[u8] = b"tilo-cloud-mesh-actions/1";
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct MeshState {
    pub(super) challenge: Option<MeshChallenge>,
    links: BTreeMap<String, Link>,
    consumed: BTreeMap<String, (String, serde_json::Value)>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Link {
    grant: MeshRequest,
    active: bool,
}
impl Store {
    pub fn mesh_inspect(&mut self, now: u64) -> Result<MeshChallenge> {
        self.ensure_reconciled()?;
        if let Some(c) = &self.state.mesh.challenge
            && now < c.expires_at_ns
        {
            return Ok(c.clone());
        }
        let b = self
            .state
            .bound
            .as_ref()
            .ok_or_else(|| anyhow!("workspace not enrolled"))?;
        let relay = std::env::var("TILO_CLOUD_PACKET_RELAY").or_else(|e| {
            if cfg!(test) {
                Ok("https://example.invalid".into())
            } else {
                Err(e)
            }
        })?;
        let address =
            iroh_base::EndpointAddr::new(self.action_key().public()).with_relay_url(relay.parse()?);
        let mut nonce = [0; 32];
        rand::rand_core::UnwrapErr(rand::rngs::SysRng).fill_bytes(&mut nonce);
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
            if let Some(old) = state.mesh.links.get(&r.link_id)
                && (!old.active || !same_members(&old.grant, r))
            {
                bail!("mesh link id already used")
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
        let _ = now;
        let links = link_status(&self.state.mesh, &self.state.machine_id);
        // Links and grants are kept; no operation is delegated over a link yet.
        Ok(
            serde_json::json!({"version":1,"machine_id":self.state.machine_id,"links":links,"capabilities":{"protocol":"tilo-cloud-mesh-actions/1","remote_actions":Vec::<String>::new(),"agent_execution":false,"private_workspace_access":false,"max_concurrent":8},"runtime_readiness":"requires_live_action_probe"}),
        )
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
        tilo_user_identity::verify_device_cert_at(&remote.host_certificate, now)?;
        tilo_user_identity::verify_device_cert_at(&local.host_certificate, now)?;
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
    fn mesh_authorize(&self, cert: &DeviceCert, call: &MeshAction, now: u64) -> Result<()> {
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
        bail!("no operation is delegated over machine links")
    }
}
use iroh::protocol::{AcceptError, ProtocolHandler};
use tilo_relay::{
    auth::{self, Hello, HelloAck2},
    framing::{read_frame, write_frame},
};
#[derive(Debug)]
pub(crate) struct Handler {
    pub root: PathBuf,
    pub machine: String,
    pub slots: tokio::sync::Semaphore,
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
                    .mesh_peer(id, machine, tilo_delivery_protocol::clock::now_ns())
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
        rand::rand_core::UnwrapErr(rand::rngs::SysRng).fill_bytes(&mut nonce);
        write_frame(
            &mut send,
            &postcard::to_stdvec(&auth::build_hello_ack(
                cert,
                &key,
                nonce,
                &hello.client_nonce,
            ))?,
        )
        .await?;
        let ack2: HelloAck2 = postcard::from_bytes(&read_frame(&mut recv, 8192).await?)?;
        auth::verify_hello_ack2(&ack2, &hello.cert.device_public_key, &nonce)?;
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
        let now = tilo_delivery_protocol::clock::now_ns();
        Store::open(&self.root, &self.machine)?.mesh_authorize(cert, call, now)?;
        bail!("no operation is delegated over machine links")
    }
}
/// Serve the mesh endpoint under the machine's mesh identity.
pub(crate) async fn start(
    root: &Path,
    machine: &str,
    mode: iroh::RelayMode,
) -> Result<iroh::protocol::Router> {
    let key = { Store::open(root, machine)?.action_key() };
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .relay_mode(mode)
        .bind()
        .await?;
    let handler = std::sync::Arc::new(Handler {
        root: root.into(),
        machine: machine.into(),
        slots: tokio::sync::Semaphore::new(8),
    });
    Ok(iroh::protocol::Router::builder(endpoint)
        .accept(MESH_ALPN, handler)
        .spawn())
}

fn link_status(state: &MeshState, machine: &str) -> Vec<serde_json::Value> {
    state.links.iter().map(|(id,l)| {
        let remote=if l.grant.source.machine_id==machine {&l.grant.target}else{&l.grant.source};
        let mut value=serde_json::json!({"link_id":id,"peer_machine_id":remote.machine_id,"active":l.active,"allowed_actions":l.grant.allowed_actions,"peer_readiness":"unknown"});
        value["grant_json"]=serde_json::to_string(&l.grant).expect("mesh grant serializes").into();
        value
    }).collect()
}
