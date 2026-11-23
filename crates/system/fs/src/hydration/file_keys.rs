//! Sealing file content with the same access as the workspace's Files tree.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, ensure};
use bytes::Bytes;
use collab_types::{ArtifactId, ContentRetention, EncryptionMetadata, WorkspaceId};
use fs_content::{ChunkRef, SealedChunks};
use iroh_blobs::Hash;
use key_group::ContentKeys;
use tilo_identity_client::BrokerSigningIdentity;
use tilo_identity_client::wire::{
    HANDSHAKE_FRAMING, Handshake, KeyAccess, PRESENTATION_VERSION, PresentationHandshake,
};
use tilo_lock::MutexRecover as _;
use tokio::io::AsyncReadExt as _;

use crate::acl::{Access, AclHandle, IdentityKey};
use crate::content::{self, Manifest};
use fs_protocol::tree::{FILES_APP, document_id};

use super::Hydrator;

/// This device's connection to the session's identity service as the Files endpoint of
/// `workspace`: the app and workspace the keys of the tree and its files are handed for.
fn connect_identity(identity: &Path, workspace: &str) -> Result<BrokerSigningIdentity> {
    use std::io::Write as _;
    let mut stream = tilo_files::native::socket::connect(identity)
        .with_context(|| format!("connect to the identity service at {}", identity.display()))?;
    let handshake = PresentationHandshake {
        handshake: Handshake {
            workspace_id: workspace.to_owned(),
            app_id: FILES_APP.to_owned(),
            surface: None,
        },
        presentation_version: PRESENTATION_VERSION,
    };
    let bytes = serde_json::to_vec(&handshake).context("this connection's handshake is JSON")?;
    HANDSHAKE_FRAMING
        .write(&mut stream, &bytes)
        .context("name this device's app and workspace on the identity connection")?;
    stream
        .flush()
        .context("the identity service reads this connection's handshake")?;
    BrokerSigningIdentity::from_connected_io(stream)
        .context("the identity service answers this device")
}

type ReadKeys = dyn Fn() -> Result<ContentKeys> + Send + Sync;

/// Each workspace's content keys as one run of saves and reads on a thread holds them
/// ([`Hydrator::key_run`]): fetched when the run first needs them, checked once when it ends.
type KeyRun = BTreeMap<WorkspaceId, (FileKeys, ContentKeys)>;

thread_local! {
    /// The key run this thread is in, if any. A run is a thread's own: a save on another thread
    /// (a mount's writer beside a folder pass) never seals under keys a run it is not part of
    /// will check.
    static RUN: RefCell<Option<KeyRun>> = const { RefCell::new(None) };
}

/// The keys this thread's run holds for `workspace`; `Err(())` when the thread is in no run.
fn run_keys(workspace: WorkspaceId) -> std::result::Result<Option<ContentKeys>, ()> {
    RUN.with_borrow(|run| {
        run.as_ref()
            .map(|run| run.get(&workspace).map(|(_, keys)| keys.clone()))
            .ok_or(())
    })
}

/// Keep `keys`, just fetched from `source`, for the rest of this thread's run; the first keys
/// kept for a workspace are the ones the run's end checks.
fn keep_in_run(source: &FileKeys, keys: &ContentKeys) {
    RUN.with_borrow_mut(|run| {
        if let Some(run) = run {
            run.entry(source.workspace)
                .or_insert_with(|| (source.clone(), keys.clone()));
        }
    });
}

/// Connections to the identity service a workspace's key reads keep open while idle: one per
/// writer a mount runs at once, beyond which a burst opens more and closes them after.
const IDLE_KEY_LINKS: usize = 16;

/// What a caller waiting for a workspace's Files content keys is told.
pub const KEYS_NOT_READY: &str = "the Files content keys are not ready";

/// The key authority for the Files endpoint's workspace.
#[derive(Clone)]
pub struct FileKeys {
    workspace: WorkspaceId,
    read: Arc<ReadKeys>,
    /// The keys the last save read once it had sealed its file: what the next save seals with,
    /// which its own read after sealing checks.
    sealing: Arc<Mutex<Option<ContentKeys>>>,
}

