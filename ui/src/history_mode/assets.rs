//! Historical asset delivery remains outside the presentation-only renderer.
use super::{FrozenPreviewAssets, HistoryResource, HistorySnapshot, PreviewSide};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, mpsc},
};

/// A source reads only this immutable selected version when resolving media.
#[derive(Clone)]
pub struct HistoricalAssetRequest {
    pub resource: HistoryResource,
    pub snapshot: HistorySnapshot,
}
/// Host-registered resolver. It may complete immediately or on a worker; the
/// same captured response key is checked on the owner thread in both cases.
/// The trusted domain resolver derives content-hash references from the supplied
/// immutable snapshot, not a live record or current media path. The framework
/// checks response custody and byte integrity; it cannot infer a domain's media
/// schema. A missing recorded hash must remain unavailable.
pub trait HistoricalAssetSource {
    fn request(&self, request: HistoricalAssetRequest, response: HistoricalAssetResponse);
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AssetResponseKey {
    pub(crate) host: uuid::Uuid,
    pub(crate) resource: HistoryResource,
    pub(crate) generation: u64,
    pub(crate) side: PreviewSide,
    pub(crate) heads: Vec<automerge::ChangeHash>,
}
/// A single request-bound completion capability, never a live document handle.
/// The provider cannot relabel an old result with another request's identity.
pub struct HistoricalAssetResponse {
    key: AssetResponseKey,
    sender: mpsc::Sender<(AssetResponseKey, Result<FrozenPreviewAssets, String>)>,
    wake: crate::wake::WakeHandle,
}
impl HistoricalAssetResponse {
    pub(crate) fn new(
        key: AssetResponseKey,
        sender: mpsc::Sender<(AssetResponseKey, Result<FrozenPreviewAssets, String>)>,
    ) -> Self {
        Self {
            key,
            sender,
            wake: crate::wake::WakeHandle::new(),
        }
    }
    /// Keys must be SHA-256 content digests (bare hex or `sha256:`). Bytes are
    /// admitted only after digest and total-size verification. Missing keys
    /// remain missing; no current-media fallback is available to a renderer.
    pub fn complete(self, assets: Result<BTreeMap<String, Arc<[u8]>>, String>) {
        let admitted = assets.and_then(|assets| {
            if assets.len() > 4096 {
                return Err("Historical media exceeds the 4096 asset preview limit".into());
            }
            let mut total = 0usize;
            for (hash, bytes) in &assets {
                total = total
                    .checked_add(bytes.len())
                    .ok_or("Historical media is too large")?;
                if total > 128 * 1024 * 1024 {
                    return Err("Historical media exceeds the 128 MiB preview limit".into());
                }
                let digest = format!("{:x}", Sha256::digest(bytes));
                if hash.strip_prefix("sha256:").unwrap_or(hash) != digest {
                    return Err("Historical media does not match its recorded content hash".into());
                }
            }
            Ok(FrozenPreviewAssets::new(assets))
        });
        if self.sender.send((self.key, admitted)).is_ok() {
            self.wake.notify_events();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_verifies_content_hash_and_keeps_original_request_key() {
        let key = AssetResponseKey {
            host: uuid::Uuid::new_v4(),
            resource: HistoryResource::new("profile", "room", "app", crate::RecordId::for_tab(3)),
            generation: 4,
            side: PreviewSide::Before,
            heads: vec![],
        };
        let bytes: Arc<[u8]> = Arc::from(b"recorded bytes".as_slice());
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let (sender, receiver) = mpsc::channel();
        HistoricalAssetResponse::new(key.clone(), sender.clone())
            .complete(Ok(BTreeMap::from([(hash.clone(), bytes.clone())])));
        let (received, result) = receiver.recv().unwrap();
        assert_eq!(received, key);
        assert_eq!(result.unwrap().get(&hash), Some(bytes.as_ref()));
        HistoricalAssetResponse::new(key.clone(), sender).complete(Ok(BTreeMap::from([(
            hash,
            Arc::from(b"current replacement".as_slice()),
        )])));
        let (received, result) = receiver.recv().unwrap();
        assert_eq!(received, key);
        assert!(result.unwrap_err().contains("recorded content hash"));
    }
}
