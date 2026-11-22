//! The key holder performing Files actions seals and opens the same file
//! versions that members write from the browser or the filesystem.

use collab_types::{ContentRetention, EncryptionMetadata, WorkspaceId, file_content_artifact};
use fs_content::{ChunkRef, Manifest, SealedChunks, Streamed};
use key_group::ContentKeys;
use tilo_content::ContentStore;
use tilo_session_client::engine::SealKeys;

use crate::workspace_actions::{Worker, bounded};

pub(in crate::workspace_actions) struct FileSealing {
    workspace: WorkspaceId,
    keys: ContentKeys,
}

impl Worker {
    pub(in crate::workspace_actions) fn file_sealing(
        &self,
        workspace: WorkspaceId,
        create: bool,
    ) -> Result<Option<FileSealing>, String> {
        let route = Self::tree_chunk_route(&workspace.to_string(), 0);
        let from_serving_device = self
            .shared
            .link
            .get()
            .and_then(|link| link.serving_keys(&route, create));
        let keys = match from_serving_device {
            Some(answer) => answer?,
            None if !bounded::governed(&self.shared, workspace) => return Ok(None),
            None => {
                let engine = self
                    .workspaces
                    .get(&workspace)
                    .ok_or("the Files session is not open")?
                    .peer
                    .engine();
                let SealKeys::Here(keys) = engine.document_seal_keys(&route)? else {
                    return Err("the workspace's Files keys are still arriving".into());
                };
                keys
            }
        };
        Ok(Some(FileSealing { workspace, keys }))
    }
}

impl Worker {
    pub(super) fn check_file_keys(
        &self,
        workspace: WorkspaceId,
        used: Option<&FileSealing>,
    ) -> Result<(), String> {
        let generation = |sealing: &FileSealing| (sealing.keys.workspace(), sealing.keys.epoch());
        // The same ask the put sealed with: a put that founded its record's group asks after
        // it as it asked before, rather than as a reader that founds nothing.
        let current = self.file_sealing(workspace, true)?;
        if used.map(generation) != current.as_ref().map(generation) {
            return Err("file access changed while it was saved".into());
        }
        Ok(())
    }
}

impl FileSealing {
    pub(in crate::workspace_actions) fn open(
        &self,
        sealed: SealedChunks,
        bytes: &[u8],
    ) -> Option<Vec<u8>> {
        let group = WorkspaceId::from_u128(u128::from_be_bytes(sealed.group));
        let metadata = EncryptionMetadata::content_group(
            self.workspace,
            group,
            sealed.epoch,
            ContentRetention::Delta,
        );
        self.keys
            .open_content(file_content_artifact(self.workspace), &metadata, bytes)
            .ok()
            .map(|clear| clear.to_vec())
    }

    pub(super) fn manifest(
        &self,
        store: &ContentStore,
        mut reader: impl std::io::Read,
        size: u64,
    ) -> Result<Vec<u8>, String> {
        let mut failed = None;
        let mut chunks = Vec::new();
        let mut overhead = None;
        let mut keep = |part: &ChunkRef, bytes: &[u8]| {
            if failed.is_some() {
                return;
            }
            match self.store_chunk(store, bytes, part.len, &mut overhead) {
                Ok(chunk) => chunks.push(chunk),
                Err(error) => failed = Some(error),
            }
        };
        let mut stream = Streamed::new(size, None);
        let mut buffer = vec![0; super::PIECE];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.to_string()),
            };
            stream
                .push(buffer.get(..count).unwrap_or_default(), &mut keep)
                .map_err(|error| error.to_string())?;
        }
        let (manifest, _) = stream
            .finish(&mut keep)
            .map_err(|error| error.to_string())?;
        if let Some(error) = failed {
            return Err(error);
        }
        Ok(self.finish_manifest(&manifest, chunks, overhead))
    }

    /// Preserve the original file's chunking while moving it to the current key.
    pub(super) fn reseal(
        &self,
        store: &ContentStore,
        manifest: &Manifest,
    ) -> Result<Vec<u8>, String> {
        let mut overhead = None;
        let mut chunks = Vec::with_capacity(manifest.chunks().len());
        for part in manifest.chunks() {
            let stored = store
                .read(&hex::encode(part.hash))
                .ok_or("a file chunk is missing")?;
            if stored.len() as u64 != manifest.stored_len(part) {
                return Err("unexpected stored file chunk length".into());
            }
            let clear = match manifest.sealed() {
                Some(sealed) => self
                    .open(sealed, &stored)
                    .ok_or("this file's earlier keys are unavailable")?,
                None => stored,
            };
            chunks.push(self.store_chunk(store, &clear, part.len, &mut overhead)?);
        }
        Ok(self.finish_manifest(manifest, chunks, overhead))
    }

    fn store_chunk(
        &self,
        store: &ContentStore,
        clear: &[u8],
        len: u32,
        overhead: &mut Option<u32>,
    ) -> Result<ChunkRef, String> {
        if clear.len() as u64 != u64::from(len) {
            return Err("unexpected file chunk length".into());
        }
        let (_, cipher) = self
            .keys
            .seal_content(
                file_content_artifact(self.workspace),
                ContentRetention::Delta,
                clear,
            )
            .map_err(|error| error.to_string())?;
        let extra = u32::try_from(cipher.len().saturating_sub(clear.len()))
            .map_err(|error| error.to_string())?;
        if *overhead.get_or_insert(extra) != extra {
            return Err("inconsistent sealed file chunk length".into());
        }
        let address = store
            .put_public(&cipher)
            .map_err(|error| error.to_string())?;
        let mut hash = [0; 32];
        hex::decode_to_slice(address, &mut hash).map_err(|error| error.to_string())?;
        Ok(ChunkRef { hash, len })
    }

    fn finish_manifest(
        &self,
        manifest: &Manifest,
        chunks: Vec<ChunkRef>,
        overhead: Option<u32>,
    ) -> Vec<u8> {
        let Some(overhead) = overhead else {
            return manifest.encode();
        };
        Manifest::new_sealed(
            manifest.tier(),
            chunks,
            SealedChunks {
                group: *self.keys.workspace().as_uuid().as_bytes(),
                epoch: self.keys.epoch(),
                overhead,
            },
        )
        .encode()
    }
}
