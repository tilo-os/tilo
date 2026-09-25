//! Addressed RPC using the native mutual device-certificate handshake.
//! Arguments and receipts stay on this stream and never enter workspace fan-out.
use super::*;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use qos_relay::{
    auth::{self, Hello, HelloAck2},
    framing::{read_frame, write_frame},
};
use std::{sync::Arc, time::Duration};
#[derive(Debug)]
struct Handler {
    endpoint: iroh::Endpoint,
    root: PathBuf,
    machine: String,
    slots: tokio::sync::Semaphore,
    gateway: gateway::Gateway,
    access_updates: Option<tokio::sync::mpsc::Sender<crate::collaboration::AccessUpdate>>,
}
impl ProtocolHandler for Handler {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let _slot = self
            .slots
            .try_acquire()
            .map_err(|e| AcceptError::from_err(std::io::Error::other(e.to_string())))?;
        tokio::time::timeout(Duration::from_secs(30), self.exchange(&connection))
            .await
            .map_err(|e| AcceptError::from_err(std::io::Error::other(e.to_string())))?
            .map_err(|e| AcceptError::from_err(std::io::Error::other(e.to_string())))
    }
}
impl Handler {
    async fn exchange(&self, connection: &iroh::endpoint::Connection) -> Result<()> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let hello: Hello = postcard::from_bytes(&read_frame(&mut recv, 8192).await?)?;
        let (peer, key, cert) = {
            let store = Store::open(&self.root, &self.machine)?;
            let b = store
                .state
                .bound
                .as_ref()
                .ok_or_else(|| anyhow!("not enrolled"))?;
            let state_peer = store.peer()?;
            let access = state_peer
                .governed_access(web_time::SystemTime::now())
                .map_err(anyhow::Error::msg)?;
            let policy = qos_relay::TrustPolicy::AllowList {
                users: access.member_user_roots.iter().map(hex::encode).collect(),
            };
            let peer = auth::accept_hello(
                &hello,
                &b.root,
                &policy,
                &revocations(&state_peer.revocations())?,
                qos_relay::clock::now_ns(),
            )?;
            if !access.member_devices.contains(&peer.device_public_key)
                || !state_peer
                    .certificates()
                    .iter()
                    .any(|cert| cert == &peer.cert)
            {
                bail!("device absent from current governed roster")
            }
            state_peer
                .permissions(peer.device_public_key, web_time::SystemTime::now())
                .map_err(anyhow::Error::msg)?;
            (peer, store.key(), b.authority.clone())
        };
        let mut nonce = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ack = auth::build_hello_ack(
            auth::AuthDomain::LegacyQos,
            cert,
            &key,
            nonce,
            &hello.client_nonce,
        );
        write_frame(&mut send, &postcard::to_stdvec(&ack)?).await?;
        let ack2: HelloAck2 = postcard::from_bytes(&read_frame(&mut recv, 8192).await?)?;
        auth::verify_hello_ack2(
            auth::AuthDomain::LegacyQos,
            &ack2,
            &peer.device_public_key,
            &nonce,
        )?;
        let request: ActionRequest =
            serde_json::from_slice(&read_frame(&mut recv, 64 * 1024).await?)?;
        let result = if matches!(request.operation, NativeOperation::Remote { .. }) {
            mesh::forward(
                &self.root,
                &self.machine,
                &self.endpoint,
                peer.device_public_key,
                &request,
            )
            .await
        } else {
            respond_with_access_updates(
                &self.root,
                &self.machine,
                &self.gateway,
                peer.device_public_key,
                &request,
                self.access_updates.as_ref(),
            )
            .await
        };
        let response = match result {
            Ok(receipt) => receipt,
            Err(error) if is_store_failure(&error) => return Err(error),
            Err(error) => ActionReceipt {
                invocation_id: request.invocation_id.clone(),
                status: "rejected".into(),
                error: Some(error.to_string()),
                output: None,
            },
        };
        write_frame(&mut send, &serde_json::to_vec(&response)?).await?;
        send.finish()?;
        // Wait for the peer to receive/close, bounded by the handler deadline.
        connection.closed().await;
        Ok(())
    }
}
pub async fn start(root: &Path, machine: &str, mode: iroh::RelayMode) -> Result<Router> {
    start_inner(root, machine, mode, None).await
}

pub(crate) async fn start_with_access_updates(
    root: &Path,
    machine: &str,
    mode: iroh::RelayMode,
    access_updates: tokio::sync::mpsc::Sender<crate::collaboration::AccessUpdate>,
) -> Result<Router> {
    start_inner(root, machine, mode, Some(access_updates)).await
}

