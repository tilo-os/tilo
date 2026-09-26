//! The store the blob provider serves from.
//!
//! iroh-blobs' provider (`GET`, `GET_MANY`, observe) talks to a store over
//! its request protocol. This one answers the requests the provider makes:
//! a chunk that lives in the file-backed area is read from its entry's
//! file, checked against its hash and sent as a BAO stream with an outboard
//! computed from the bytes just read; everything else (manifests, small
//! content, legacy chunks) comes from the loose blob store. A corrupt or
//! stale file therefore fails the request instead of reaching a peer, and
//! file-backed content is never copied into blobs to be served.

use std::sync::{Arc, Weak};

use bao_tree::io::mixed::{EncodedItem, traverse_ranges_validated};
use bao_tree::io::outboard::PreOrderMemOutboard;
use futures_util::StreamExt;
use iroh_blobs::Hash;
use iroh_blobs::api::Store;
use iroh_blobs::api::blobs::Bitfield;
use iroh_blobs::api::proto::{
    self, BlobStatus, BlobStatusMsg, Command, ExportBaoMsg, ExportBaoRequest, ObserveMsg,
    ObserveRequest,
};
use iroh_blobs::store::IROH_BLOCK_SIZE;
use ref_cast::RefCast as _;

use super::area::FileArea;

/// A [`Store`] for the provider over `loose` and the file-backed `files`.
/// It does not keep the area open: once the content store is gone, only
/// loose blobs are served.
pub(super) fn serving_store(loose: Store, files: &Arc<FileArea>) -> Store {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(run(rx, loose, Arc::downgrade(files)));
    let client: irpc::Client<proto::Request> = irpc::LocalSender::from(tx).into();
    Store::ref_cast(&client).clone()
}

async fn run(
    mut commands: tokio::sync::mpsc::Receiver<Command>,
    loose: Store,
    files: Weak<FileArea>,
) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::ExportBao(msg) => {
                tokio::spawn(export_bao(loose.clone(), files.upgrade(), msg));
            }
            Command::Observe(msg) => {
                tokio::spawn(observe(loose.clone(), files.upgrade(), msg));
            }
            Command::BlobStatus(msg) => {
                tokio::spawn(status(loose.clone(), files.upgrade(), msg));
            }
            Command::WaitIdle(msg) => {
                msg.tx.send(()).await.ok();
            }
            Command::Shutdown(msg) => {
                // The loose store belongs to the content store, which shuts
                // it down itself.
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

async fn export_bao(loose: Store, files: Option<Arc<FileArea>>, msg: ExportBaoMsg) {
    let ExportBaoMsg {
        inner: ExportBaoRequest { hash, ranges },
        mut tx,
        ..
    } = msg;
    let Some((files, location)) = files.and_then(|files| {
        let location = files.find_chunk(hash.as_bytes())?;
        Some((files, location))
    }) else {
        let mut items = loose.export_bao(hash, ranges).stream();
        while let Some(item) = items.next().await {
            if tx.send(item).await.is_err() {
                return;
            }
        }
        return;
    };
    let read = tokio::task::spawn_blocking(move || {
        let bytes = files.read_at(&location.key, location.offset, u64::from(location.len))?;
        let outboard = PreOrderMemOutboard::create(&bytes, IROH_BLOCK_SIZE);
        Ok::<_, std::io::Error>((bytes::Bytes::from(bytes), outboard))
    })
    .await;
    let (bytes, outboard) = match read {
        Ok(Ok(read)) => read,
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
    if outboard.root.as_bytes() != hash.as_bytes() {
        tx.send(io_error(
            std::io::ErrorKind::InvalidData,
            "stored chunk does not match its hash",
        ))
        .await
        .ok();
        return;
    }
    traverse_ranges_validated(bytes, outboard, &ranges, &mut Out(&mut tx))
        .await
        .ok();
}

async fn observe(loose: Store, files: Option<Arc<FileArea>>, msg: ObserveMsg) {
    let ObserveMsg {
        inner: ObserveRequest { hash },
        tx,
        ..
    } = msg;
    if let Some(location) = files.and_then(|f| f.find_chunk(hash.as_bytes())) {
        tx.send(Bitfield::complete(u64::from(location.len)))
            .await
            .ok();
        return;
    }
    let Ok(mut updates) = loose.observe(hash).stream().await else {
        return;
    };
    while let Some(update) = updates.next().await {
        if tx.send(update).await.is_err() {
            return;
        }
    }
}

async fn status(loose: Store, files: Option<Arc<FileArea>>, msg: BlobStatusMsg) {
    let hash: Hash = msg.inner.hash;
    let reply = match files.and_then(|f| f.find_chunk(hash.as_bytes())) {
        Some(location) => BlobStatus::Complete {
            size: u64::from(location.len),
        },
        None => loose.status(hash).await.unwrap_or(BlobStatus::NotFound),
    };
    msg.tx.send(reply).await.ok();
}