impl FileKeys {
    #[must_use]
    pub fn new(workspace: WorkspaceId, identity: PathBuf) -> Self {
        let scope = workspace.to_string();
        let connect_scope = scope.clone();
        Self::pooled(
            workspace,
            move || connect_identity(&identity, &connect_scope),
            move |connection: &mut BrokerSigningIdentity| {
                let reply = connection
                    .group_keys(&scope, FILES_APP, &document_id(), 0)
                    .map_err(|error| anyhow!("could not read the Files keys: {error}"))?;
                Ok(if reply.access == KeyAccess::Ready {
                    reply.keys.ok_or_else(|| anyhow!("no Files keys returned"))
                } else {
                    Err(anyhow!("{KEYS_NOT_READY}"))
                })
            },
        )
    }

    /// Keys read over connections `connect` opens, each asked by `read`. A connection answers one
    /// request at a time, so files saved at once (a mount's writers, each closing one) each take
    /// an idle connection or open another, rather than queueing on one; up to [`IDLE_KEY_LINKS`]
    /// stay open for the next. `read` answers what the service said, the encoded keys or why
    /// there are none, or why the connection failed, which closes it.
    fn pooled<C: Send + 'static>(
        workspace: WorkspaceId,
        connect: impl Fn() -> Result<C> + Send + Sync + 'static,
        read: impl Fn(&mut C) -> Result<Result<Vec<u8>>> + Send + Sync + 'static,
    ) -> Self {
        let idle = Mutex::new(Vec::<C>::new());
        Self::from_session(workspace, move || {
            let held = idle.lock_recover().pop();
            let mut connection = match held {
                Some(connection) => connection,
                None => connect()?,
            };
            let said = read(&mut connection)?;
            let mut idle = idle.lock_recover();
            if idle.len() < IDLE_KEY_LINKS {
                idle.push(connection);
            }
            drop(idle);
            ContentKeys::from_bytes(&said?).context("decoding Files content keys")
        })
    }

    /// Take keys directly from the device's authenticated workspace session.
    #[must_use]
    pub fn from_session(
        workspace: WorkspaceId,
        read: impl Fn() -> Result<ContentKeys> + Send + Sync + 'static,
    ) -> Self {
        Self {
            workspace,
            read: Arc::new(read),
            sealing: Arc::default(),
        }
    }

    /// The keys as they are now.
    ///
    /// # Errors
    /// The key authority is not reachable or refuses them.
    pub async fn access(&self) -> Result<ContentKeys> {
        let read = Arc::clone(&self.read);
        tokio::task::spawn_blocking(move || read()).await?
    }

    async fn save(
        &self,
        store: &crate::store::ContentStore,
        manifest: &Manifest,
        source: impl tokio::io::AsyncRead + Unpin,
    ) -> Result<Hash> {
        // In a run, the keys it holds seal the file, and the run's end checks them once for all
        // it sealed.
        if let Ok(held) = run_keys(self.workspace) {
            let keys = if let Some(keys) = held {
                keys
            } else {
                let keys = self.access().await?;
                keep_in_run(self, &keys);
                keys
            };
            return write_sealed_chunks(store, &keys, self.workspace, manifest, source).await;
        }
        // Outside a run, the keys the last save checked seal the file, and one read after sealing
        // says whether they are still the workspace's: a file sealed under keys that moved, before
        // or while it was saved, is refused, and the next save seals with the keys read here.
        let held = self.sealing.lock_recover().clone();
        let keys = match held {
            Some(keys) => keys,
            None => self.access().await?,
        };
        let hash = write_sealed_chunks(store, &keys, self.workspace, manifest, source).await?;
        let current = self.access().await;
        *self.sealing.lock_recover() = current.as_ref().ok().cloned();
        let current = current?;
        ensure!(
            (current.workspace(), current.epoch()) == (keys.workspace(), keys.epoch()),
            "file access changed while it was saved",
        );
        Ok(hash)
    }

    fn artifact(&self) -> ArtifactId {
        collab_types::file_content_artifact(self.workspace)
    }

    fn metadata(&self, sealed: SealedChunks) -> EncryptionMetadata {
        let group = WorkspaceId::from_u128(u128::from_be_bytes(sealed.group));
        EncryptionMetadata::content_group(
            self.workspace,
            group,
            sealed.epoch,
            ContentRetention::Delta,
        )
    }
}

enum ScopedKeyService {
    Identity(PathBuf),
    /// The session serving this device's trees, over the tree's own connection.
    Tree {
        service: crate::tree_sync::remote::Service,
        runtime: tokio::runtime::Handle,
    },
}

impl ScopedKeyService {
    fn file_keys(&self, workspace: WorkspaceId) -> FileKeys {
        match self {
            Self::Identity(path) => FileKeys::new(workspace, path.clone()),
            Self::Tree { service, runtime } => {
                let service = service.clone();
                let runtime = runtime.clone();
                FileKeys::from_session(workspace, move || runtime.block_on(service.keys(workspace)))
            }
        }
    }
}