async fn start_inner(
    root: &Path,
    machine: &str,
    mode: iroh::RelayMode,
    access_updates: Option<tokio::sync::mpsc::Sender<crate::collaboration::AccessUpdate>>,
) -> Result<Router> {
    let key = { Store::open(root, machine)?.action_key() };
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .relay_mode(mode)
        .bind()
        .await?;
    let mesh_handler = Arc::new(mesh::Handler {
        root: root.into(),
        machine: machine.into(),
        slots: tokio::sync::Semaphore::new(8),
        gateway: gateway::Gateway::from_environment(),
    });
    let handler = Arc::new(Handler {
        endpoint: endpoint.clone(),
        root: root.into(),
        machine: machine.into(),
        slots: tokio::sync::Semaphore::new(32),
        gateway: gateway::Gateway::from_environment(),
        access_updates,
    });
    Ok(Router::builder(endpoint)
        .accept(actions::ACTION_ALPN, handler)
        .accept(mesh::MESH_ALPN, mesh_handler)
        .spawn())
}

async fn respond_with_access_updates(
    root: &Path,
    machine: &str,
    gateway: &gateway::Gateway,
    source: [u8; 32],
    request: &ActionRequest,
    access_updates: Option<&tokio::sync::mpsc::Sender<crate::collaboration::AccessUpdate>>,
) -> Result<ActionReceipt> {
    let receipt = respond(root, machine, gateway, source, request).await?;
    await_access_reconciliation(receipt, &request.operation, access_updates).await
}

async fn await_access_reconciliation(
    receipt: ActionReceipt,
    operation: &NativeOperation,
    access_updates: Option<&tokio::sync::mpsc::Sender<crate::collaboration::AccessUpdate>>,
) -> Result<ActionReceipt> {
    if receipt.status == "completed"
        && matches!(operation, NativeOperation::CollaborationRemove { .. })
        && let Some(access_updates) = access_updates
    {
        let (completion, completed) = tokio::sync::oneshot::channel();
        access_updates
            .send(crate::collaboration::AccessUpdate { completion })
            .await
            .map_err(|_| anyhow!("workspace relay reconciliation is unavailable"))
            .context(StoreUnavailable)?;
        completed
            .await
            .map_err(|_| anyhow!("workspace relay reconciliation was interrupted"))
            .context(StoreUnavailable)?
            .map_err(anyhow::Error::msg)
            .context(StoreUnavailable)?;
    }
    Ok(receipt)
}

#[cfg(test)]
mod access_update_tests {
    use super::*;

    #[tokio::test]
    async fn removal_receipt_waits_for_authoritative_relay_reconciliation() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let receipt = ActionReceipt {
            invocation_id: "remove".into(),
            status: "completed".into(),
            error: None,
            output: None,
        };
        let operation = NativeOperation::CollaborationRemove {
            user_public_key: [9; 32],
        };
        let mut waiting = Box::pin(await_access_reconciliation(
            receipt,
            &operation,
            Some(&sender),
        ));
        let update = tokio::select! {
            result = &mut waiting => panic!("receipt completed before reconciliation: {result:?}"),
            update = receiver.recv() => update.unwrap(),
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err(),
            "receipt must stay pending while the live relay update is pending"
        );
        update.completion.send(Ok(())).unwrap();
        assert_eq!(waiting.await.unwrap().status, "completed");
    }
}

/// Authenticated request handling; only the certificate handshake supplies source.
pub(crate) async fn respond(
    root: &Path,
    machine: &str,
    gateway: &gateway::Gateway,
    source: [u8; 32],
    request: &ActionRequest,
) -> Result<ActionReceipt> {
    if matches!(request.operation, NativeOperation::ModelComplete { .. }) {
        model::invoke(root,machine,source,&request,qos_relay::clock::now_ns(),|pending,text| async move {
                gateway.complete(machine,&pending.gateway_id,&text).await
            }).await
    } else {
        let mut store = Store::open(root, machine)?;
        let mut result = store.action(source, &request, qos_relay::clock::now_ns());
        if matches!(request.operation, NativeOperation::Catalogue)
            && gateway.available(machine, qos_relay::clock::now_ns() / 1_000_000_000)
            && store
                .authorize_model(source, qos_relay::clock::now_ns())
                .is_ok()
        {
            if let Ok(receipt) = &mut result {
                if let Some(operations) = receipt
                    .output
                    .as_mut()
                    .and_then(|v| v.get_mut("operations"))
                    .and_then(|v| v.as_array_mut())
                {
                    operations.push(serde_json::json!({"kind":"model_complete","parameters":{"text":"string"},"max_input_bytes":32768,"max_output_tokens":2048}));
                }
            }
        }
        if matches!(request.operation, NativeOperation::Catalogue)
            && store
                .authorize_model(source, qos_relay::clock::now_ns())
                .is_ok()
        {
            if let Ok(receipt) = &mut result {
                if let Some(operations) = receipt
                    .output
                    .as_mut()
                    .and_then(|v| v.get_mut("operations"))
                    .and_then(|v| v.as_array_mut())
                {
                    operations.extend(store.remote_descriptors()?);
                }
            }
        }
        result
    }
}
