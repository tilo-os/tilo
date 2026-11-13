//! Signed, monotonic workspace-wide ACL state.
//!
//! Separate from a host-local capability file on purpose: the envelope is verified against the
//! workspace governor, persists its revision, and can only narrow or replace authority through a
//! newer signed revision.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use collab_types::{CapOverride, Principal};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use wire_frame::{FrameError, Framing};

use crate::sync::Ticket;
use crate::{AclHandle, IdentityKey, WorkspaceAcl};

pub const TILO_FS_AUTHORITY_ALPN: &[u8] = b"tilo-fs-workspace-authority/1";
const MAX_AUTHORITY_FRAME: usize = 256 * 1024;
const CONTENT_PEERS_FILE: &str = "content-peers.json";
const MAX_PERSISTED_CONTENT_PEERS: usize = 32;
/// Local, group-restricted control socket used by the desktop authority publisher, so it need not
/// self-dial an Iroh node whose UDP socket the filesystem daemon owns.
pub const LOCAL_AUTHORITY_SOCKET: &str = "workspace-authority.sock";

const LOCAL_AUTHORITY_SOCKET_PREFIX: &str = "workspace-authority-";
const LOCAL_SOCKET_SUFFIX: &str = ".sock";

/// `/run/tilo`, created root:tilo-identity by tmpfiles, unless `TILO_AUTHORITY_SOCKET_DIR` names
/// another.
fn local_socket_dir() -> PathBuf {
    std::env::var_os("TILO_AUTHORITY_SOCKET_DIR")
        .map_or_else(|| PathBuf::from("/run/tilo"), PathBuf::from)
}

/// The stable local control path for a workspace. `/run/tilo` is created root:tilo-identity by the
/// service unit, so no root-owned cache directory is exposed to the desktop publisher.
pub fn local_authority_socket_path(workspace: uuid::Uuid) -> PathBuf {
    local_socket_dir().join(format!(
        "{LOCAL_AUTHORITY_SOCKET_PREFIX}{workspace}{LOCAL_SOCKET_SUFFIX}"
    ))
}

/// The workspace authority the filesystem daemon serves on this machine, and its socket. The
/// workspace comes from the socket's name, so the desktop publisher learns it without reading the
/// daemon's root-only cache.
#[cfg(unix)]
pub async fn find_local_authority() -> anyhow::Result<(uuid::Uuid, PathBuf)> {
    find_local_authority_in(&local_socket_dir()).await
}

#[cfg(unix)]
async fn find_local_authority_in(dir: &Path) -> anyhow::Result<(uuid::Uuid, PathBuf)> {
    use anyhow::Context as _;

    let mut live = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("list {}", dir.display()))? {
        let entry = entry?;
        let Some(workspace) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix(LOCAL_AUTHORITY_SOCKET_PREFIX))
            .and_then(|name| name.strip_suffix(LOCAL_SOCKET_SUFFIX))
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
        else {
            continue;
        };
        // A daemon that served another workspace before a rebind leaves its socket behind; only one
        // that still answers is the authority.
        let path = entry.path();
        if tokio::net::UnixStream::connect(&path).await.is_ok() {
            live.push((workspace, path));
        }
    }
    match live.len() {
        1 => Ok(live.remove(0)),
        0 => anyhow::bail!(
            "no filesystem daemon serves a workspace authority in {} yet",
            dir.display()
        ),
        _ => anyhow::bail!(
            "{} workspace authorities answer in {}; expected one",
            live.len(),
            dir.display()
        ),
    }
}

/// Stable, workspace-scoped handoff for a direct endpoint returned by the authenticated relay
/// discovery ALPN.
pub fn local_content_socket_path(workspace: uuid::Uuid) -> PathBuf {
    local_socket_dir().join(format!(
        "workspace-content-{workspace}{LOCAL_SOCKET_SUFFIX}"
    ))
}

/// Durable direct Files endpoints discovered through the authenticated relay. Written only by the
/// root-owned daemon after its group-restricted control socket accepts the request.
pub fn content_peers_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(CONTENT_PEERS_FILE)
}

/// A discovered Files endpoint with the namespace and record plane it serves. `ticket` is absent
/// only for entries written by older daemons; such an entry still serves blob hydration but is not a
/// record-plane bootstrap source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedContentPeer {
    pub endpoint: iroh_base::EndpointAddr,
    #[serde(default)]
    pub ticket: Option<String>,
    /// Enrolled relay service which authenticated this endpoint; legacy entries have none.
    #[serde(default)]
    pub relay_service_id: Option<[u8; 32]>,
}

impl PersistedContentPeer {
    fn validate(&self) -> anyhow::Result<()> {
        if let Some(ticket) = &self.ticket {
            // The ticket names the shared namespace and record plane. Its node is commonly the
            // originating owner while `endpoint` is the authenticated always-on replica discovered
            // through relay; discovery authentication pins the endpoint to the enrolled host, so only
            // the bounded ticket encoding and its workspace identity are checked here.
            let _ = Ticket::decode(ticket)?;
        }
        Ok(())
    }