/// The managed mount shares one namespace between the person's folders and signed workspace roots.
pub(super) struct ScopedFileKeys {
    authority: ScopedKeyService,
    policy: AclHandle,
    reader: IdentityKey,
    sources: Mutex<BTreeMap<WorkspaceId, FileKeys>>,
}

impl ScopedFileKeys {
    fn authorize(&self, workspace: WorkspaceId, access: Access) -> Result<()> {
        let root = tilo_paths::workspace_tree(Path::new("/"), &workspace.to_string())
            .ok_or_else(|| anyhow!("invalid workspace folder"))?;
        ensure!(
            self.policy
                .permits_workspace_tree(&self.reader, &root.to_string_lossy(), access),
            "workspace Files access was refused"
        );
        Ok(())
    }

    fn source(&self, workspace: WorkspaceId, access: Access) -> Result<FileKeys> {
        self.authorize(workspace, access)?;
        Ok(self
            .sources
            .lock_recover()
            .entry(workspace)
            .or_insert_with(|| self.authority.file_keys(workspace))
            .clone())
    }
}

pub(super) struct FileDecoder {
    keys: ContentKeys,
    metadata: EncryptionMetadata,
    artifact: ArtifactId,
}

impl FileDecoder {
    pub(super) fn open(&self, bytes: &[u8], expected: u32) -> Result<Bytes> {
        let clear = self
            .keys
            .open_content(self.artifact, &self.metadata, bytes)?;
        ensure!(
            clear.len() as u64 == u64::from(expected),
            "unexpected file chunk length"
        );
        Ok(Bytes::copy_from_slice(&clear))
    }
}

impl Hydrator {
    /// Runs `apply` with each workspace's content keys fetched once for everything it seals or
    /// opens on this thread, and checked once when it ends, rather than fetched before and after
    /// every file it seals. Answers whether they moved meanwhile, or could not be checked: what
    /// `apply` sealed is then under keys the workspace may no longer use, and must be sealed
    /// again. A run inside another is part of it; the outer one checks.
    pub fn key_run<T>(&self, apply: impl FnOnce() -> T) -> (T, bool) {
        /// Ends this thread's run however `apply` returns, a panic included.
        struct Ends;
        impl Drop for Ends {
            fn drop(&mut self) {
                RUN.with_borrow_mut(Option::take);
            }
        }
        if RUN.with_borrow(Option::is_some) {
            return (apply(), false);
        }
        RUN.with_borrow_mut(|run| *run = Some(BTreeMap::new()));
        let ends = Ends;
        let value = apply();
        let held = RUN.with_borrow_mut(Option::take).unwrap_or_default();
        drop(ends);
        let moved = held
            .into_values()
            .any(|(source, keys)| match self.block_on(source.access()) {
                Ok(current) => {
                    (current.workspace(), current.epoch()) != (keys.workspace(), keys.epoch())
                }
                Err(error) => {
                    tracing::warn!(%error, "the Files keys a run sealed with could not be checked");
                    true
                }
            });
        (value, moved)
    }

    /// Use the workspace key for each governed folder of the managed Files mount.
    #[must_use]
    pub fn with_scoped_file_keys(
        self,
        identity: PathBuf,
        policy: AclHandle,
        reader: IdentityKey,
    ) -> Self {
        self.with_key_service(ScopedKeyService::Identity(identity), policy, reader)
    }

    /// Use the keys the session serving this device's trees hands the Files endpoint, over the
    /// tree's connection: a daemon that cannot reach the person's identity itself opens nothing
    /// more to get them.
    #[must_use]
    pub fn with_tree_file_keys(
        self,
        service: crate::tree_sync::remote::Service,
        policy: AclHandle,
        reader: IdentityKey,
    ) -> Self {
        let authority = ScopedKeyService::Tree {
            service,
            runtime: self.runtime_handle.clone(),
        };
        self.with_key_service(authority, policy, reader)
    }

    fn with_key_service(
        mut self,
        authority: ScopedKeyService,
        policy: AclHandle,
        reader: IdentityKey,
    ) -> Self {
        self.scoped_file_keys = Some(ScopedFileKeys {
            authority,
            policy,
            reader,
            sources: Mutex::default(),
        });
        self
    }

