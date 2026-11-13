//! Placeholder bytes from the content plane. Filling a range makes it resident in
//! the content store first, and on the store volume copying it into the
//! placeholder clones extents instead of writing the bytes twice.
//!
//! The transfer runs as a task of its own, so a worker returns once the access it
//! serves is answered; a retry finds the bytes in the store or joins the transfer.

use std::fs::File;
use std::io;
use std::ops::Range;
use std::os::fd::BorrowedFd;
use std::os::unix::fs::FileExt as _;
use std::sync::Arc;

use fs_native::hydrate::Cancel;
use iroh_blobs::Hash;

use crate::hydration::{Hydrator, Priority};

pub const DEFAULT_PARALLEL_CHUNKS: usize = 16;

pub struct ContentFetcher {
    content: Arc<Hydrator>,
    runtime: tokio::runtime::Handle,
    parallel: usize,
}

impl ContentFetcher {
    pub fn new(content: Arc<Hydrator>, runtime: tokio::runtime::Handle, parallel: usize) -> Self {
        Self {
            content,
            runtime,
            parallel: parallel.max(1),
        }
    }
}

fn unavailable(error: &anyhow::Error) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, error.to_string())
}

/// Wait for `transfer`, unless `cancel` fires: the transfer keeps going and what
/// it brings lands in the store.
async fn unless_cancelled<T: Send + 'static>(
    cancel: &Cancel,
    transfer: tokio::task::JoinHandle<anyhow::Result<T>>,
) -> io::Result<T> {
    let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel::<()>();
    cancel.on_cancel(move || {
        let _ = cancelled_tx.send(());
    });
    let cancelled = async move {
        // Dropped unsent: the fill is never cancelled.
        if cancelled_rx.await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        joined = transfer => match joined {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(unavailable(&error)),
            Err(stopped) => Err(io::Error::other(format!("the transfer stopped: {stopped}"))),
        },
        () = cancelled => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "nobody waits for these bytes any more",
        )),
    }
}

impl fs_native::hydrate::Fetcher for ContentFetcher {
    fn fetch(&self, content: &[u8; 32], offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let hydrator = Arc::clone(&self.content);
        let hash = Hash::from(*content);
        let parallel = self.parallel;
        self.runtime
            .block_on(async move { hydrator.read_parallel(hash, offset, len, parallel).await })
            .map_err(|error| unavailable(&error))
    }

    fn fill(
        &self,
        content: &[u8; 32],
        range: Range<u64>,
        dest: BorrowedFd<'_>,
        ahead: bool,
        cancel: &Cancel,
    ) -> io::Result<()> {
        if cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "nobody waits for these bytes any more",
            ));
        }
        let hydrator = Arc::clone(&self.content);
        let hash = Hash::from(*content);
        let priority = if ahead {
            Priority::ReadAhead
        } else {
            Priority::Foreground
        };
        let (start, len) = (range.start, range.end.saturating_sub(range.start));
        let transfer = self
            .runtime
            .spawn(async move { hydrator.hydrate_range(hash, start, len, priority).await });
        let manifest = self.runtime.block_on(unless_cancelled(cancel, transfer))?;
        // A duplicate shares the event descriptor's open file, which raises
        // no events of its own.
        let dest = File::from(dest.try_clone_to_owned()?);
        if crate::content::is_file_backed(&manifest) {
            return self.content.store.files().copy_to(content, &range, &dest);
        }
        let bytes = self.fetch(content, start, len)?;
        if bytes.len() as u64 != len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "content is shorter than the range",
            ));
        }
        dest.write_all_at(&bytes, start)
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd as _;
    use std::time::{Duration, Instant};

    use fs_native::hydrate::{Cancel, Fetcher as _};

    use super::*;
    use crate::store::ContentStore;
    use crate::test_bytes::pseudo;

    /// A fill of bytes only a quiet peer holds returns as soon as the access it
    /// serves is answered, and serves another file's access next.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_given_up_fill_frees_its_worker_while_the_transfer_goes_on() {
        let len = 3 * 1024 * 1024;
        let elsewhere = ContentStore::memory();
        let quiet_id = elsewhere
            .write_content(&pseudo(len, 21))
            .await
            .unwrap()
            .unwrap();
        let store = Arc::new(ContentStore::memory());
        let here = pseudo(len, 22);
        let here_id = store.write_content(&here).await.unwrap().unwrap();
        let manifest = elsewhere.local(quiet_id).await.unwrap();
        assert_eq!(store.ingest(manifest).await.unwrap(), quiet_id);
        let quiet = crate::hydration::test_peers::silent().await;
        let hydrator = Arc::new(Hydrator::new(test_relay::direct_endpoint().await, store));
        hydrator.add_peer(quiet.endpoint().addr());
        let fetcher = ContentFetcher::new(
            Arc::clone(&hydrator),
            tokio::runtime::Handle::current(),
            DEFAULT_PARALLEL_CHUNKS,
        );
        let cancel = Cancel::default();

        let worker = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                let quiet_dest = tempfile::tempfile().unwrap();
                let given_up = fetcher.fill(
                    quiet_id.as_bytes(),
                    0..64 * 1024,
                    quiet_dest.as_fd(),
                    false,
                    &cancel,
                );
                let freed_at = Instant::now();
                let here_dest = tempfile::tempfile().unwrap();
                let next = fetcher.fill(
                    here_id.as_bytes(),
                    0..64 * 1024,
                    here_dest.as_fd(),
                    false,
                    &Cancel::default(),
                );
                let served_in = freed_at.elapsed();
                let mut filled = vec![0_u8; 64 * 1024];
                std::os::unix::fs::FileExt::read_exact_at(&here_dest, &mut filled, 0).unwrap();
                (given_up, freed_at, next, served_in, filled)
            })
        };
        tilo_test_env::within("the quiet transfer is on the wire", async {
            while !hydrator.is_fetching(quiet_id) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        let cancelled_at = Instant::now();
        cancel.cancel();
        let (given_up, freed_at, next, served_in, filled) =
            tokio::task::spawn_blocking(move || worker.join().unwrap())
                .await
                .unwrap();

        let error = given_up.expect_err("a quiet peer served nothing");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
        let held = freed_at.saturating_duration_since(cancelled_at);
        assert!(
            held < Duration::from_secs(10),
            "the worker was held {held:?} after its access was answered"
        );
        next.expect("the next access is served");
        assert!(
            served_in < Duration::from_secs(10),
            "the next access waited {served_in:?}"
        );
        assert!(filled == here[..64 * 1024], "the next file's bytes");
        assert!(
            hydrator.is_fetching(quiet_id),
            "the transfer goes on without its worker"
        );
    }
}
