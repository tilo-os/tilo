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
    /// Keys are SHA-256 (bare hex or `sha256:`), or explicit `asset:blake3:`
    /// content identities. Bare hashes always retain SHA-256 semantics. Bytes are
    /// admitted only after digest and total-size verification. Missing keys
    /// remain missing; no current-media fallback is available to a renderer.
    pub fn complete(self, assets: Result<BTreeMap<String, Arc<[u8]>>, String>) {
        let admitted = assets.and_then(|assets| {
            if assets.len() > 4096 {
                return Err(crate::i18n::I18nManager::lookup_plain(
                    "history-media-too-many",
                ));
            }
            let mut total = 0usize;
            for (hash, bytes) in &assets {
                total = total.checked_add(bytes.len()).ok_or_else(|| {
                    crate::i18n::I18nManager::lookup_plain("history-media-too-large")
                })?;
                if total > 128 * 1024 * 1024 {
                    return Err(crate::i18n::I18nManager::lookup_plain(
                        "history-media-over-limit",
                    ));
                }
                let (recorded, digest) = if let Some(hex) = hash.strip_prefix("asset:blake3:") {
                    (hex, blake3::hash(bytes).to_hex().to_string())
                } else {
                    (
                        hash.strip_prefix("sha256:").unwrap_or(hash),
                        hex::encode(Sha256::digest(bytes)),
                    )
                };
                if recorded.len() != 64
                    || !recorded.bytes().all(|b| b.is_ascii_hexdigit())
                    || recorded != digest
                {
                    return Err(crate::i18n::I18nManager::lookup_plain(
                        "history-media-hash-mismatch",
                    ));
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
    fn asset_admission_and_responses_keep_the_recorded_key() {
        // phase: explicit_blake3_admission_preserves_recorded_key
        {
            let key = AssetResponseKey {
                host: uuid::Uuid::new_v4(),
                resource: HistoryResource::new("p", "r", "app", crate::RecordId::for_tab(3)),
                generation: 4,
                side: PreviewSide::After,
                heads: vec![],
            };
            let (sender, receiver) = mpsc::channel();
            let hash =
                "asset:blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
            HistoricalAssetResponse::new(key.clone(), sender)
                .complete(Ok(BTreeMap::from([(hash.into(), Arc::from([]))])));
            let (received, result) = receiver.recv().unwrap();
            assert_eq!(received, key);
            assert_eq!(result.unwrap().get(hash), Some([].as_slice()));
        }
        // phase: response_verifies_content_hash_and_keeps_original_request_key
        {
            let key = AssetResponseKey {
                host: uuid::Uuid::new_v4(),
                resource: HistoryResource::new(
                    "profile",
                    "room",
                    "app",
                    crate::RecordId::for_tab(3),
                ),
                generation: 4,
                side: PreviewSide::Before,
                heads: vec![],
            };
            let bytes: Arc<[u8]> = Arc::from(b"recorded bytes".as_slice());
            let hash = hex::encode(Sha256::digest(&bytes));
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
}

#[cfg(test)]
mod algorithm_regressions {
    use super::*;
    fn admit(assets: BTreeMap<String, Arc<[u8]>>) -> Result<FrozenPreviewAssets, String> {
        let key = AssetResponseKey {
            host: uuid::Uuid::new_v4(),
            resource: HistoryResource::new("p", "w", "a", crate::RecordId::for_tab(9)),
            generation: 7,
            side: PreviewSide::Before,
            heads: vec![],
        };
        let (tx, rx) = mpsc::channel();
        HistoricalAssetResponse::new(key.clone(), tx).complete(Ok(assets));
        let (received, result) = rx.recv().unwrap();
        assert_eq!(received, key);
        result
    }
    #[test]
    fn algorithm_semantics_and_limits_precede_delivery() {
        // phase: explicit_algorithms_wrong_bytes_malformed_unknown_and_bare_sha256_semantics
        {
            let bytes: Arc<[u8]> = Arc::from(b"historical image bytes".as_slice());
            let sha = hex::encode(Sha256::digest(&bytes));
            let blake = blake3::hash(&bytes).to_hex().to_string();
            for id in [
                sha.clone(),
                format!("sha256:{sha}"),
                format!("asset:blake3:{blake}"),
            ] {
                assert_eq!(
                    admit(BTreeMap::from([(id.clone(), bytes.clone())]))
                        .unwrap()
                        .get(&id),
                    Some(bytes.as_ref())
                );
                assert!(
                    admit(BTreeMap::from([(id, Arc::from(b"replacement".as_slice()))])).is_err()
                );
            }
            for id in [
                blake.clone(),
                format!("blake3:{blake}"),
                format!("asset:md5:{blake}"),
                format!("asset:blake3:{blake}0"),
                "asset:blake3:xyz".into(),
                format!("sha256:{blake}"),
            ] {
                assert!(admit(BTreeMap::from([(id, bytes.clone())])).is_err());
            }
        }
        // phase: count_and_aggregate_limits_precede_delivery
        {
            let too_many = (0..4097).map(|i| (i.to_string(), Arc::from([]))).collect();
            assert!(admit(too_many).unwrap_err().contains("4096"));
            let bytes: Arc<[u8]> = vec![0u8; 65 * 1024 * 1024].into();
            let sha = hex::encode(Sha256::digest(&bytes));
            let blake = format!("asset:blake3:{}", blake3::hash(&bytes).to_hex());
            assert!(
                admit(BTreeMap::from([(sha, bytes.clone()), (blake, bytes)]))
                    .unwrap_err()
                    .contains("128 MiB")
            );
        }
    }
}
