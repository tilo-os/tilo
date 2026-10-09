//! A read-only recovery lead: use the existing enrollment and discovery door,
//! but never publish a ticket or refresh the user's registry as a side effect.
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use tilo_user_identity::{IdentityPaths, persist};

#[derive(Debug, Serialize)]
struct FilesStatus {
    schema_version: u8,
    relay_workspace_id: u64,
    files_workspace_id: uuid::Uuid,
    files_endpoint_id: String,
    relay_service_id: String,
}

fn public_status(
    registration: &tilo_relay::transport::ContentPeerRegistration,
    relay_workspace_id: u64,
    service: [u8; 32],
) -> Result<FilesStatus> {
    anyhow::ensure!(
        registration.relay_service_id == service,
        "discovery returned another relay service"
    );
    let ticket =
        fs::Ticket::decode(&registration.ticket).context("invalid Files workspace binding")?;
    Ok(FilesStatus {
        schema_version: 1,
        relay_workspace_id,
        files_workspace_id: ticket.workspace_id(),
        files_endpoint_id: registration.endpoint.id.to_string(),
        relay_service_id: hex::encode(service),
    })
}

pub(super) fn run(root: &Path, device: &str, service: &str, workspace: u64) -> Result<()> {
    let paths = IdentityPaths::under(root);
    // load_device_key is deliberately read-only: unlike the mount's first-boot
    // loader, it cannot create a missing key or substitute another identity.
    let key = persist::load_device_key(&paths)?;
    let user = persist::read_public_key(&paths.user_pubkey)?;
    let cert = persist::read_device_cert(&paths.device_cert)?;
    let now = tilo_clock::unix_now_ns();
    tilo_user_identity::verify_device_cert_at(&cert, now)?;
    anyhow::ensure!(
        cert.user_public_key == user,
        "certificate belongs to another person"
    );
    anyhow::ensure!(
        cert.device_public_key == key.verifying_key().to_bytes(),
        "certificate belongs to another device key"
    );
    let revocations = fs::load_revocation_list(&paths)?;
    let expected_device = super::device_key(device)?;
    anyhow::ensure!(
        !revocations.is_revoked_at(&cert.device_public_key, now)
            && !revocations.is_revoked_at(&expected_device, now),
        "local device or selected home peer is revoked"
    );
    let service_id = super::device_key(service)?;
    let registrations: Vec<tilo_relay::HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(tilo_relay::home_peer_registry_path(root))?)?;
    anyhow::ensure!(
        registrations.iter().any(|r| r.device_node_id == device
            && r.relay_service_node_id == service
            && r.user_public_key == hex::encode(user)
            && r.relay_workspace_ids.contains(&workspace)),
        "selected room is not registered to this person's enrolled home peer"
    );
    let address = super::registered_home_peer_addr(root, device, service_id)?;
    let registration = tilo_rt::block_on(async move {
        let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0).await?;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tilo_relay::RelayClient::discover_content_peer(
                &endpoint,
                address,
                workspace,
                user,
                expected_device,
                cert,
                key,
                revocations,
            ),
        )
        .await;
        endpoint.close().await;
        outcome
            .map_err(|_| {
                anyhow::anyhow!(
                    "home peer Files discovery timed out; existing data is unknown, not empty"
                )
            })?
            .map_err(anyhow::Error::from)
    })??;
    println!(
        "{}",
        serde_json::to_string_pretty(&public_status(&registration, workspace, service_id)?)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_credentials_do_not_create_an_identity_or_registry() {
        let dir = tempfile::tempdir().expect("isolated profile");
        let root = dir.path().join("identity");
        assert!(run(&root, &hex::encode([1; 32]), &hex::encode([2; 32]), 3).is_err());
        assert!(!root.exists());
    }

    #[test]
    fn output_contains_only_public_binding_metadata() {
        let endpoint = iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public());
        let ticket = fs::Ticket {
            node: endpoint.clone(),
            workspace: [8; 32],
        };
        let encoded = ticket.encode().expect("fixture ticket");
        let registration = tilo_relay::transport::ContentPeerRegistration {
            endpoint,
            ticket: encoded.clone(),
            relay_service_id: [9; 32],
        };
        let status = public_status(&registration, 10, [9; 32]).expect("matching service");
        assert_eq!(status.files_workspace_id, ticket.workspace_id());
        let json = serde_json::to_string(&status).expect("public JSON");
        assert!(!json.contains(&encoded));
        assert!(!json.contains("ticket"));
        assert!(public_status(&registration, 10, [11; 32]).is_err());
    }
}
