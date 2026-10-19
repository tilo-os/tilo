//! A session peer that serves another workspace's namespace, validly
//! signed, gets none of it applied.

use super::*;
use crate::namespace::ROOT_INO;
use crate::sync::bind_certified_test_endpoint;
use iroh::protocol::Router;

/// Another workspace's namespace: a real, valid tree, just not this one.
fn other_workspace_namespace() -> Namespace {
    let mut namespace = Namespace::new("other-workspace").unwrap();
    namespace
        .create_dir(ROOT_INO, "other-workspace-secret", 1)
        .unwrap();
    namespace
}

/// How the rogue answers a namespace request.
#[derive(Clone, Copy, Debug)]
enum Rogue {
    /// Binds its answer to this workspace, whatever the request named.
    BindTo(uuid::Uuid),
    /// Sends the changes without any binding.
    Unbound,
}

/// A certified session peer that answers namespace requests as `rogue`
/// says instead of checking the request's workspace.
#[derive(Clone)]
struct RogueSessionServer {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    rogue: Rogue,
}

impl std::fmt::Debug for RogueSessionServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RogueSessionServer")
    }
}

impl RogueSessionServer {
    async fn answer(&self, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
        let request: SessionRequestV1 = read_frame(&mut recv, MAX_FRAME_BYTES).await?;
        let mut other = other_workspace_namespace();
        match request.operation {
            SessionOperationV1::NamespaceSince { .. } => {
                let delta = other.changes_since(&[]);
                write_frame(&mut send, &SessionFrameV1::NamespaceChunk(delta.clone())).await?;
                if let Rogue::BindTo(workspace) = self.rogue {
                    let binding = NamespaceBindingV1::sign(
                        workspace,
                        NamespacePayloadKind::Changes,
                        &delta,
                        self.identity.credentials.signing_key(),
                    )?;
                    write_frame(&mut send, &SessionFrameV1::NamespaceBound(binding)).await?;
                }
                write_frame(&mut send, &SessionFrameV1::Done).await?;
            }
            SessionOperationV1::NamespaceSync => {
                write_frame(&mut send, &SessionFrameV1::Done).await?;
                let _: SessionFrameV1 = read_frame(&mut recv, MAX_SYNC_FRAME_BYTES).await?;
                let message = other
                    .sync_generate(&mut automerge::sync::State::new())
                    .unwrap_or_default();
                let Rogue::BindTo(workspace) = self.rogue else {
                    anyhow::bail!("an unbound sync message is not expressible");
                };
                let bound = self.identity.bind_namespace(
                    workspace,
                    NamespacePayloadKind::SyncMessage,
                    message,
                )?;
                write_frame(&mut send, &SessionFrameV1::Sync(Some(bound))).await?;
            }
            _ => write_frame(&mut send, &SessionFrameV1::Denied).await?,
        }
        send.finish()?;
        Ok(())
    }
}

impl ProtocolHandler for RogueSessionServer {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        authenticate_connection(
            &self.endpoint,
            &connection,
            &mut recv,
            &mut send,
            &self.identity,
        )
        .await
        .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        send.finish()
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        while let Ok((send, recv)) = connection.accept_bi().await {
            let _ = self.answer(send, recv).await;
        }
        Ok(())
    }
}

struct Fixture {
    _router: Router,
    pool: SessionPool,
    session: Arc<Session>,
    namespace: Arc<Mutex<Namespace>>,
}

async fn session_to_rogue(workspace: uuid::Uuid, rogue: Rogue) -> Fixture {
    let (rogue_endpoint, _lookup, rogue_identity) = bind_certified_test_endpoint().await;
    let router = Router::builder(rogue_endpoint.clone())
        .accept(
            TILO_FS_SESSION_ALPN,
            RogueSessionServer {
                endpoint: rogue_endpoint.clone(),
                identity: rogue_identity,
                rogue,
            },
        )
        .spawn();
    let (endpoint, _lookup, identity) = bind_certified_test_endpoint().await;
    let pool = SessionPool::new(endpoint, identity, workspace);
    let session = pool
        .get(&rogue_endpoint.addr())
        .await
        .expect("the rogue speaks the session protocol");
    Fixture {
        _router: router,
        pool,
        session,
        namespace: Arc::new(Mutex::new(Namespace::new("requester").unwrap())),
    }
}