    pub fn decoded_ticket(&self) -> anyhow::Result<Option<Ticket>> {
        self.ticket.as_deref().map(Ticket::decode).transpose()
    }
}

/// Load the bounded, de-duplicated direct content peers for a cache.
pub fn load_persisted_content_peers(cache_dir: &Path) -> anyhow::Result<Vec<PersistedContentPeer>> {
    let path = content_peers_path(cache_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let peers: Vec<PersistedContentPeer> = serde_json::from_slice(&std::fs::read(&path)?)?;
    anyhow::ensure!(
        peers.len() <= MAX_PERSISTED_CONTENT_PEERS,
        "persisted content peers exceed {MAX_PERSISTED_CONTENT_PEERS}"
    );
    let mut unique = Vec::with_capacity(peers.len());
    for peer in peers {
        peer.validate()?;
        if !unique
            .iter()
            .any(|known: &PersistedContentPeer| known.endpoint.id == peer.endpoint.id)
        {
            unique.push(peer);
        }
    }
    Ok(unique)
}

fn persist_content_peer(cache_dir: &Path, peer: PersistedContentPeer) -> anyhow::Result<()> {
    peer.validate()?;
    let mut peers = load_persisted_content_peers(cache_dir)?;
    if let Some(index) = peers
        .iter()
        .position(|known| known.endpoint.id == peer.endpoint.id)
    {
        // A later authenticated discovery can upgrade a legacy address with the authoritative
        // workspace ticket, so every refresh moves to the back: vector order is a bounded,
        // least-recently-discovered queue.
        let previous = peers.remove(index);
        let mut refreshed = peer;
        if refreshed.ticket.is_none() {
            refreshed.ticket = previous.ticket;
        }
        if refreshed.relay_service_id.is_none() {
            refreshed.relay_service_id = previous.relay_service_id;
        }
        peers.push(refreshed);
    } else {
        if peers.len() == MAX_PERSISTED_CONTENT_PEERS {
            peers.remove(0);
        }
        peers.push(peer);
    }
    tilo_files::native::atomic::write(
        &content_peers_path(cache_dir),
        &serde_json::to_vec(&peers)?,
    )?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceAuthorityEnvelope {
    pub workspace: uuid::Uuid,
    pub revision: u64,
    pub capabilities: Vec<CapOverride>,
    /// Who may reach each workspace a relay carries, by its relay workspace id. Written under
    /// `room_members`, the key every signed and stored revision was written under; one written while
    /// the field was named for the workspace reads as the same map.
    #[serde(default, rename = "room_members", alias = "workspace_members")]
    pub workspace_members: std::collections::BTreeMap<String, Vec<collab_types::WorkspaceMember>>,
    pub tombstoned: bool,
    pub governor: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

impl WorkspaceAuthorityEnvelope {
    /// Sign one monotonic workspace authority revision.
    pub fn sign(
        workspace: uuid::Uuid,
        revision: u64,
        authority: impl Into<collab_types::FsAuthority>,
        tombstoned: bool,
        governor: &ed25519_dalek::SigningKey,
    ) -> anyhow::Result<Self> {
        Self::sign_with(
            workspace,
            revision,
            authority,
            tombstoned,
            governor.verifying_key().to_bytes(),
            |payload| Ok(governor.sign(payload).to_bytes()),
        )
    }

    /// [`Self::sign`] for a governor whose key this process cannot hold, such as a person's identity
    /// record: `sign` receives the exact payload and returns the governor's signature over it.
    pub fn sign_with(
        workspace: uuid::Uuid,
        revision: u64,
        authority: impl Into<collab_types::FsAuthority>,
        tombstoned: bool,
        governor: [u8; 32],
        sign: impl FnOnce(&[u8]) -> anyhow::Result<[u8; 64]>,
    ) -> anyhow::Result<Self> {
        let collab_types::FsAuthority {
            capabilities,
            workspace_members,
        } = authority.into();
        let mut envelope = Self {
            workspace,
            revision,
            capabilities,
            workspace_members,
            tombstoned,
            governor,
            signature: [0; 64],
        };
        envelope.signature = sign(&envelope.payload()?)?;
        envelope.verify(workspace, governor)?;
        Ok(envelope)
    }

    pub fn verify(&self, expected_workspace: uuid::Uuid, governor: [u8; 32]) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.workspace == expected_workspace,
            "authority workspace mismatch"
        );
        anyhow::ensure!(self.governor == governor, "authority governor mismatch");
        VerifyingKey::from_bytes(&governor)?
            .verify(&self.payload()?, &Signature::from_bytes(&self.signature))?;
        Ok(())
    }

    fn payload(&self) -> anyhow::Result<Vec<u8>> {
        Ok(postcard::to_allocvec(&(
            self.workspace,
            self.revision,
            &self.capabilities,
            &self.workspace_members,
            self.tombstoned,
            self.governor,
        ))?)
    }
}

/// Durable authority projection for one workspace. The ACL handle is shared with every filesystem
/// protocol, so applying a newer envelope changes the verdict for the next request without
/// restarting the home peer.
pub struct WorkspaceAuthority {
    path: PathBuf,
    workspace: uuid::Uuid,
    governor: [u8; 32],
    subject: IdentityKey,
    principals: Vec<Principal>,
    handle: AclHandle,
    current: Mutex<Option<WorkspaceAuthorityEnvelope>>,
}

impl WorkspaceAuthority {
    /// Open the durable authority projection for one workspace.
    pub fn open(
        path: impl Into<PathBuf>,
        workspace: uuid::Uuid,
        governor: [u8; 32],
        subject: IdentityKey,
        principals: Vec<Principal>,
    ) -> anyhow::Result<Self> {
        let path = path.into();
        let current = match std::fs::read(&path) {
            Ok(bytes) => {
                let envelope: WorkspaceAuthorityEnvelope = postcard::from_bytes(&bytes)?;
                envelope.verify(workspace, governor)?;
                Some(envelope)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let handle = AclHandle::new(project(subject, &principals, current.as_ref()));
        Ok(Self {
            path,
            workspace,
            governor,
            subject,
            principals,
            handle,
            current: Mutex::new(current),
        })
    }

    pub fn handle(&self) -> AclHandle {
        self.handle.clone()
    }

    pub const fn workspace(&self) -> uuid::Uuid {
        self.workspace
    }

    pub const fn governor(&self) -> [u8; 32] {
        self.governor
    }

    pub fn current(&self) -> Option<WorkspaceAuthorityEnvelope> {
        self.current.lock().ok().and_then(|current| current.clone())
    }

    /// Reload and apply the latest atomically published signed revision: the bridge from a desktop
    /// capability change into the already-running authority service.
    pub fn reload(&self) -> anyhow::Result<bool> {
        let bytes = std::fs::read(&self.path)?;
        let envelope: WorkspaceAuthorityEnvelope = postcard::from_bytes(&bytes)?;
        self.apply(envelope)
    }

    /// Verify, durably replace and publish a strictly newer authority state. Equal revisions are
    /// accepted only when byte-identical, so retries are idempotent while equivocation at one
    /// revision is refused.
    pub fn apply(&self, next: WorkspaceAuthorityEnvelope) -> anyhow::Result<bool> {
        next.verify(self.workspace, self.governor)?;
        let mut current = self
            .current
            .lock()
            .map_err(|_| anyhow::anyhow!("authority lock poisoned"))?;
        if let Some(previous) = current.as_ref() {
            anyhow::ensure!(
                next.revision >= previous.revision,
                "authority revision regressed"
            );
            if next.revision == previous.revision {
                anyhow::ensure!(
                    postcard::to_allocvec(&next)? == postcard::to_allocvec(previous)?,
                    "authority equivocated at one revision"
                );
                return Ok(false);
            }
        }
        let bytes = postcard::to_allocvec(&next)?;
        tilo_files::native::atomic::write(&self.path, &bytes)?;
        self.handle
            .replace(project(self.subject, &self.principals, Some(&next)));
        *current = Some(next);
        drop(current);
        Ok(true)
    }
}

/// Keep `authority` in step with the host-local capability list at `path`: whenever the file's bytes
/// change, sign them as the next revision with `governor` and apply it. Bytes present when the task
/// starts count as already applied; an unreadable or malformed file changes nothing, so a
/// half-written list is never signed.
pub fn spawn_capability_file_reload(
    path: PathBuf,
    authority: std::sync::Arc<WorkspaceAuthority>,
    governor: ed25519_dalek::SigningKey,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let mut previous = std::fs::read(&path).ok();
    let mut changes = crate::file_watch::FileChanges::for_file(&path)?;
    Ok(tokio::spawn(async move {
        loop {
            changes.next().await;
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if previous.as_ref() == Some(&bytes) {
                continue;
            }
            let Ok(capabilities) = serde_json::from_slice::<collab_types::FsAuthority>(&bytes)
            else {
                continue;
            };
            let revision = authority
                .current()
                .map_or(1, |current| current.revision.saturating_add(1));
            let Ok(envelope) = WorkspaceAuthorityEnvelope::sign(
                authority.workspace(),
                revision,
                capabilities,
                false,
                &governor,
            ) else {
                continue;
            };
            if authority.apply(envelope).is_ok() {
                previous = Some(bytes);
            }
        }
    }))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityRequest {
    pub workspace: uuid::Uuid,
    #[serde(default)]
    pub publish: Option<WorkspaceAuthorityEnvelope>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityResponse {
    pub authority: Option<WorkspaceAuthorityEnvelope>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPeerRequest {
    pub endpoint: iroh_base::EndpointAddr,
    /// Exact NAS ticket returned by authenticated relay discovery; the daemon validates that it names
    /// this endpoint before making it durable. Omitted only by old clients, which keep blob-only
    /// behaviour.
    #[serde(default)]
    pub ticket: Option<String>,
    #[serde(default)]
    pub relay_service_id: Option<[u8; 32]>,
}

/// Authenticated local Files-control requests. The Unix socket is owned by the daemon and
/// group-restricted to the enrolled identity; callers cannot edit the cache directly.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ContentPeerControlRequest {
    Register(ContentPeerRequest),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPeerResponse {
    pub accepted: bool,
    /// The endpoint was durably stored, but its ticket names a different workspace from the
    /// currently-running record plane; the daemon restarts itself after replying so it joins that
    /// plane.
    #[serde(default)]
    pub rebind_required: bool,
    /// A managed system service has scheduled that rebind after this response was flushed. Ad-hoc
    /// `fs serve` callers get the same durable registration but must restart their daemon themselves.
    #[serde(default)]
    pub restart_scheduled: bool,
}

#[cfg(unix)]
/// Bind the local, group-restricted direct-content registration service.
pub fn serve_local_content_peer(
    socket: &Path,
    hydrator: std::sync::Arc<crate::Hydrator>,
    cache_dir: Option<PathBuf>,
    active_workspace: uuid::Uuid,
    rebind: Option<tokio::sync::mpsc::UnboundedSender<()>>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    use std::os::unix::fs::PermissionsExt;

    if socket.exists() {
        std::fs::remove_file(socket)?;
    }
    let listener = tokio::net::UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660))?;
    Ok(tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let hydrator = hydrator.clone();
            let cache_dir = cache_dir.clone();
            let rebind = rebind.clone();
            tokio::spawn(async move {
                let (accepted, rebind_required) =
                    match read_frame::<_, ContentPeerControlRequest>(&mut stream).await {
                        Ok(ContentPeerControlRequest::Register(request)) => {
                            let rebind_required = request
                                .ticket
                                .as_deref()
                                .and_then(|encoded| {
                                    Ticket::decode(encoded)
                                        .ok()
                                        .map(|ticket| ticket.workspace_id() != active_workspace)
                                })
                                .unwrap_or(false);
                            let persisted = cache_dir.as_deref().map_or(Ok(()), |cache| {
                                persist_content_peer(
                                    cache,
                                    PersistedContentPeer {
                                        endpoint: request.endpoint.clone(),
                                        ticket: request.ticket.clone(),
                                        relay_service_id: request.relay_service_id,
                                    },
                                )
                            });
                            if persisted.is_ok() {
                                hydrator.add_peer(request.endpoint);
                                (true, rebind_required)
                            } else {
                                (false, false)
                            }
                        }
                        Err(_) => (false, false),
                    };
                let restart_scheduled = rebind_required && rebind.is_some();
                let response_sent = write_frame(
                    &mut stream,
                    &ContentPeerResponse {
                        accepted,
                        rebind_required,
                        restart_scheduled,
                    },
                )
                .await
                .is_ok();
                // The client needs a successful acknowledgement before the service exits and systemd
                // starts the joined record plane.
                if response_sent
                    && accepted
                    && restart_scheduled
                    && let Some(rebind) = rebind
                {
                    let _ = rebind.send(());
                }
            });
        }
    }))
}

#[cfg(unix)]
/// Register a discovered direct Files endpoint with the local daemon.
pub async fn register_local_content_peer(
    socket: &std::path::Path,
    endpoint: iroh_base::EndpointAddr,
    ticket: Option<String>,
    relay_service_id: Option<[u8; 32]>,
) -> anyhow::Result<ContentPeerResponse> {
    let mut stream = tokio::net::UnixStream::connect(socket).await?;
    write_frame(
        &mut stream,
        &ContentPeerControlRequest::Register(ContentPeerRequest {
            endpoint,
            ticket,
            relay_service_id,
        }),
    )
    .await?;
    let response = read_frame::<_, ContentPeerResponse>(&mut stream).await?;
    anyhow::ensure!(
        response.accepted,
        "filesystem daemon rejected content endpoint"
    );
    Ok(response)
}

/// Start the local authority publisher service. The caller places the socket in a
/// `root:tilo-identity` directory; the socket is group writable so an enrolled desktop can submit a
/// governor-signed update without reaching the root-owned durable cache directly.
#[cfg(unix)]
/// Bind the local, group-restricted workspace-authority publisher service.
pub fn serve_local_authority(
    socket: &Path,
    authority: std::sync::Arc<WorkspaceAuthority>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    use std::os::unix::fs::PermissionsExt;

    if socket.exists() {
        std::fs::remove_file(socket)?;
    }
    let listener = tokio::net::UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660))?;
    Ok(tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let authority = authority.clone();
            tokio::spawn(async move {
                let request: anyhow::Result<AuthorityRequest> = read_frame(&mut stream).await;
                let response = match request {
                    Ok(request) if request.workspace == authority.workspace() => {
                        if let Some(envelope) = request.publish {
                            // `apply` verifies the workspace governor and strict revision monotonicity
                            // before replacing the ACL.
                            let _ = authority.apply(envelope);
                        }
                        AuthorityResponse {
                            authority: authority.current(),
                        }
                    }
                    _ => AuthorityResponse { authority: None },
                };
                let _ = write_frame(&mut stream, &response).await;
            });
        }
    }))
}

