//! Reading a notebook's pictures from the content store.
//!
//! The notebook names each picture by its digest; the bytes of one that is
//! not inline live in the content store, which reads them from the disk. On
//! the compositor's frame loop, where a hosted notebook is drawn, that read
//! runs on a worker and its result is taken on a later sync; anywhere else it
//! runs at once. Only pictures the window does not hold yet are read.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ui::lock::MutexRecover as _;

/// Pictures read from the content store, by their `asset:` source. A digest
/// whose bytes are not here (or do not match it) is absent.
pub(super) type Read = HashMap<String, Vec<u8>>;

/// The read a window has outstanding, if any.
pub(super) struct AssetReads {
    pending: Option<Arc<Mutex<Option<Read>>>>,
    wake: ui::WakeHandle,
}

impl AssetReads {
    pub(super) fn new() -> Self {
        Self {
            pending: None,
            wake: ui::WakeHandle::new(),
        }
    }

    /// Wake the window through `wake` when a read finishes.
    pub(super) fn wake_through(&mut self, wake: ui::WakeHandle) {
        self.wake = wake;
    }

    /// Read the pictures `digests` name. Returns the result at once where
    /// this thread may wait on storage; otherwise starts a worker, replacing
    /// a read under way, and returns `None`.
    pub(super) fn start(&mut self, digests: Vec<String>) -> Option<Read> {
        if !ui::storage::waiting::forbidden() {
            self.pending = None;
            return Some(read(&digests));
        }
        let slot = Arc::new(Mutex::new(None));
        let done = Arc::clone(&slot);
        let wake = self.wake.clone();
        let spawned = ui::background::Background::current().spawn("notebook pictures", move || {
            *done.lock_recover() = Some(read(&digests));
            wake.wake();
        });
        match spawned {
            Ok(()) => {
                self.pending = Some(slot);
                None
            }
            Err(error) => {
                tracing::warn!(%error, "could not start reading the notebook's pictures");
                self.pending = None;
                Some(Read::new())
            }
        }
    }

    /// The finished read, if the one under way finished.
    pub(super) fn take(&mut self) -> Option<Read> {
        let read = self.pending.as_ref()?.lock_recover().take()?;
        self.pending = None;
        Some(read)
    }
}

impl super::NotesApp {
    /// Keep the image snapshot in step with the notebook's asset references,
    /// and pick up referenced bytes that arrive from a collaborator's device
    /// after the reference did. Their arrival syncs the window and moves the
    /// record content's arrival count; nothing looks for them on a timer.
    pub(super) fn refresh_canonical_assets(&mut self) {
        if let Some(read) = self.asset_reads.take() {
            self.take_asset_read(read);
        }
        let manifest = self.workspace.images.clone();
        let looked = ui::collaboration::record_content::arrivals();
        let retry = self
            .canonical_assets_waiting
            .is_some_and(|seen| seen != looked);
        if manifest == self.canonical_asset_manifest && !retry {
            return;
        }
        // Only the pictures the window does not hold yet are read. Bytes
        // this window put in itself (a paste) stay while the notebook still
        // names them, even where the store would not serve them back.
        let mut missing = Vec::new();
        if let Ok(mut assets) = self.canonical_assets.write() {
            assets.retain(|key, _| {
                key.strip_prefix("asset:")
                    .is_some_and(|digest| manifest.contains_key(digest))
            });
            for (digest, stored) in manifest.iter() {
                let source = format!("asset:{digest}");
                if assets.contains_key(&source) {
                    continue;
                }
                match stored.inline() {
                    Some(bytes) => {
                        assets.insert(source, bytes.to_vec());
                    }
                    None => missing.push(digest.clone()),
                }
            }
        }
        self.canonical_asset_manifest = manifest;
        if missing.is_empty() {
            self.canonical_assets_waiting = None;
            return;
        }
        // Waiting until the read says otherwise; a read started meanwhile
        // starts over with what is missing then.
        self.canonical_assets_waiting = Some(looked);
        self.asset_read_looked = looked;
        if let Some(read) = self.asset_reads.start(missing) {
            self.take_asset_read(read);
        }
    }

    /// Show the pictures a read found.
    fn take_asset_read(&mut self, read: Read) {
        let manifest = &self.canonical_asset_manifest;
        let mut arrived = false;
        let mut waiting = false;
        if let Ok(mut assets) = self.canonical_assets.write() {
            for (source, bytes) in read {
                let named = source
                    .strip_prefix("asset:")
                    .is_some_and(|digest| manifest.contains_key(digest));
                if named && !assets.contains_key(&source) {
                    assets.insert(source, bytes);
                    arrived = true;
                }
            }
            waiting = manifest
                .keys()
                .any(|digest| !assets.contains_key(&format!("asset:{digest}")));
        }
        self.canonical_assets_waiting = waiting.then_some(self.asset_read_looked);
        if arrived {
            // The image store remembers a failed load, and an image block is
            // laid out (and asked for) in the frame it appears, before its
            // bytes were in the snapshot: forget the failures so the images
            // that were placeholders are fetched again.
            if let Some(store) = self.main.image_store() {
                store.forget_failed();
            }
            self.self_dirty = true;
        }
    }
}

fn read(digests: &[String]) -> Read {
    let content = crate::notebook::content();
    digests
        .iter()
        .filter_map(|digest| {
            let bytes = content
                .read(digest)
                .filter(|bytes| tilo_content::verify(digest, bytes))?;
            Some((format!("asset:{digest}"), bytes))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_read_at_once_off_the_frame_loop_and_on_a_worker_there() {
        let mut reads = AssetReads::new();
        let digest = tilo_content::hash_bytes(b"picture");
        assert!(
            reads.start(vec![digest.clone()]).is_some(),
            "a thread that may wait reads at once"
        );
        let _loop = ui::storage::waiting::forbid_strictly_on_this_thread();
        assert!(
            reads.start(vec![digest]).is_none(),
            "the frame loop never reads them itself"
        );
        tilo_test_env::wait_until("the worker to read the pictures", || reads.take().is_some());
        assert!(reads.take().is_none(), "a read is taken once");
    }
}