fn merged_other_workspace(namespace: &Arc<Mutex<Namespace>>) -> bool {
    namespace
        .lock()
        .unwrap()
        .entry_at("other-workspace-secret")
        .is_some()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pull_of_another_workspaces_signed_changes_is_refused() {
    let requested = uuid::Uuid::new_v4();
    let other = uuid::Uuid::new_v4();
    let f = session_to_rogue(requested, Rogue::BindTo(other)).await;

    let error = pull_namespace(&f.pool, &f.session, &f.namespace, &None)
        .await
        .expect_err("another workspace's changes must not be applied");
    assert!(
        format!("{error:#}").contains(&format!("belongs to workspace {other}")),
        "{error:#}"
    );
    assert!(!merged_other_workspace(&f.namespace));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pull_of_unbound_changes_is_refused() {
    let f = session_to_rogue(uuid::Uuid::new_v4(), Rogue::Unbound).await;

    let error = pull_namespace(&f.pool, &f.session, &f.namespace, &None)
        .await
        .expect_err("changes without a binding must not be applied");
    assert!(
        format!("{error:#}").contains("without a workspace binding"),
        "{error:#}"
    );
    assert!(!merged_other_workspace(&f.namespace));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pull_from_a_device_outside_the_policy_is_refused() {
    let workspace = uuid::Uuid::new_v4();
    let f = session_to_rogue(workspace, Rogue::BindTo(workspace)).await;
    let mut acl = crate::acl::WorkspaceAcl::new();
    acl.grant_read(f.pool.identity.user_public_key(), "/");
    let policy = Some(crate::acl::AclHandle::new(acl));

    let error = pull_namespace(&f.pool, &f.session, &f.namespace, &policy)
        .await
        .expect_err("a device whose holder may not read the workspace serves no changes");
    assert!(format!("{error:#}").contains("not authorised"), "{error:#}");
    assert!(!merged_other_workspace(&f.namespace));

    // Bound to this workspace by an unrestricted peer, the same changes apply.
    let (changed, _) = pull_namespace(&f.pool, &f.session, &f.namespace, &None)
        .await
        .unwrap();
    assert!(changed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sync_message_of_another_workspace_over_a_session_is_refused() {
    let requested = uuid::Uuid::new_v4();
    let other = uuid::Uuid::new_v4();
    let f = session_to_rogue(requested, Rogue::BindTo(other)).await;

    let error = sync_namespace(&f.pool, &f.session, &f.namespace, None, &None)
        .await
        .expect_err("another workspace's sync message must not be applied");
    assert!(
        format!("{error:#}").contains(&format!("belongs to workspace {other}")),
        "{error:#}"
    );
    assert!(!merged_other_workspace(&f.namespace));
}

#[tokio::test]
async fn session_frames_are_big_endian_length_then_postcard() {
    let mut wire = Vec::new();
    SESSION_FRAMING
        .write_value_async(&mut wire, &SessionFrameV1::NamespaceChunk(b"ab".to_vec()))
        .await
        .unwrap();
    assert_eq!(wire, [0, 0, 0, 4, 5, 2, b'a', b'b']);

    let frame: Option<SessionFrameV1> = SESSION_FRAMING
        .with_max_len(MAX_FRAME_BYTES)
        .read_value_async(&mut &[0, 0, 0, 1, 9][..])
        .await
        .unwrap();
    assert!(matches!(frame, Some(SessionFrameV1::Done)));
}

#[tokio::test]
async fn session_reads_are_bounded_by_the_step_limit() {
    let over = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes();
    assert!(matches!(
        SESSION_FRAMING
            .with_max_len(MAX_FRAME_BYTES)
            .read_value_async::<_, SessionFrameV1>(&mut &over[..])
            .await,
        Err(FrameError::TooLarge(_))
    ));
    // A sync round may carry up to the larger sync bound.
    assert!(matches!(
        SESSION_FRAMING
            .with_max_len(MAX_SYNC_FRAME_BYTES)
            .read_value_async::<_, SessionFrameV1>(&mut &over[..])
            .await,
        Err(FrameError::Truncated)
    ));
    let too_big = u32::try_from(MAX_SYNC_FRAME_BYTES + 1)
        .unwrap()
        .to_be_bytes();
    assert!(matches!(
        SESSION_FRAMING
            .with_max_len(MAX_SYNC_FRAME_BYTES)
            .read_value_async::<_, SessionFrameV1>(&mut &too_big[..])
            .await,
        Err(FrameError::TooLarge(_))
    ));
}
