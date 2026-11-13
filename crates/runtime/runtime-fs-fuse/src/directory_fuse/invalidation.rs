//! Kernel dentry eviction: once the table holds more than `TRIM_AT` nodes, the
//! thread asks the kernel to drop lookups it no longer needs. Only an actual
//! FORGET releases a lookup, and notifications run outside callbacks and the
//! table lock.
use super::{Node, ROOT, TRIM_AT, TRIM_TO, Table};
use fuser::{INodeNo, Notifier};
use std::{
    io,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone)]
pub struct InvalidationControl {
    table: Weak<Mutex<Table>>,
    sender: Arc<Mutex<Option<SyncSender<()>>>>,
    started: Arc<AtomicBool>,
}
/// A stop request for this FUSE process, not a sign the kernel retired it.
/// Drop does not wait on a notifier call that may need FUSE dispatch; the
/// `NamespaceOwner` still stops the worker process.
pub struct InvalidationWorker {
    stop: Arc<AtomicBool>,
    control: InvalidationControl,
    thread: Option<JoinHandle<()>>,
}
impl InvalidationControl {
    pub(super) fn new(table: &Arc<Mutex<Table>>) -> Self {
        Self {
            table: Arc::downgrade(table),
            sender: Arc::new(Mutex::new(None)),
            started: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(super) fn wake(&self) {
        if let Ok(sender) = self.sender.lock()
            && let Some(sender) = sender.as_ref()
        {
            let _ = sender.try_send(());
        }
    }
    /// Start only with the notifier belonging to this filesystem's Session.
    pub fn start(&self, notifier: Notifier) -> io::Result<InvalidationWorker> {
        let mut slot = self
            .sender
            .lock()
            .map_err(|_| io::Error::other("FUSE notifier custody failed"))?;
        if slot.is_some() || self.started.load(Ordering::Acquire) {
            return Err(io::Error::other("FUSE notifier already started"));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let table = self.table.clone();
        let thread = thread::Builder::new()
            .name("home-dentry-invalidation".into())
            .spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    match receiver.recv_timeout(Duration::from_millis(50)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => (),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    let Some(table) = table.upgrade() else { break };
                    // At most RESIDENT_NODES candidates, never all of Home's.
                    let candidates = match table.lock() {
                        Ok(state) if state.nodes.len() >= TRIM_AT => {
                            let count = state.nodes.len().saturating_sub(TRIM_TO);
                            state
                                .nodes
                                .iter()
                                .filter(|&(&ino, node)| eligible(&state, ino, node))
                                .take(count)
                                .map(|(&ino, node)| (ino, node.clone()))
                                .collect::<Vec<_>>()
                        }
                        Ok(_) => continue,
                        Err(_) => break,
                    };
                    for (ino, candidate) in candidates {
                        if stopped.load(Ordering::Acquire) {
                            break;
                        }
                        let current = table.lock().is_ok_and(|mut state| {
                            let ready = state.nodes.get(&ino).is_some_and(|node| {
                                node.parent == candidate.parent
                                    && node.name == candidate.name
                                    && node.identity == candidate.identity
                                    && eligible(&state, ino, node)
                            });
                            if ready && let Some(node) = state.nodes.get_mut(&ino) {
                                node.invalidating = true;
                            }
                            ready
                        });
                        if !current {
                            clear_candidate(&table, ino, &candidate);
                            continue;
                        }
                        let Some(name) = candidate.name.as_ref() else {
                            clear_candidate(&table, ino, &candidate);
                            continue;
                        };
                        if let Err(failure) =
                            notifier.inval_entry(INodeNo(candidate.parent), name.as_os_str())
                        {
                            clear_candidate(&table, ino, &candidate);
                            if matches!(failure.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) {
                                break;
                            }
                            if let Ok(mut state) = table.lock() {
                                state.notification_error =
                                    Some(failure.raw_os_error().unwrap_or(libc::EIO));
                            }
                            return;
                        }
                        // ENOENT is success: the positive reply may not be
                        // installed yet, or the dentry is busy.
                        clear_candidate(&table, ino, &candidate);
                    }
                }
            })?;
        self.started.store(true, Ordering::Release);
        *slot = Some(sender);
        drop(slot);
        Ok(InvalidationWorker {
            stop,
            control: self.clone(),
            thread: Some(thread),
        })
    }
}
fn clear_candidate(table: &Mutex<Table>, ino: u64, candidate: &Node) {
    if let Ok(mut table) = table.lock()
        && let Some(node) = table.nodes.get_mut(&ino)
        && node.parent == candidate.parent
        && node.name == candidate.name
        && node.identity == candidate.identity
    {
        node.invalidating = false;
    }
}
fn unused(table: &Table, ino: u64, node: &Node) -> bool {
    ino != ROOT
        && node.published
        && node.lookups > 0
        && !table.files.values().any(|file| file.node == ino)
        && !table.rows.values().any(|rows| rows.node == ino)
        && !table.nodes.values().any(|child| child.parent == ino)
}
fn eligible(table: &Table, ino: u64, node: &Node) -> bool {
    !node.invalidating && unused(table, ino, node)
}
impl Drop for InvalidationWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut sender) = self.control.sender.lock() {
            sender.take();
        }
        // Detaching is not proof the thread stopped; `NamespaceOwner` handles
        // the exit.
        if let Some(thread) = self.thread.take()
            && thread.is_finished()
        {
            let _ = thread.join();
        }
    }
}
