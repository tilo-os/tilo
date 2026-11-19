//! Sealing file content with the same access as the workspace's Files tree.

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
        let hash = write_sealed_chunks(store, &keys, self.artifact(), manifest, source).await?;
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
    pub(super) async fn decoder(&self, manifest: &Manifest) -> Result<Option<FileDecoder>> {
        let Some(sealed) = manifest.sealed() else {
            return Ok(None);
        };
        let source = self
            .file_keys
            .as_ref()
            .ok_or_else(|| anyhow!("this mount has no Files key authority"))?;
        Ok(Some(FileDecoder {
            keys: source.access().await?,
            metadata: source.metadata(sealed),
            artifact: source.artifact(),
        }))
    }

    pub(crate) async fn write_file_content(
        &self,
        data: &[u8],
        previous: Option<Hash>,
    ) -> Result<Option<Hash>> {
        let Some(source) = &self.file_keys else {
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
    ) -> Result<Option<(Hash, u64)>> {
        let Some(source) = &self.file_keys else {
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
    artifact: ArtifactId,
    plaintext: &Manifest,
    mut read: impl tokio::io::AsyncRead + Unpin,
) -> Result<Hash> {
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
        let hash = write_sealed_chunks(&store, &keys, artifact, &plain, bytes.as_slice())
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
