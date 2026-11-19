//! The store the blob provider serves from.
//!
//! iroh-blobs' provider (`GET`, `GET_MANY`, observe) talks to a store over
//! its request protocol. This one answers the requests the provider makes
//! from the file area: an inline blob is sent from its bytes, a whole blob
//! from its file through the outboard persisted beside it, and a chunk of a
//! file-backed version from its entry's file, checked against its hash and
//! sent as a BAO stream with an outboard computed from the bytes just read.
//! A corrupt or stale file therefore fails the request instead of reaching a
//! peer, and nothing is ever copied into a second store to be served.

use std::fs::File;
use std::sync::Arc;

use bao_tree::BaoTree;
use bao_tree::io::mixed::{EncodedItem, traverse_ranges_validated};
use bao_tree::io::outboard::PreOrderMemOutboard;
use iroh_blobs::Hash;
use iroh_blobs::api::Store;
use iroh_blobs::api::blobs::Bitfield;
use iroh_blobs::api::proto::{
    self, BlobStatus, BlobStatusMsg, Command, ExportBaoMsg, ExportBaoRequest, ObserveMsg,
    ObserveRequest,
};
use iroh_blobs::protocol::ChunkRangesExt as _;
use iroh_blobs::store::IROH_BLOCK_SIZE;
use ref_cast::RefCast as _;

use super::area::FileArea;

/// A [`Store`] for the provider over the file-backed `files`. It keeps the area open for as long as
/// the protocol is served, so a peer is answered even after the handle that built the protocol is
/// gone; closing the area ends the answers.
pub(super) fn serving_store(files: &Arc<FileArea>) -> Store {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(run(rx, Arc::clone(files)));
    let client: irpc::Client<proto::Request> = irpc::LocalSender::from(tx).into();
    Store::ref_cast(&client).clone()
}

async fn run(mut commands: tokio::sync::mpsc::Receiver<Command>, files: Arc<FileArea>) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::ExportBao(msg) => {
                tokio::spawn(export_bao(Arc::clone(&files), msg));
            }
            Command::Observe(msg) => {
                tokio::spawn(observe(Arc::clone(&files), msg));
            }
            Command::BlobStatus(msg) => {
                tokio::spawn(status(Arc::clone(&files), msg));
            }
            Command::WaitIdle(msg) => {
                msg.tx.send(()).await.ok();
            }
            Command::Shutdown(msg) => {
                // The area belongs to the content store, which closes it itself.
                msg.tx.send(()).await.ok();
                return;
            }
            // The provider makes no other requests; pushes are refused by
            // the request guard. Dropping the message closes its reply.
            _ => {}
        }
    }
}

/// `bao_tree`'s sender over an irpc channel.
struct Out<'a>(&'a mut irpc::channel::mpsc::Sender<EncodedItem>);

impl bao_tree::io::mixed::Sender for Out<'_> {
    type Error = irpc::channel::SendError;
    async fn send(&mut self, item: EncodedItem) -> Result<(), Self::Error> {
        self.0.send(item).await
    }
}

fn io_error(kind: std::io::ErrorKind, message: &str) -> EncodedItem {
    EncodedItem::Error(bao_tree::io::EncodeError::Io(std::io::Error::new(
        kind, message,
    )))
}

/// Where a blob's ranges are read from.
enum Source {
    /// Bytes inside the area's index database.
    Inline(Vec<u8>),
    /// A whole blob's file, with its persisted outboard.
    Whole { file: File, outboard: Vec<u8> },
    /// A chunk of a file-backed version.
    Chunk { file: File, offset: u64, len: u32 },
}

fn prepare(files: &FileArea, hash: Hash) -> std::io::Result<Option<(u64, Source)>> {
    let key = *hash.as_bytes();
    if let Some(bytes) = files.get_inline(&key) {
        let size = bytes.len() as u64;
        return Ok(Some((size, Source::Inline(bytes))));
    }
    if files.is_blob(&key) {
        let Some(entry) = files.entry(&key) else {
            return Ok(None);
        };
        if !entry.complete() {
            return Ok(None);
        }
        let Some(outboard) = files.outboard(&key) else {
            return Ok(None);
        };
        let file = files.file(&key)?.as_ref().try_clone()?;
        return Ok(Some((entry.size, Source::Whole { file, outboard })));
    }
    if let Some(location) = files.find_chunk(&key) {
        let file = files.file(&location.key)?.as_ref().try_clone()?;
        return Ok(Some((
            u64::from(location.len),
            Source::Chunk {
                file,
                offset: location.offset,
                len: location.len,
            },
        )));
    }
    Ok(None)
}