    pub(crate) fn scoped_workspace(&self, path: &str) -> Option<WorkspaceId> {
        self.scoped_file_keys.as_ref()?;
        let mut components = path.strip_prefix('/')?.split('/');
        if components.next()? != tilo_paths::WORKSPACE_TREES_DIR {
            return None;
        }
        let workspace = components.next()?;
        WorkspaceId::parse(workspace).filter(|id| id.to_string() == workspace)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn has_scoped_file_keys(&self) -> bool {
        self.scoped_file_keys.is_some()
    }

    pub(crate) fn native_workspace(&self, relative: &Path) -> Option<WorkspaceId> {
        use std::path::Component;

        self.scoped_file_keys.as_ref()?;
        let mut components = relative.components();
        let Component::Normal(root) = components.next()? else {
            return None;
        };
        let Component::Normal(workspace) = components.next()? else {
            return None;
        };
        if root != tilo_paths::WORKSPACE_TREES_DIR {
            return None;
        }
        let workspace = workspace.to_str()?;
        WorkspaceId::parse(workspace).filter(|id| id.to_string() == workspace)
    }

    pub(crate) fn authorize_scoped_read(&self, workspace: Option<WorkspaceId>) -> Result<()> {
        if let Some(workspace) = workspace {
            self.scoped_file_keys
                .as_ref()
                .ok_or_else(|| anyhow!("this mount has no workspace file policy"))?
                .authorize(workspace, Access::Read)?;
        }
        Ok(())
    }

    pub(super) async fn decoder(
        &self,
        manifest: &Manifest,
        expected: Option<WorkspaceId>,
    ) -> Result<Option<FileDecoder>> {
        let Some(sealed) = manifest.sealed() else {
            return Ok(None);
        };
        let source = match (&self.file_keys, &self.scoped_file_keys, expected) {
            (Some(source), _, _) => source.clone(),
            (None, Some(scopes), Some(workspace)) => scopes.source(workspace, Access::Read)?,
            _ => return Err(anyhow!("this mount has no key authority for that file")),
        };
        let metadata = source.metadata(sealed);
        let keys = match run_keys(source.workspace) {
            Ok(Some(keys)) if keys.holds_key_for(&metadata) => keys,
            held => {
                let keys = source.access().await?;
                if held.is_ok() {
                    keep_in_run(&source, &keys);
                }
                keys
            }
        };
        ensure!(
            keys.holds_key_for(&metadata),
            "this workspace holds no key for that file"
        );
        Ok(Some(FileDecoder {
            keys,
            metadata,
            artifact: source.artifact(),
        }))
    }

    fn file_writer(&self, workspace: Option<WorkspaceId>) -> Result<Option<FileKeys>> {
        match (&self.file_keys, &self.scoped_file_keys, workspace) {
            (Some(source), _, _) => Ok(Some(source.clone())),
            (None, Some(scopes), Some(workspace)) => {
                scopes.source(workspace, Access::Write).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(crate) async fn write_file_content(
        &self,
        data: &[u8],
        previous: Option<Hash>,
        workspace: Option<WorkspaceId>,
    ) -> Result<Option<Hash>> {
        let Some(source) = self.file_writer(workspace)? else {
            return self.store.write_content_after(data, previous).await;
        };
        if data.is_empty() {
            return Ok(None);
        }
        let tier = self.previous_tier(previous).await;
        let (manifest, _) = content::build(data, tier);
        source.save(&self.store, &manifest, data).await.map(Some)
    }

    pub(crate) async fn publish_file_content(
        &self,
        path: &Path,
        known: Option<Arc<Manifest>>,
        previous: Option<Hash>,
        workspace: Option<WorkspaceId>,
        adopt: bool,
    ) -> Result<(Option<(Hash, u64)>, bool)> {
        let Some(source) = self.file_writer(workspace)? else {
            return self.store.publish_file(path, known, previous, adopt).await;
        };
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok((None, false));
        }
        let manifest = match known {
            Some(manifest) if manifest.total_len() == len && manifest.sealed().is_none() => {
                manifest
            }
            _ => {
                let tier = self.previous_tier(previous).await;
                Arc::new(
                    tokio::task::spawn_blocking(move || {
                        content::build_reader(BufReader::with_capacity(1 << 20, file), len, tier)
                            .map(|(manifest, _)| manifest)
                    })
                    .await??,
                )
            }
        };
        let file = tokio::fs::File::open(path).await?;
        let hash = source.save(&self.store, &manifest, file).await?;
        Ok((Some((hash, len)), false))
    }

    /// Whether this mount's content for `workspace` is stored plainly, so a staged file can
    /// become its entry on the thread that closed it; a workspace with its own keys seals it.
    pub(crate) fn stores_plainly(&self, workspace: Option<WorkspaceId>) -> bool {
        matches!(self.file_writer(workspace), Ok(None))
    }

    /// [`ContentStore::adopt_staged_inline`] for this mount's plain content.
    pub(crate) fn adopt_staged_inline(
        &self,
        path: &Path,
    ) -> Result<Option<crate::store::Published>> {
        self.store.adopt_staged_inline(path)
    }

    async fn previous_tier(&self, previous: Option<Hash>) -> Option<content::ChunkTier> {
        Some(self.store.manifest(previous?).await?.tier())
    }
}

async fn write_sealed_chunks(
    store: &crate::store::ContentStore,
    keys: &ContentKeys,
    workspace: WorkspaceId,
    plaintext: &Manifest,
    mut read: impl tokio::io::AsyncRead + Unpin,
) -> Result<Hash> {
    let artifact = collab_types::file_content_artifact(workspace);
    let mut chunks = Vec::with_capacity(plaintext.chunks().len());
    let mut clear = Vec::new();
    let mut overhead = None;
    for chunk in plaintext.chunks() {
        clear.resize(chunk.len as usize, 0);
        read.read_exact(&mut clear).await?;
        ensure!(
            blake3::hash(&clear).as_bytes() == &chunk.hash,
            "file changed during sealing"
        );
        let (_, sealed) = keys.seal_content(artifact, ContentRetention::Delta, &clear)?;
        let extra = u32::try_from(sealed.len().saturating_sub(clear.len()))?;
        ensure!(
            overhead.get_or_insert(extra) == &extra,
            "inconsistent file chunk overhead"
        );
        chunks.push(ChunkRef {
            hash: *store.ingest(Bytes::from(sealed)).await?.as_bytes(),
            len: chunk.len,
        });
    }
    ensure!(
        read.read_u8()
            .await
            .is_err_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof),
        "file changed during sealing"
    );
    let manifest = Manifest::new_sealed(
        plaintext.tier(),
        chunks,
        SealedChunks {
            group: *keys.workspace().as_uuid().as_bytes(),
            epoch: keys.epoch(),
            overhead: overhead.ok_or_else(|| anyhow!("empty file manifest"))?,
        },
    );
    store.ingest(Bytes::from(manifest.encode())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    async fn native_range(
        hydration: Arc<Hydrator>,
        hash: Hash,
        workspace: WorkspaceId,
        len: u64,
    ) -> std::io::Result<Vec<u8>> {
        use std::os::fd::AsFd as _;

        use crate::native::fetch::ContentFetcher;
        use fs_native::hydrate::{Cancel, Fetcher as _};

        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let dir = tempfile::tempdir()?;
            let relative = Path::new(".workspaces").join(workspace.to_string());
            let parent = dir.path().join(relative);
            std::fs::create_dir_all(&parent)?;
            let path = parent.join("placeholder");
            let file = std::fs::File::create(&path)?;
            let fetch = ContentFetcher::new(hydration, runtime, 1)
                .with_tree(dir.path().to_path_buf(), dir.path().join("mount"));
            fetch.fill(
                hash.as_bytes(),
                0..len,
                file.as_fd(),
                false,
                &Cancel::default(),
            )?;
            std::fs::read(path)
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_mounts_open_file_content_only_inside_its_workspace() {
        verify_scoped_file_access(false).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_mounted_file_refuses_a_read_after_workspace_access_is_revoked() {
        tilo_test_env::require_capability!(
            Fuse,
            crate::fuse_session::mountable(
                Path::new("/dev/fuse"),
                std::env::var_os("PATH").as_deref()
            ),
            "requires /dev/fuse and fusermount3"
        );
        verify_scoped_file_access(true).await;
    }

    fn scoped_file_acl(
        reader: [u8; 32],
        workspace: WorkspaceId,
        another: WorkspaceId,
        allowed: bool,
    ) -> crate::acl::WorkspaceAcl {
        use collab_types::{CapOverride, Effect, Scope};
        use tilo_action_policy::Grantee;

        let capabilities: Vec<_> = [workspace, another]
            .into_iter()
            .flat_map(|id| {
                let scope = Scope::FsPath(format!("/var/workspace/.workspaces/{id}"));
                let who = Grantee::person("local");
                let mut caps = vec![CapOverride::new(
                    who.clone(),
                    scope.clone(),
                    Effect::WorkspaceFileBoundary,
                    collab_types::Access::Read,
                )];
                if id != workspace || allowed {
                    caps.push(CapOverride::new(
                        who,
                        scope,
                        Effect::WorkspaceFileAccess,
                        collab_types::Access::Write,
                    ));
                }
                caps
            })
            .collect();
        crate::acl::WorkspaceAcl::from_capabilities(
            reader,
            &capabilities,
            &[collab_types::MemberId::person("local")],
            None,
        )
    }

    fn group_keys() -> ContentKeys {
        key_group::KeyGroup::create(
            WorkspaceId::new(),
            key_group::MemberKey::generate("member").unwrap(),
        )
        .unwrap()
        .content_keys()
        .unwrap()
    }

    /// A mount's writers each close a file at once: their key reads go over a connection each
    /// rather than queueing on one, and the connections stay open for the next, at most
    /// [`IDLE_KEY_LINKS`] of them. A connection whose request failed is not used again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 16)]
    async fn files_saved_at_once_read_their_keys_side_by_side() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Default)]
        struct Service {
            opened: AtomicUsize,
            reading: AtomicUsize,
            most_at_once: AtomicUsize,
            reads: AtomicUsize,
        }
        let service = Arc::new(Service::default());
        let keys = group_keys().to_bytes().unwrap().to_vec();
        let (opens, reads) = (Arc::clone(&service), Arc::clone(&service));
        let source = FileKeys::pooled(
            WorkspaceId::new(),
            move || Ok(opens.opened.fetch_add(1, Ordering::SeqCst)),
            move |connection: &mut usize| {
                let at_once = reads.reading.fetch_add(1, Ordering::SeqCst) + 1;
                reads.most_at_once.fetch_max(at_once, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(5));
                reads.reading.fetch_sub(1, Ordering::SeqCst);
                let read = reads.reads.fetch_add(1, Ordering::SeqCst);
                // The first connection breaks on its second request.
                ensure!(
                    !(*connection == 0 && read > 0 && read < 20),
                    "the connection broke"
                );
                Ok(Ok(keys.clone()))
            },
        );
        let store = Arc::new(crate::store::ContentStore::memory());
        let started = std::time::Instant::now();
        let writers: Vec<_> = (0..16_u8)
            .map(|writer| {
                let (source, store) = (source.clone(), Arc::clone(&store));
                tokio::spawn(async move {
                    let mut saved = 0;
                    for file in 0..32_u8 {
                        let data = vec![writer.wrapping_mul(32).wrapping_add(file); 4096];
                        let (manifest, _) = content::build(&data, None);
                        saved += usize::from(
                            source
                                .save(&store, &manifest, data.as_slice())
                                .await
                                .is_ok(),
                        );
                    }
                    saved
                })
            })
            .collect();
        let mut saved = 0;
        for writer in writers {
            saved += writer.await.unwrap();
        }
        let took = started.elapsed();
        println!(
            "BENCH file_keys sealed_saves_s {:.0} files/s over {} key reads",
            512.0 / took.as_secs_f64(),
            service.reads.load(Ordering::SeqCst)
        );
        assert!(
            saved >= 510,
            "{saved} of 512 files saved: only the broken connection's fail"
        );
        let reads = service.reads.load(Ordering::SeqCst);
        assert!(
            reads <= 512 + 16 + 20,
            "{reads} key reads for 512 files: one each, beside each writer's first and the broken connection's"
        );
        let most = service.most_at_once.load(Ordering::SeqCst);
        assert!(most > 1, "key reads went one at a time ({most} at most)");
        let opened = service.opened.load(Ordering::SeqCst);
        assert!(
            opened <= 16 + 1,
            "{opened} connections opened for 16 writers and one broken connection"
        );
    }

    /// A save seals with the keys the save before it read, and its own read after sealing finds
    /// whether they are still the workspace's: a file sealed under keys that moved since is
    /// refused, and the next one seals under the keys that took their place.
    #[tokio::test]
    async fn a_file_sealed_under_keys_that_moved_is_refused() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (old, new) = (
            group_keys().to_bytes().unwrap().to_vec(),
            group_keys().to_bytes().unwrap().to_vec(),
        );
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        // The keys move after the first save read them twice.
        let source = FileKeys::from_session(WorkspaceId::new(), move || {
            let read = counted.fetch_add(1, Ordering::SeqCst);
            Ok(ContentKeys::from_bytes(if read < 2 { &old } else { &new })?)
        });
        let store = crate::store::ContentStore::memory();
        let save = |file: u8| {
            let data = vec![file; 4096];
            let (manifest, _) = content::build(&data, None);
            let source = source.clone();
            let store = &store;
            async move { source.save(store, &manifest, data.as_slice()).await }
        };
        save(1).await.unwrap();
        assert!(
            save(2).await.is_err(),
            "a file sealed under keys that had moved was kept"
        );
        save(3).await.unwrap();
        save(4).await.unwrap();
        assert_eq!(
            reads.load(Ordering::SeqCst),
            5,
            "one key read per file after the first"
        );
    }

    fn install_file_keys(hydrator: &Hydrator, workspaces: [WorkspaceId; 2]) {
        let source = hydrator.scoped_file_keys.as_ref().unwrap();
        for id in workspaces {
            let mut group = *id.as_uuid().as_bytes();
            group[8] |= 0xe0;
            let keys = key_group::KeyGroup::create(
                WorkspaceId::from_u128(u128::from_be_bytes(group)),
                key_group::MemberKey::generate("member").unwrap(),
            )
            .unwrap()
            .content_keys()
            .unwrap();
            source
                .sources
                .lock_recover()
                .insert(id, FileKeys::from_session(id, move || Ok(keys.clone())));
        }
    }

    async fn verify_scoped_file_access(kernel_mount: bool) {
        let reader = [19; 32];
        let workspace = WorkspaceId::new();
        let another = WorkspaceId::new();
        let acl = |allowed| scoped_file_acl(reader, workspace, another, allowed);
        let policy = AclHandle::new(acl(true));
        let store = Arc::new(crate::store::ContentStore::memory());
        let hydrator = Hydrator::new(store.clone()).with_scoped_file_keys(
            PathBuf::new(),
            policy.clone(),
            reader,
        );
        install_file_keys(&hydrator, [workspace, another]);

        let data = b"private workspace file";
        let len = data.len() as u64;
        let namespace = Arc::new(Mutex::new(crate::Namespace::new("scoped-mount").unwrap()));
        let tree = {
            let mut ns = namespace.lock_recover();
            let scopes = ns.create_dir(crate::ROOT_INO, ".workspaces", 1).unwrap();
            ns.create_dir(scopes, workspace.to_string(), 1).unwrap()
        };
        let hydrator = Arc::new(hydrator);
        let fs = crate::QuickFS::new(
            namespace.clone(),
            hydrator.clone(),
            Arc::new(crate::Heuristics::new(hydrator.clone())),
        )
        .with_acl(policy.clone(), reader);
        let path = Path::new(".workspaces")
            .join(workspace.to_string())
            .join("private.txt");
        fs.apply_local_file(&path, data, 2).unwrap();
        let hash = {
            let mut ns = namespace.lock_recover();
            let ino = ns.lookup(tree, "private.txt").unwrap().ino.0;
            ns.hash_for(ino).unwrap()
        };
        let kept = store.manifest(hash).await.unwrap();
        assert!(kept.sealed().is_some(), "the mount persisted ciphertext");
        assert_eq!(fs.read_path(&path).unwrap(), data);
        assert_eq!(
            hydrator
                .read_scoped(hash, 0, len, Some(workspace))
                .await
                .unwrap(),
            data
        );
        assert!(
            hydrator
                .read_scoped(hash, 0, len, Some(another))
                .await
                .is_err()
        );
        assert!(hydrator.read(hash, 0, len).await.is_err());

        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                native_range(hydrator.clone(), hash, workspace, len)
                    .await
                    .unwrap(),
                data
            );
            assert!(
                native_range(hydrator.clone(), hash, another, len)
                    .await
                    .is_err()
            );
        }

