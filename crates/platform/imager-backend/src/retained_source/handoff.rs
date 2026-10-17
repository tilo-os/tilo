//! Owned second connections to the same private provider; never a directory fallback.
use runtime_fs::workspace_server::{
    PendingHandoffRegistry, serve_history_read_connected, serve_pending_connected,
};
use std::{
    io::{self, Read},
    net::Shutdown,
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
const LIMIT: usize = 64;
const TICK: Duration = Duration::from_millis(20);
struct Child {
    socket: UnixStream,
    thread: thread::JoinHandle<io::Result<()>>,
    ticket: Arc<Mutex<Option<[u8; 16]>>>,
}
pub(super) struct Handoffs {
    stop: Arc<AtomicBool>,
    registry: Arc<PendingHandoffRegistry>,
    thread: Option<thread::JoinHandle<io::Result<()>>>,
}
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn root_connector(stream: &UnixStream) -> io::Result<()> {
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length =
        libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut peer).cast(),
            &raw mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() || peer.uid != 0 {
        return Err(refused());
    }
    Ok(())
}
fn join(child: Child, registry: &PendingHandoffRegistry) -> io::Result<()> {
    let result = child
        .thread
        .join()
        .map_err(|_| io::Error::other("source handoff panicked"))
        .and_then(|result| result);
    let ticket = *child
        .ticket
        .lock()
        .map_err(|_| io::Error::other("source handoff ticket poisoned"))?;
    if let Some(ticket) = ticket {
        registry.observed_join(
            ticket,
            result.as_ref().copied().map_err(ToString::to_string),
        )?;
    }
    result
}
struct Fence {
    done: Arc<AtomicBool>,
    watch: Option<thread::JoinHandle<()>>,
}
impl Fence {
    fn finish(&mut self) -> io::Result<()> {
        self.done.store(true, Ordering::Release);
        self.watch.take().map_or(Ok(()), |watch| {
            watch
                .join()
                .map_err(|_| io::Error::other("source handoff fence panicked"))
        })
    }
}
impl Drop for Fence {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
enum Route {
    Pending(Arc<dyn runtime_fs::directory_backend::WorkspacePendingReplacementBackend>),
    Read(Arc<dyn runtime_fs::directory_backend::WorkspaceHistoryReadSubjectBackend>),
}
impl Route {
    fn check(&self) -> io::Result<()> {
        match self {
            Self::Pending(cap) => cap.check(),
            Self::Read(cap) => cap.check(),
        }
        .map_err(io::Error::other)
    }
}
fn serve(
    mut stream: UnixStream,
    registry: Arc<PendingHandoffRegistry>,
    runtime: tokio::runtime::Handle,
    stop: Arc<AtomicBool>,
    active_ticket: Arc<Mutex<Option<[u8; 16]>>>,
) -> io::Result<()> {
    root_connector(&stream)?;
    // Intrinsic absolute deadline covers partial prelude and QPE progress.
    // Canonical QPE may clear per-syscall timeouts; only this exact socket is shut.
    let guard = stream.try_clone()?;
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .ok_or_else(refused)?;
    let watch = thread::Builder::new()
        .name("source-handoff-deadline".into())
        .spawn(move || {
            while !finished.load(Ordering::Acquire) {
                if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                    let _ = guard.shutdown(Shutdown::Both);
                    break;
                }
                thread::sleep(TICK);
            }
        })?;
    let mut fence = Fence {
        done,
        watch: Some(watch),
    };
    let result = (|| {
        let mut prelude = [0u8; 52];
        stream.read_exact(&mut prelude)?;
        let ticket = prelude[4..20].try_into().map_err(|_| refused())?;
        let challenge = prelude[20..].try_into().map_err(|_| refused())?;
        let route = match &prelude[..4] {
            b"RPH1" => {
                let pending = registry.take(ticket, challenge).map_err(io::Error::other)?;
                *active_ticket
                    .lock()
                    .map_err(|_| io::Error::other("source handoff ticket poisoned"))? =
                    Some(ticket);
                registry.begin_pending(ticket, pending.clone(), stream.try_clone()?)?;
                Route::Pending(pending)
            }
            b"RHR1" => {
                let subject = registry
                    .take_read(ticket, challenge)
                    .map_err(io::Error::other)?;
                *active_ticket
                    .lock()
                    .map_err(|_| io::Error::other("source handoff ticket poisoned"))? =
                    Some(ticket);
                registry.begin_read(ticket, subject.clone(), stream.try_clone()?)?;
                Route::Read(subject)
            }
            _ => return Err(refused()),
        };
        // This running source task performs canonical kernel sender proof.
        // The daemon independently compares it with the root-pinned worker.
        runtime_portal::process_binding::complete(&mut stream)?;
        route.check()?;
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok((stream, route))
    })();
    drop(active_ticket);
    fence.finish()?;
    let (stream, route) = result?;
    match route {
        Route::Pending(pending) => {
            serve_pending_connected(stream, pending, runtime, registry)?.wait()
        }
        Route::Read(subject) => {
            serve_history_read_connected(stream, subject, runtime, registry)?.wait()
        }
    }
}
impl Handoffs {
    pub(super) fn start(
        listener: UnixListener,
        registry: Arc<PendingHandoffRegistry>,
        runtime: tokio::runtime::Handle,
    ) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let registered = registry.clone();
        let worker = thread::Builder::new()
            .name("source-pending-handoffs".into())
            .spawn(move || {
                let mut children: Vec<Child> = Vec::new();
                let mut failure: Option<io::Error> = None;
                while !cancelled.load(Ordering::Acquire) {
                    while let Some(index) =
                        children.iter().position(|child| child.thread.is_finished())
                    {
                        if let Err(error) = join(children.swap_remove(index), &registered) {
                            failure.get_or_insert(error);
                        }
                    }
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if children.len() >= LIMIT {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                            let socket = match stream.try_clone() {
                                Ok(socket) => socket,
                                Err(error) => {
                                    failure.get_or_insert(error);
                                    continue;
                                }
                            };
                            let registry = registered.clone();
                            let runtime = runtime.clone();
                            let stop = cancelled.clone();
                            let ticket = Arc::new(Mutex::new(None));
                            let active_ticket = ticket.clone();
                            match thread::Builder::new()
                                .name("source-pending-handoff".into())
                                .spawn(move || {
                                    serve(stream, registry, runtime, stop, active_ticket)
                                }) {
                                Ok(thread) => children.push(Child {
                                    socket,
                                    thread,
                                    ticket,
                                }),
                                Err(error) => {
                                    failure.get_or_insert(error);
                                }
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(TICK);
                        }
                        Err(error) => {
                            failure.get_or_insert(error);
                            break;
                        }
                    }
                }
                // Close all transports before joining callbacks. Original session's
                // closed witness rejects queued native effects; no timeout success.
                for child in &children {
                    let _ = child.socket.shutdown(Shutdown::Both);
                }
                for child in children {
                    if let Err(error) = join(child, &registered) {
                        failure.get_or_insert(error);
                    }
                }
                failure.map_or(Ok(()), Err)
            })?;
        Ok(Self {
            stop,
            registry,
            thread: Some(worker),
        })
    }
    pub(super) fn retire(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        let closed = self.registry.close();
        let joined = match self.thread.take() {
            Some(worker) => worker
                .join()
                .map_err(|_| io::Error::other("source handoff supervisor panicked"))
                .and_then(|result| result),
            None => Ok(()),
        };
        closed.and(joined)
    }
}
impl Drop for Handoffs {
    fn drop(&mut self) {
        let _ = self.retire();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn unarmed_actual_socket_cannot_obtain_any_bootstrap() {
        assert_eq!(
            rustix::process::geteuid().as_raw(),
            0,
            "root-only source handoff fixture"
        );
        let (source, mut controller) = UnixStream::pair().unwrap();
        let registry = Arc::new(PendingHandoffRegistry::default());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let worker_runtime = runtime.handle().clone();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let registered = registry.clone();
        let ticket = Arc::new(Mutex::new(None));
        let active_ticket = ticket.clone();
        let socket = source.try_clone().unwrap();
        let worker = thread::spawn(move || {
            serve(source, registered, worker_runtime, cancelled, active_ticket)
        });
        let mut prelude = [0; 52];
        prelude[..4].copy_from_slice(b"RPH1");
        prelude[4..].fill(1);
        controller.write_all(&prelude).unwrap();
        controller
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        // Reap the completed child as the supervisor does, releasing its
        // cancellation descriptor before observing EOF on the controller.
        assert!(
            join(
                Child {
                    socket,
                    thread: worker,
                    ticket: ticket.clone(),
                },
                &registry,
            )
            .is_err()
        );
        let mut byte = [0];
        assert_eq!(
            controller.read(&mut byte).unwrap(),
            0,
            "no QPE or capability for nonexistent reservation"
        );
        assert!(ticket.lock().unwrap().is_none());
        stop.store(true, Ordering::Release);
    }
}