async fn export_bao(files: Arc<FileArea>, msg: ExportBaoMsg) {
    let ExportBaoMsg {
        inner: ExportBaoRequest { hash, ranges },
        mut tx,
        ..
    } = msg;
    let Some((size, source)) = tokio::task::spawn_blocking(move || prepare(&files, hash))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
    else {
        tx.send(io_error(
            std::io::ErrorKind::NotFound,
            "the store holds no such blob",
        ))
        .await
        .ok();
        return;
    };
    match source {
        Source::Inline(bytes) => {
            let outboard = PreOrderMemOutboard::create(&bytes, IROH_BLOCK_SIZE);
            traverse_ranges_validated(
                bytes::Bytes::from(bytes),
                outboard,
                &ranges,
                &mut Out(&mut tx),
            )
            .await
            .ok();
        }
        Source::Whole { file, outboard } => {
            let tree = BaoTree::new(size, IROH_BLOCK_SIZE);
            if outboard.len() as u64 != tree.outboard_size() {
                tx.send(io_error(
                    std::io::ErrorKind::InvalidData,
                    "the stored outboard does not match its blob",
                ))
                .await
                .ok();
                return;
            }
            let root = bao_tree::blake3::Hash::from(*hash.as_bytes());
            let outboard = PreOrderMemOutboard {
                root,
                tree,
                data: outboard,
            };
            traverse_ranges_validated(file, outboard, &ranges, &mut Out(&mut tx))
                .await
                .ok();
        }
        Source::Chunk { file, offset, len } => {
            let bytes = tokio::task::spawn_blocking(move || {
                use std::os::unix::fs::FileExt as _;
                let mut bytes = vec![0_u8; len as usize];
                file.read_exact_at(&mut bytes, offset)?;
                Ok::<_, std::io::Error>(bytes)
            })
            .await;
            let bytes = match bytes {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(error)) => {
                    tx.send(EncodedItem::Error(bao_tree::io::EncodeError::Io(error)))
                        .await
                        .ok();
                    return;
                }
                Err(_) => {
                    tx.send(io_error(std::io::ErrorKind::Other, "chunk read failed"))
                        .await
                        .ok();
                    return;
                }
            };
            let outboard = PreOrderMemOutboard::create(&bytes, IROH_BLOCK_SIZE);
            if outboard.root.as_bytes() != hash.as_bytes() {
                tx.send(io_error(
                    std::io::ErrorKind::InvalidData,
                    "stored chunk does not match its hash",
                ))
                .await
                .ok();
                return;
            }
            traverse_ranges_validated(
                bytes::Bytes::from(bytes),
                outboard,
                &ranges,
                &mut Out(&mut tx),
            )
            .await
            .ok();
        }
    }
}

/// The bitfield of what is resident: complete for an inline blob, a whole blob or a chunk; a
/// partial whole blob's resident ranges otherwise.
fn bitfield(files: &FileArea, hash: Hash) -> Option<Bitfield> {
    let key = *hash.as_bytes();
    if let Some(bytes) = files.get_inline(&key) {
        return Some(Bitfield::complete(bytes.len() as u64));
    }
    if files.is_blob(&key) {
        let entry = files.entry(&key)?;
        if entry.complete() {
            return Some(Bitfield::complete(entry.size));
        }
        let (size, resident) = files.resident(&key)?;
        let mut ranges = iroh_blobs::protocol::ChunkRanges::empty();
        for range in resident {
            ranges |= iroh_blobs::protocol::ChunkRanges::bytes(range);
        }
        return Some(Bitfield::new(ranges, size));
    }
    files
        .find_chunk(&key)
        .map(|location| Bitfield::complete(u64::from(location.len)))
}

async fn observe(files: Arc<FileArea>, msg: ObserveMsg) {
    let ObserveMsg {
        inner: ObserveRequest { hash },
        tx,
        ..
    } = msg;
    let update = tokio::task::spawn_blocking(move || bitfield(&files, hash))
        .await
        .ok()
        .flatten();
    if let Some(update) = update {
        tx.send(update).await.ok();
    }
}

async fn status(files: Arc<FileArea>, msg: BlobStatusMsg) {
    let hash: Hash = msg.inner.hash;
    let reply = tokio::task::spawn_blocking(move || {
        let key = *hash.as_bytes();
        if let Some(bytes) = files.get_inline(&key) {
            return BlobStatus::Complete {
                size: bytes.len() as u64,
            };
        }
        if let Some(entry) = files.is_blob(&key).then(|| files.entry(&key)).flatten() {
            return if entry.complete() {
                BlobStatus::Complete { size: entry.size }
            } else {
                BlobStatus::Partial {
                    size: Some(entry.resident),
                }
            };
        }
        files
            .find_chunk(&key)
            .map_or(BlobStatus::NotFound, |location| BlobStatus::Complete {
                size: u64::from(location.len),
            })
    })
    .await
    .unwrap_or(BlobStatus::NotFound);
    msg.tx.send(reply).await.ok();
}