/// Submit or query a local authority service; the Unix permissions are the first gate, the signed
/// envelope the authority gate.
#[cfg(unix)]
pub async fn local_authority_request(
    socket: &std::path::Path,
    request: AuthorityRequest,
) -> anyhow::Result<AuthorityResponse> {
    let mut stream = tokio::net::UnixStream::connect(socket).await?;
    write_frame(&mut stream, &request).await?;
    read_frame(&mut stream).await
}

/// Each authority and content-peer control message, over the local socket and the peer protocol
/// alike: a little-endian `u32` length, then the postcard encoding.
const AUTHORITY_FRAMING: Framing = Framing::little_endian(MAX_AUTHORITY_FRAME);

pub async fn write_frame<W: tokio::io::AsyncWrite + Unpin + Send, T: Serialize + Sync>(
    writer: &mut W,
    value: &T,
) -> anyhow::Result<()> {
    AUTHORITY_FRAMING.write_value_async(writer, value).await?;
    Ok(())
}

pub async fn read_frame<R: tokio::io::AsyncRead + Unpin + Send, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> anyhow::Result<T> {
    Ok(AUTHORITY_FRAMING
        .read_value_async(reader)
        .await?
        .ok_or(FrameError::Closed)?)
}

fn project(
    subject: IdentityKey,
    principals: &[Principal],
    envelope: Option<&WorkspaceAuthorityEnvelope>,
) -> WorkspaceAcl {
    match envelope {
        Some(envelope) if !envelope.tombstoned => {
            WorkspaceAcl::from_capabilities(subject, &envelope.capabilities, principals, None)
        }
        _ => WorkspaceAcl::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn authority_frames_are_little_endian_length_then_postcard() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &AuthorityResponse { authority: None })
            .await
            .unwrap();
        assert_eq!(wire, [1, 0, 0, 0, 0]);

        let mut wire: Vec<u8> = vec![18, 0, 0, 0, 16];
        wire.extend_from_slice(&[0x44; 16]);
        wire.push(0);
        let request: AuthorityRequest = read_frame(&mut wire.as_slice()).await.unwrap();
        assert_eq!(request.workspace, uuid::Uuid::from_bytes([0x44; 16]));
        assert!(request.publish.is_none());
    }

    #[tokio::test]
    async fn authority_frames_reject_oversized_truncated_and_closed_streams() {
        let oversized = u32::try_from(MAX_AUTHORITY_FRAME + 1)
            .unwrap()
            .to_le_bytes();
        assert!(
            read_frame::<_, AuthorityResponse>(&mut &oversized[..])
                .await
                .is_err()
        );
        for wire in [&[][..], &[1, 0][..], &[2, 0, 0, 0, 0][..]] {
            let error = read_frame::<_, AuthorityResponse>(&mut &wire[..])
                .await
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<FrameError>().map(FrameError::kind),
                Some(std::io::ErrorKind::UnexpectedEof),
                "{wire:?}"
            );
        }
        let oversized = vec![0_u8; MAX_AUTHORITY_FRAME];
        let mut wire = Vec::new();
        assert!(write_frame(&mut wire, &oversized).await.is_err());
        assert!(wire.is_empty());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn peer_catalog_and_acl_tombstones_persist_and_reject_replay() {
        {
            let temp = tempfile::tempdir().unwrap();
            for byte in 1..=u8::try_from(MAX_PERSISTED_CONTENT_PEERS).unwrap() {
                persist_content_peer(
                    temp.path(),
                    PersistedContentPeer {
                        endpoint: iroh_base::EndpointAddr::new(
                            iroh::SecretKey::from_bytes(&[byte; 32]).public(),
                        ),
                        ticket: None,
                        relay_service_id: None,
                    },
                )
                .unwrap();
            }
            let refreshed_id = iroh::SecretKey::from_bytes(&[1; 32]).public();
            persist_content_peer(
                temp.path(),
                PersistedContentPeer {
                    endpoint: iroh_base::EndpointAddr::new(refreshed_id)
                        .with_ip_addr("127.0.0.1:4788".parse().unwrap()),
                    ticket: None,
                    relay_service_id: None,
                },
            )
            .unwrap();
            let replacement_id = iroh::SecretKey::from_bytes(&[0x7f; 32]).public();
            persist_content_peer(
                temp.path(),
                PersistedContentPeer {
                    endpoint: iroh_base::EndpointAddr::new(replacement_id),
                    ticket: None,
                    relay_service_id: None,
                },
            )
            .unwrap();

            let peers = load_persisted_content_peers(temp.path()).unwrap();
            assert_eq!(peers.len(), MAX_PERSISTED_CONTENT_PEERS);
            assert!(peers.iter().any(|peer| {
                peer.endpoint
                    == iroh_base::EndpointAddr::new(refreshed_id)
                        .with_ip_addr("127.0.0.1:4788".parse().unwrap())
            }));
            assert!(peers.iter().any(|peer| peer.endpoint.id == replacement_id));
            let evicted = iroh::SecretKey::from_bytes(&[2; 32]).public();
            assert!(!peers.iter().any(|peer| peer.endpoint.id == evicted));
        }
        {
            let temp = tempfile::tempdir().unwrap();
            let governor = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let workspace = uuid::Uuid::new_v4();
            let subject = [4; 32];
            let authority = WorkspaceAuthority::open(
                temp.path().join("authority.bin"),
                workspace,
                governor.verifying_key().to_bytes(),
                subject,
                Vec::new(),
            )
            .unwrap();
            let grant = CapOverride::new(
                Subject::User(collab_types::Ed25519PublicKey(subject)),
                Scope::FsPath("/".into()),
                Effect::Grant,
                CapAccess::Read,
            );
            assert!(
                authority
                    .apply(
                        WorkspaceAuthorityEnvelope::sign(
                            workspace,
                            1,
                            vec![grant],
                            false,
                            &governor
                        )
                        .unwrap()
                    )
                    .unwrap()
            );
            assert!(authority.handle().permits(&subject, "/file", Access::Read));
            assert!(
                authority
                    .apply(
                        WorkspaceAuthorityEnvelope::sign(workspace, 2, Vec::new(), true, &governor)
                            .unwrap()
                    )
                    .unwrap()
            );
            assert!(!authority.handle().permits(&subject, "/file", Access::Read));
            assert!(
                authority
                    .apply(
                        WorkspaceAuthorityEnvelope::sign(workspace, 1, Vec::new(), true, &governor)
                            .unwrap()
                    )
                    .is_err()
            );
        }
    }
    use crate::Access;
    use collab_types::{Access as CapAccess, Effect, Scope, Subject};
    use std::sync::Arc;

    /// The workspace authority follows the host-local capability list through its file watch: a
    /// revocation written to the list is signed, applied and enforced, and a half-written list is
    /// never signed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_revoked_capability_leaves_the_workspace_authority_through_the_watch() {
        let temp = tempfile::tempdir().unwrap();
        let governor = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let workspace = uuid::Uuid::new_v4();
        let subject = [4; 32];
        let grant = |path: &str| {
            CapOverride::new(
                Subject::User(collab_types::Ed25519PublicKey(subject)),
                Scope::FsPath(path.into()),
                Effect::Grant,
                CapAccess::Read,
            )
        };
        let list = temp.path().join("fs-capabilities.json");
        std::fs::write(
            &list,
            serde_json::to_vec(&collab_types::FsAuthority::from(vec![
                grant("/docs"),
                grant("/plans"),
            ]))
            .unwrap(),
        )
        .unwrap();
        let authority = Arc::new(
            WorkspaceAuthority::open(
                temp.path().join("authority.bin"),
                workspace,
                governor.verifying_key().to_bytes(),
                subject,
                Vec::new(),
            )
            .unwrap(),
        );
        authority
            .apply(
                WorkspaceAuthorityEnvelope::sign(
                    workspace,
                    1,
                    vec![grant("/docs"), grant("/plans")],
                    false,
                    &governor,
                )
                .unwrap(),
            )
            .unwrap();
        let handle = authority.handle();
        let reload =
            spawn_capability_file_reload(list.clone(), Arc::clone(&authority), governor.clone())
                .unwrap();
        assert!(handle.permits(&subject, "/plans/b.md", Access::Read));

        std::fs::write(&list, b"{\"capabilities\":[{\"subject\"").unwrap();

        std::fs::write(
            &list,
            serde_json::to_vec(&collab_types::FsAuthority::from(vec![grant("/docs")])).unwrap(),
        )
        .unwrap();
        tilo_test_env::within("the revocation to reach the workspace authority", async {
            while handle.permits(&subject, "/plans/b.md", Access::Read) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await;
        assert_eq!(
            authority.current().unwrap().revision,
            2,
            "a half-written list must never be signed"
        );
        assert!(handle.permits(&subject, "/docs/a.md", Access::Read));
        reload.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_publisher_finds_the_answering_authority_by_its_socket_name() {
        let temp = tempfile::tempdir().unwrap();
        let governor = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let workspace = uuid::Uuid::new_v4();
        let authority = Arc::new(
            WorkspaceAuthority::open(
                temp.path().join("authority.bin"),
                workspace,
                governor.verifying_key().to_bytes(),
                [1; 32],
                Vec::new(),
            )
            .unwrap(),
        );
        let sockets = temp.path().join("run");
        std::fs::create_dir(&sockets).unwrap();
        assert!(
            find_local_authority_in(&sockets).await.is_err(),
            "no daemon is serving yet"
        );
        let stale = sockets.join(format!(
            "{LOCAL_AUTHORITY_SOCKET_PREFIX}{}{LOCAL_SOCKET_SUFFIX}",
            uuid::Uuid::new_v4()
        ));
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        std::fs::write(sockets.join("workspace-content-x.sock"), b"").unwrap();
        let live = sockets.join(format!(
            "{LOCAL_AUTHORITY_SOCKET_PREFIX}{workspace}{LOCAL_SOCKET_SUFFIX}"
        ));
        let task = serve_local_authority(&live, authority).unwrap();
        assert_eq!(
            find_local_authority_in(&sockets).await.unwrap(),
            (workspace, live)
        );
        let other_workspace = uuid::Uuid::new_v4();
        let other_authority = Arc::new(
            WorkspaceAuthority::open(
                temp.path().join("other-authority.bin"),
                other_workspace,
                governor.verifying_key().to_bytes(),
                [1; 32],
                Vec::new(),
            )
            .unwrap(),
        );
        let other_socket = sockets.join(format!(
            "{LOCAL_AUTHORITY_SOCKET_PREFIX}{other_workspace}{LOCAL_SOCKET_SUFFIX}"
        ));
        let other_task = serve_local_authority(&other_socket, other_authority).unwrap();
        assert!(
            find_local_authority_in(&sockets).await.is_err(),
            "multiple live workspace authorities must not be guessed"
        );
        other_task.abort();
        task.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn authenticated_discovery_updates_live_routes_and_rebinds() {
        {
            let temp = tempfile::tempdir().unwrap();
            let governor = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
            let workspace = uuid::Uuid::new_v4();
            let authority = Arc::new(
                WorkspaceAuthority::open(
                    temp.path().join("authority.bin"),
                    workspace,
                    governor.verifying_key().to_bytes(),
                    [1; 32],
                    Vec::new(),
                )
                .unwrap(),
            );
            let socket = temp.path().join(LOCAL_AUTHORITY_SOCKET);
            let task = serve_local_authority(&socket, authority.clone()).unwrap();
            let first =
                WorkspaceAuthorityEnvelope::sign(workspace, 1, Vec::new(), false, &governor)
                    .unwrap();
            let first_response = local_authority_request(
                &socket,
                AuthorityRequest {
                    workspace,
                    publish: Some(first.clone()),
                },
            )
            .await
            .unwrap();
            assert_eq!(first_response.authority, Some(first));

            let left = WorkspaceAuthorityEnvelope::sign(workspace, 2, Vec::new(), false, &governor)
                .unwrap();
            let right = WorkspaceAuthorityEnvelope::sign(workspace, 2, Vec::new(), true, &governor)
                .unwrap();
            let (left_response, right_response) = tokio::join!(
                local_authority_request(
                    &socket,
                    AuthorityRequest {
                        workspace,
                        publish: Some(left.clone())
                    },
                ),
                local_authority_request(
                    &socket,
                    AuthorityRequest {
                        workspace,
                        publish: Some(right.clone())
                    },
                )
            );
            let left_response = left_response.unwrap().authority.unwrap();
            let right_response = right_response.unwrap().authority.unwrap();
            assert!(left_response == left || right_response == right);
            assert_eq!(left_response, right_response);
            task.abort();
        }
        {
            let temp = tempfile::tempdir().unwrap();
            let endpoint = test_relay::direct_endpoint().await;
            let hydrator = Arc::new(crate::Hydrator::new(
                endpoint.clone(),
                Arc::new(crate::ContentStore::memory()),
            ));
            let socket = temp.path().join("content.sock");
            let cache = temp.path().join("cache");
            std::fs::create_dir_all(&cache).unwrap();
            let active_workspace = Ticket::workspace_id_for_topic([0x71; 32]);
            let (rebind_tx, mut rebind_rx) = tokio::sync::mpsc::unbounded_channel();
            let task = serve_local_content_peer(
                &socket,
                hydrator.clone(),
                Some(cache.clone()),
                active_workspace,
                Some(rebind_tx),
            )
            .unwrap();
            let discovered =
                iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x42; 32]).public())
                    .with_ip_addr("127.0.0.1:45454".parse().unwrap());
            let ticket = Ticket {
                node: discovered.clone(),
                workspace: [0x71; 32],
            }
            .encode()
            .unwrap();
            register_local_content_peer(&socket, discovered.clone(), Some(ticket.clone()), None)
                .await
                .unwrap();
            assert!(hydrator.known_peers().contains(&discovered));
            assert_eq!(
                load_persisted_content_peers(&cache).unwrap(),
                vec![PersistedContentPeer {
                    endpoint: discovered,
                    ticket: Some(ticket),
                    relay_service_id: None,
                }]
            );
            assert!(rebind_rx.try_recv().is_err());
            task.abort();
            endpoint.close().await;
        }
        {
            let temp = tempfile::tempdir().unwrap();
            let endpoint = test_relay::direct_endpoint().await;
            let hydrator = Arc::new(crate::Hydrator::new(
                endpoint.clone(),
                Arc::new(crate::ContentStore::memory()),
            ));
            let socket = temp.path().join("content.sock");
            let cache = temp.path().join("cache");
            std::fs::create_dir_all(&cache).unwrap();
            let workspace = Ticket::workspace_id_for_topic([0x71; 32]);
            let task = serve_local_content_peer(
                &socket,
                hydrator.clone(),
                Some(cache.clone()),
                workspace,
                None,
            )
            .unwrap();
            let discovered =
                iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x42; 32]).public());
            let relay_service_id = [0x99; 32];
            let ticket = Ticket {
                node: discovered.clone(),
                workspace: [0x71; 32],
            }
            .encode()
            .unwrap();
            register_local_content_peer(
                &socket,
                discovered.clone(),
                Some(ticket.clone()),
                Some(relay_service_id),
            )
            .await
            .unwrap();
            assert!(hydrator.known_peers().contains(&discovered));
            assert_eq!(
                load_persisted_content_peers(&cache).unwrap(),
                vec![PersistedContentPeer {
                    endpoint: discovered,
                    ticket: Some(ticket),
                    relay_service_id: Some(relay_service_id),
                }]
            );
            task.abort();
            endpoint.close().await;
        }
        {
            let temp = tempfile::tempdir().unwrap();
            let endpoint = test_relay::direct_endpoint().await;
            let hydrator = Arc::new(crate::Hydrator::new(
                endpoint.clone(),
                Arc::new(crate::ContentStore::memory()),
            ));
            let socket = temp.path().join("content.sock");
            let cache = temp.path().join("cache");
            std::fs::create_dir_all(&cache).unwrap();
            let active_workspace = uuid::Uuid::from_u128(0x11);
            let remote_workspace = [0x22; 32];
            let (rebind_tx, mut rebind_rx) = tokio::sync::mpsc::unbounded_channel();
            let task = serve_local_content_peer(
                &socket,
                hydrator.clone(),
                Some(cache.clone()),
                active_workspace,
                Some(rebind_tx),
            )
            .unwrap();
            let nas =
                iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x42; 32]).public());
            let owner =
                iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x43; 32]).public());
            let ticket = Ticket {
                node: owner,
                workspace: remote_workspace,
            }
            .encode()
            .unwrap();

            register_local_content_peer(&socket, nas.clone(), Some(ticket.clone()), None)
                .await
                .unwrap();
            assert!(hydrator.known_peers().contains(&nas));
            let restored = load_persisted_content_peers(&cache).unwrap()[0]
                .decoded_ticket()
                .unwrap()
                .unwrap();
            assert_eq!(restored.workspace, remote_workspace);
            assert_ne!(restored.node.id, nas.id, "the owner and NAS are distinct");
            tilo_test_env::within("the daemon restart request", rebind_rx.recv())
                .await
                .expect("different workspace ticket asks the daemon to restart into it");
            task.abort();
            endpoint.close().await;
        }
    }

    #[test]
    fn an_envelope_names_its_members_under_the_stored_key() {
        let governor = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
        let workspace = uuid::Uuid::new_v4();
        let member = collab_types::Ed25519PublicKey([0x52; 32]);
        let envelope = WorkspaceAuthorityEnvelope::sign(
            workspace,
            3,
            vec![CapOverride::new(
                Subject::User(member),
                Scope::FsPath("/".into()),
                Effect::Grant,
                CapAccess::Read,
            )],
            false,
            &governor,
        )
        .unwrap();

        let written = serde_json::to_string(&envelope).unwrap();
        assert!(written.contains("\"room_members\""), "{written}");
        assert!(!written.contains("\"workspace_members\""), "{written}");
        let stored = written.replace("\"room_members\"", "\"workspace_members\"");
        let read: WorkspaceAuthorityEnvelope = serde_json::from_str(&stored).unwrap();
        assert_eq!(read, envelope);
        read.verify(workspace, governor.verifying_key().to_bytes())
            .unwrap();
    }
}
