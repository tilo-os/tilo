//! One inherited service connection shared by every client in the process.
//!
//! The launcher hands a process exactly one connected socket per service.
//! Two Cap'n Proto vats on one socket corrupt each other's stream, and two
//! `OwnedFd`s for one descriptor abort the process when the second is
//! dropped. So the first caller starts a hub thread that owns a duplicate of
//! the inherited socket; every caller then gets a private socket pair whose
//! far end the hub serves with the service's bootstrap capability, forwarding
//! each call over the single real connection.

use std::collections::HashMap;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::{Mutex, OnceLock};

use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use rustix::io::{FdFlags, fcntl_setfd};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::{CapnpClient, NotAvailable};

type Hub = mpsc::UnboundedSender<StdUnixStream>;

/// A private connection to the service behind the inherited descriptor
/// `fd`, multiplexed with every other client of it in this process. The
/// inherited descriptor itself stays open and no client owns it.
pub(crate) fn connect(name: &str, fd: RawFd) -> Result<OwnedFd, NotAvailable> {
    static HUBS: OnceLock<Mutex<HashMap<RawFd, Hub>>> = OnceLock::new();
    let hub = {
        let mut hubs = HUBS.get_or_init(Mutex::default).lock().map_err(|_| {
            NotAvailable::ConnectionFailed(io::Error::other("service connection registry poisoned"))
        })?;
        match hubs.entry(fd) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.get().clone(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(start(name, fd)?).clone()
            }
        }
    };
    let (local, remote) = StdUnixStream::pair()?;
    hub.send(remote).map_err(|_| {
        NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::BrokenPipe,
            format!("the {name} service connection has ended"),
        ))
    })?;
    Ok(OwnedFd::from(local))
}

fn start(name: &str, fd: RawFd) -> Result<Hub, NotAvailable> {
    // SAFETY: `service_fd` has just checked that the slot is open; the borrow
    // lasts only for these calls.
    let inherited = unsafe { BorrowedFd::borrow_raw(fd) };
    // The launcher clears CLOEXEC only for this process's exec; children the
    // app starts later must not hold the service connection.
    fcntl_setfd(inherited, FdFlags::CLOEXEC).map_err(io::Error::from)?;
    let upstream = StdUnixStream::from(inherited.try_clone_to_owned()?);
    upstream.set_nonblocking(true)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name(format!("tilo-svc-{name}"))
        .spawn(move || {
            tokio::task::LocalSet::new().block_on(&runtime, serve(upstream, receiver));
        })?;
    Ok(sender)
}

#[expect(
    clippy::future_not_send,
    reason = "Cap'n Proto RPC is single-threaded; the hub runs on its own LocalSet"
)]
async fn serve(upstream: StdUnixStream, mut clients: mpsc::UnboundedReceiver<StdUnixStream>) {
    // Returning drops `clients`, so later `connect` calls report the end.
    let Ok(upstream) = UnixStream::from_std(upstream) else {
        return;
    };
    let service: capnp::capability::Client =
        CapnpClient::from_connected_io(upstream, rpc_twoparty_capnp::Side::Client).bootstrap();
    while let Some(client) = clients.recv().await {
        let Ok(client) = client
            .set_nonblocking(true)
            .and_then(|()| UnixStream::from_std(client))
        else {
            continue;
        };
        let (read, write) = tokio::io::split(client);
        let network = Box::new(twoparty::VatNetwork::new(
            read.compat(),
            write.compat_write(),
            rpc_twoparty_capnp::Side::Server,
            capnp::message::ReaderOptions::default(),
        ));
        let rpc = RpcSystem::new(network, Some(service.clone()));
        tokio::task::spawn_local(async move {
            let _ = rpc.await;
        });
    }
}
