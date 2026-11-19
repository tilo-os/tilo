//! Sealing file content with the same access as the workspace's Files tree.

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
use tilo_identity_client::{BrokerSigningIdentity, wire::KeyAccess};
use tilo_lock::MutexRecover as _;
use tokio::io::AsyncReadExt as _;

use crate::acl::{Access, AclHandle, IdentityKey};
use crate::content::{self, Manifest};
use crate::tree_document::{FILES_APP, connect_identity, document_id};

use super::Hydrator;

type ReadKeys = dyn Fn() -> Result<ContentKeys> + Send + Sync;

/// The key authority for the Files endpoint's workspace.
#[derive(Clone)]
pub struct FileKeys {
    workspace: WorkspaceId,
    read: Arc<ReadKeys>,
}

impl FileKeys {
    #[must_use]
    pub fn new(workspace: WorkspaceId, identity: PathBuf) -> Self {
        let broker = Mutex::new(None::<BrokerSigningIdentity>);
        let scope = workspace.to_string();
        Self::from_session(workspace, move || {
            let mut connected = broker.lock_recover();
            if connected.is_none() {
                *connected = Some(connect_identity(&identity, &scope)?);
            }
            let connection = connected
                .as_mut()
                .ok_or_else(|| anyhow!("no Files identity connection"))?;
            let reply = match connection.group_keys(&scope, FILES_APP, &document_id(), 0) {
                Ok(reply) => reply,
                Err(error) => {
                    *connected = None;
                    return Err(anyhow!("could not read the Files keys: {error}"));
                }
            };
            ensure!(
                reply.access == KeyAccess::Ready,
                "the Files content keys are not ready"
            );
            ContentKeys::from_bytes(
                reply
                    .keys
                    .as_deref()
                    .ok_or_else(|| anyhow!("no Files keys returned"))?,
            )
            .context("decoding Files content keys")
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
        }
    }

    async fn access(&self) -> Result<ContentKeys> {
        let read = Arc::clone(&self.read);
        tokio::task::spawn_blocking(move || read()).await?
    }

    async fn save(
        &self,
        store: &crate::store::ContentStore,
        manifest: &Manifest,
        source: impl tokio::io::AsyncRead + Unpin,
    ) -> Result<Hash> {
        let keys = self.access().await?;
        let hash = write_sealed_chunks(store, &keys, self.workspace, manifest, source).await?;
        let current = self.access().await?;
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

/// The managed mount shares one namespace between the person's folders and signed workspace roots.
pub(super) struct ScopedFileKeys {
    identity: PathBuf,
    policy: AclHandle,
    reader: IdentityKey,
    sources: Mutex<BTreeMap<WorkspaceId, FileKeys>>,
}

impl ScopedFileKeys {
    fn source(&self, workspace: WorkspaceId, access: Access) -> Result<FileKeys> {
        let root = tilo_paths::workspace_tree(Path::new("/"), &workspace.to_string())
            .ok_or_else(|| anyhow!("invalid workspace folder"))?;
        ensure!(
            self.policy
                .permits_workspace_tree(&self.reader, &root.to_string_lossy(), access),
            "workspace Files access was refused"
        );
        Ok(self
            .sources
            .lock_recover()
            .entry(workspace)
            .or_insert_with(|| FileKeys::new(workspace, self.identity.clone()))
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
    /// Use the workspace key for each governed folder of the managed Files mount.
    #[must_use]
    pub fn with_scoped_file_keys(
        mut self,
        identity: PathBuf,
        policy: AclHandle,
        reader: IdentityKey,
    ) -> Self {
        self.scoped_file_keys = Some(ScopedFileKeys {
            identity,
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
        let keys = source.access().await?;
        let metadata = source.metadata(sealed);
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
    ) -> Result<Option<(Hash, u64)>> {
        let Some(source) = self.file_writer(workspace)? else {
            return self.store.publish_file(path, known, previous).await;
        };
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(None);
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
        Ok(Some((hash, len)))
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_mounts_open_file_content_only_inside_its_workspace() {
        use crate::acl::WorkspaceAcl;
        use collab_types::{CapOverride, Effect, Scope};
        use tilo_action_policy::Grantee;

        let reader = [19; 32];
        let member = collab_types::MemberId::person("local");
        let workspace = WorkspaceId::new();
        let another = WorkspaceId::new();
        let capabilities = |allowed| {
            [workspace, another]
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
                .collect::<Vec<_>>()
        };
        let acl = |allowed| {
            WorkspaceAcl::from_capabilities(
                reader,
                &capabilities(allowed),
                std::slice::from_ref(&member),
                None,
            )
        };
        let policy = AclHandle::new(acl(true));
        let store = Arc::new(crate::store::ContentStore::memory());
        let hydrator = Hydrator::new(super::super::tests::local_endpoint().await, store.clone())
            .with_scoped_file_keys(PathBuf::new(), policy.clone(), reader);
        let source = hydrator.scoped_file_keys.as_ref().unwrap();
        for id in [workspace, another] {
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

        policy.replace(acl(false));
        assert!(fs.read_path(&path).is_err());
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
