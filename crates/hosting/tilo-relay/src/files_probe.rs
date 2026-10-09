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

pub(super) fn discover(
    root: &Path,
    device: &str,
    service: &str,
    workspace: u64,
    registry: Option<&Path>,
) -> Result<tilo_relay::transport::ContentPeerRegistration> {
    let paths = IdentityPaths::under(root);
    // load_device_key is deliberately read-only: unlike the mount's first-boot
    // loader, it cannot create a missing key or substitute another identity.
    let key =
        persist::load_device_key(&paths).context("load existing device key for Files probe")?;
    let user = persist::read_public_key(&paths.user_pubkey)
        .context("read existing person public key for Files probe")?;
    let cert = persist::read_device_cert(&paths.device_cert)
        .context("read existing device certificate for Files probe")?;
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
    let revocations = fs::load_revocation_list(&paths)
        .context("load signed device revocations for Files probe")?;
    let expected_device = super::device_key(device)?;
    anyhow::ensure!(
        !revocations.is_revoked_at(&cert.device_public_key, now)
            && !revocations.is_revoked_at(&expected_device, now),
        "local device or selected home peer is revoked"
    );
    let service_id = super::device_key(service)?;
    let default_registry = tilo_relay::home_peer_registry_path(root);
    let registry = registry.unwrap_or(&default_registry);
    let file = std::fs::File::open(registry)
        .context("read enrolled home peer registry for Files probe")?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "peer registry must be a regular file"
    );
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, 1024 * 1024 + 1), &mut bytes)?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "peer registry exceeds 1 MiB");
    let registrations: Vec<tilo_relay::HomePeerRegistration> = serde_json::from_slice(&bytes)?;
    let selected = registrations
        .into_iter()
        .find(|r| {
            r.device_node_id == device
                && r.relay_service_node_id == service
                && r.user_public_key == hex::encode(user)
                && r.relay_workspace_ids.contains(&workspace)
        })
        .context("selected room is not registered to this person's enrolled home peer")?;
    let service_key = iroh_base::PublicKey::from_bytes(&service_id)?;
    let address = selected
        .relay_endpoint
        .filter(|endpoint| endpoint.id == service_key && !endpoint.is_empty())
        .unwrap_or_else(|| iroh_base::EndpointAddr::new(service_key));
    let registration = tilo_rt::block_on(async move {
        let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
            .await
            .context("bind ephemeral Files probe endpoint")?;
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
    public_status(&registration, workspace, service_id)?;
    Ok(registration)
}

pub(super) fn run(
    root: &Path,
    device: &str,
    service: &str,
    workspace: u64,
    registry: Option<&Path>,
) -> Result<()> {
    let registration = discover(root, device, service, workspace, registry)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&public_status(
            &registration,
            workspace,
            super::device_key(service)?
        )?)?
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
        assert!(run(&root, &hex::encode([1; 32]), &hex::encode([2; 32]), 3, None).is_err());
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
