//! Restore an explicitly selected enrolled namespace through the native sync plane.
use super::{
    Context, IdentityPaths, Path, PathBuf, Result, WorkspaceAuthority, WorkspaceAuthorityEnvelope,
    capabilities, existing_person, new_cache, require_files_access,
};
use std::sync::{Arc, RwLock};

#[derive(clap::Args, Debug)]
pub(crate) struct JoinOptions {
    #[arg(long)]
    pub cache_dir: PathBuf,
    #[arg(long)]
    pub identity_root: PathBuf,
    #[arg(long)]
    pub acl: PathBuf,
    #[arg(long)]
    pub device: String,
    #[arg(long)]
    pub service: String,
    #[arg(long)]
    pub workspace_id: u64,
    #[arg(long)]
    pub files_workspace_id: uuid::Uuid,
    #[arg(long)]
    pub registry: Option<PathBuf>,
    /// Explicit consent to restore this exact already-authorized Files binding.
    #[arg(long)]
    pub confirm_existing_files_access: bool,
}

struct JoinedNamespace {
    registration: tilo_relay::transport::ContentPeerRegistration,
    authority: WorkspaceAuthorityEnvelope,
    namespace: Vec<u8>,
}

fn validate_binding(
    expected: uuid::Uuid,
    registration: &tilo_relay::transport::ContentPeerRegistration,
) -> Result<()> {
    anyhow::ensure!(
        registration.ticket.len() <= 64 * 1024,
        "Files binding exceeds its size limit"
    );
    let ticket = fs::Ticket::decode(&registration.ticket)?;
    anyhow::ensure!(
        ticket.workspace_id() == expected,
        "discovery returned another Files workspace"
    );
    // The enrolled replica endpoint may differ from the ticket's originating
    // owner. This is the existing native peer-catalog contract, not a mismatch.
    Ok(())
}

fn validate_authority(
    person: [u8; 32],
    workspace: uuid::Uuid,
    authority: &WorkspaceAuthorityEnvelope,
) -> Result<()> {
    authority.verify(workspace, person)?;
    anyhow::ensure!(
        !authority.tombstoned,
        "Files workspace authority is revoked"
    );
    require_files_access(person, &authority.capabilities)
}

async fn fetch_joined_namespace(
    endpoint: &iroh::Endpoint,
    identity: &fs::sync::FsTransportIdentity,
    workspace: uuid::Uuid,
    registration: tilo_relay::transport::ContentPeerRegistration,
) -> Result<JoinedNamespace> {
    validate_binding(workspace, &registration)?;
    let person = identity.user_public_key();
    let authority = fs::sync::submit_workspace_authority(
        endpoint,
        registration.endpoint.clone(),
        identity,
        workspace,
        None,
    )
    .await?
    .context("peer has no signed Files authority; establish its existing policy before recovery")?;
    validate_authority(person, workspace, &authority)?;
    let policy = Some(fs::AclHandle::new(fs::WorkspaceAcl::from_capabilities(
        person,
        &authority.capabilities,
        &[],
        None,
    )));
    let namespace = fs::sync::fetch_remote_doc(
        endpoint,
        registration.endpoint.clone(),
        identity,
        workspace,
        &policy,
    )
    .await?;
    let current = fs::sync::submit_workspace_authority(
        endpoint,
        registration.endpoint.clone(),
        identity,
        workspace,
        None,
    )
    .await?;
    anyhow::ensure!(
        current.as_ref() == Some(&authority),
        "Files authority changed during recovery; retry without replacing existing state"
    );
    Ok(JoinedNamespace {
        registration,
        authority,
        namespace,
    })
}

async fn fetch_existing(
    paths: &IdentityPaths,
    workspace: uuid::Uuid,
    registration: tilo_relay::transport::ContentPeerRegistration,
) -> Result<JoinedNamespace> {
    validate_binding(workspace, &registration)?;
    let certified =
        fs::identity::load_existing_certified_mount_identity(paths, tilo_clock::unix_now_ns())?;
    let revocations = fs::load_revocation_list(paths)?;
    let (device, credentials) = certified.into_parts();
    let identity =
        fs::sync::FsTransportIdentity::new(credentials, Arc::new(RwLock::new(revocations)));
    let (endpoint, _) = fs::sync::bind_endpoint_with_key(device.secret).await?;
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        fetch_joined_namespace(&endpoint, &identity, workspace, registration),
    )
    .await;
    endpoint.close().await;
    outcome.map_err(|_| {
        anyhow::anyhow!("Files namespace recovery timed out; existing data remains unknown")
    })?
}

fn prepare_join(
    cache: &Path,
    expected: uuid::Uuid,
    person: [u8; 32],
    joined: JoinedNamespace,
) -> Result<()> {
    validate_binding(expected, &joined.registration)?;
    validate_authority(person, expected, &joined.authority)?;
    // Load the peer's actual Automerge history; never initialize a replacement.
    let mut namespace = fs::Namespace::from_bytes("recovered-device", &joined.namespace)?;
    let bytes = namespace.save();
    let peers = [fs::workspace_authority::PersistedContentPeer {
        endpoint: joined.registration.endpoint,
        ticket: Some(joined.registration.ticket.clone()),
        relay_service_id: Some(joined.registration.relay_service_id),
    }];
    let peers = serde_json::to_vec(&peers)?;
    new_cache(cache)?;
    // The general authority writer inherits its caller's creation mode. This
    // new private recovery cache publishes its verified envelope explicitly.
    let authority_path = cache.join("workspace-authority.bin");
    tilo_files::native::atomic::write_with_mode(
        &authority_path,
        &postcard::to_allocvec(&joined.authority)?,
        0o600,
    )?;
    let authority = WorkspaceAuthority::open(authority_path, expected, person, person, Vec::new())?;
    authority.apply(joined.authority)?;
    tilo_files::native::atomic::write_with_mode(&cache.join("content-peers.json"), &peers, 0o600)?;
    tilo_files::native::atomic::write_with_mode(
        &cache.join("workspace-ticket"),
        joined.registration.ticket.as_bytes(),
        0o600,
    )?;
    // Readiness becomes visible only after signed authority and binding persist.
    tilo_files::native::atomic::write_with_mode(&cache.join("namespace.bin"), &bytes, 0o600)?;
    Ok(())
}

pub(crate) fn join(options: &JoinOptions) -> Result<()> {
    anyhow::ensure!(
        options.confirm_existing_files_access,
        "joining requires an explicit exact Files access choice"
    );
    anyhow::ensure!(
        options.cache_dir.is_absolute() && !options.cache_dir.try_exists()?,
        "Files destination must be an absent absolute directory; preserve existing state"
    );
    let paths = IdentityPaths::under(&options.identity_root);
    let person = existing_person(&paths)?;
    require_files_access(person, &capabilities(&options.acl)?)?;
    let registration = super::super::files_probe::discover(
        &options.identity_root,
        &options.device,
        &options.service,
        options.workspace_id,
        options.registry.as_deref(),
    )?;
    let joined = tilo_rt::block_on(fetch_existing(
        &paths,
        options.files_workspace_id,
        registration,
    ))??;
    prepare_join(
        &options.cache_dir,
        options.files_workspace_id,
        person,
        joined,
    )?;
    println!(
        "Restored existing Files namespace {}. This does not establish recovery of historical populated data; no file payloads were fetched.",
        options.files_workspace_id
    );
    Ok(())
}

#[cfg(test)]
mod tests;