        if kernel_mount {
            #[cfg(target_os = "linux")]
            check_open_handle(fs, path, data, policy, acl(false)).await;
            return;
        }

        assert_staged_workspace_content_is_sealed(&hydrator, workspace).await;
        policy.replace(acl(false));
        assert!(fs.read_path(&path).is_err());
        #[cfg(target_os = "linux")]
        assert!(
            native_range(hydrator.clone(), hash, workspace, len)
                .await
                .is_err()
        );
        assert!(
            hydrator
                .read_scoped(hash, 0, len, Some(workspace))
                .await
                .is_err()
        );
        assert!(
            hydrator
                .write_file_content(data, Some(hash), Some(workspace))
                .await
                .is_err()
        );
    }

    async fn assert_staged_workspace_content_is_sealed(
        hydrator: &Hydrator,
        workspace: WorkspaceId,
    ) {
        let stage = tempfile::NamedTempFile::new().unwrap();
        let clear = b"staged workspace bytes";
        std::fs::write(stage.path(), clear).unwrap();
        let (stored, adopted) = hydrator
            .publish_file_content(stage.path(), None, None, Some(workspace), true)
            .await
            .unwrap();
        let (hash, len) = stored.unwrap();
        assert!(
            !adopted,
            "the content store must not adopt the plaintext stage"
        );
        assert!(
            hydrator
                .store
                .manifest(hash)
                .await
                .unwrap()
                .sealed()
                .is_some()
        );
        assert_eq!(
            hydrator
                .read_scoped(hash, 0, len, Some(workspace))
                .await
                .unwrap(),
            clear
        );
    }

    #[cfg(target_os = "linux")]
    async fn check_open_handle(
        fs: crate::QuickFS,
        path: PathBuf,
        contents: &'static [u8],
        acl: AclHandle,
        denied: crate::acl::WorkspaceAcl,
    ) {
        use std::io::{Read as _, Seek as _};

        tokio::task::spawn_blocking(move || {
            let mountpoint = tempfile::tempdir().unwrap();
            let session = fuser::spawn_mount(fs, mountpoint.path(), &fuser::Config::default())
                .expect("mount the scoped Files namespace");
            let path = mountpoint.path().join(path);
            let result = (|| -> std::io::Result<(Vec<u8>, bool, bool)> {
                let mut opened = std::fs::File::open(&path)?;
                let mut clear = vec![0; contents.len()];
                opened.read_exact(&mut clear)?;

                acl.replace(denied);
                opened.rewind()?;
                let existing_read_denied = opened.read(&mut [0_u8; 1]).is_err();
                let new_read_denied = std::fs::read(path).is_err();
                Ok((clear, existing_read_denied, new_read_denied))
            })();
            crate::fuse_session::unmount_and_join(session, mountpoint.path()).unwrap();
            let (read, existing_read_denied, new_read_denied) = result.unwrap();
            assert_eq!(read, contents);
            assert!(
                existing_read_denied,
                "an already-open descriptor must stop reading"
            );
            assert!(new_read_denied, "opening the file again must stop reading");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn files_sealed_for_a_document_do_not_open_with_the_workspace_key() {
        let workspace = WorkspaceId::new();
        let mut group_id = *workspace.as_uuid().as_bytes();
        group_id[8] |= 0xe0;
        let group = WorkspaceId::from_u128(u128::from_be_bytes(group_id));
        let keys_for = |id| {
            key_group::KeyGroup::create(id, key_group::MemberKey::generate("member").unwrap())
                .unwrap()
                .content_keys()
                .unwrap()
        };
        let keys = keys_for(group);
        let other = keys_for(workspace);
        let bytes: Vec<u8> = (0..520_000_u32)
            .map(|value| (value % 251).to_le_bytes()[0])
            .collect();
        let (plain, _) = content::build(&bytes, None);
        let store = crate::store::ContentStore::memory();
        let artifact = collab_types::file_content_artifact(workspace);
        let hash = write_sealed_chunks(&store, &keys, workspace, &plain, bytes.as_slice())
            .await
            .unwrap();
        let manifest = store.manifest(hash).await.unwrap();
        let sealed = manifest.sealed().unwrap();
        assert_eq!(sealed.group, group_id);
        let metadata = EncryptionMetadata::content_group(
            workspace,
            group,
            sealed.epoch,
            ContentRetention::Delta,
        );
        let decoder = FileDecoder {
            keys,
            metadata,
            artifact,
        };
        let mut restored = Vec::new();
        assert!(manifest.chunks().len() > 1);
        for (plain_part, sealed_part) in plain.chunks().iter().zip(manifest.chunks()) {
            assert!(!store.has(Hash::from(plain_part.hash)).await);
            let ciphertext = store.local(Hash::from(sealed_part.hash)).await.unwrap();
            assert_eq!(ciphertext.len() as u64, manifest.stored_len(sealed_part));
            assert!(
                other
                    .open_content(artifact, &metadata, &ciphertext)
                    .is_err()
            );
            restored.extend_from_slice(&decoder.open(&ciphertext, sealed_part.len).unwrap());
        }
        assert_eq!(restored, bytes);
        assert!(store.read_local(hash, &manifest, 0..16).await.is_none());
    }
}
