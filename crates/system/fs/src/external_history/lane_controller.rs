//! One daemon-owned `LocalSet` retains real non-Send source capabilities across
//! separately authenticated storage connections. There is no new listener.
use super::coordinator::AcknowledgedReplacement;
use super::{
    Arc, AtomicBool, Duration, ExternalSourceAdmission, UnixStream, io, owned_lane, refused,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::mpsc,
};
#[derive(Default)]
pub(super) struct AcknowledgedOwners {
    pub(super) completed: BTreeMap<String, Rc<AcknowledgedReplacement>>,
    pub(super) reserved: std::collections::BTreeSet<String>,
}
pub(super) type AcknowledgedRegistry = Rc<RefCell<AcknowledgedOwners>>;
struct Lane {
    admission: ExternalSourceAdmission,
    stream: UnixStream,
    cancel: Arc<AtomicBool>,
    completion: mpsc::SyncSender<io::Result<()>>,
}
struct Custody {
    sender: Option<mpsc::SyncSender<Lane>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Custody {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            // Final factory custody can retire on this very controller after
            // the last real admission disappears. It then returns normally;
            // joining itself would deadlock that actual owned retirement.
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}
#[derive(Clone)]
pub(super) struct Controller {
    custody: Arc<Custody>,
}
impl Controller {
    pub(super) fn start() -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Lane>(64);
        let (ready, started) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("retained-history-controller".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                tokio::task::LocalSet::new().block_on(&runtime, async move {
                    let registry: AcknowledgedRegistry =
                        Rc::new(RefCell::new(AcknowledgedOwners::default()));
                    let active = Rc::new(Cell::new(0usize));
                    let mut disconnected = false;
                    loop {
                        {
                            let mut owners = registry.borrow_mut();
                            let retired: Vec<_> = owners
                                .completed
                                .iter()
                                .filter(|(_, owner)| !owner.discovery_current())
                                .map(|(key, _)| key.clone())
                                .collect();
                            for key in retired {
                                owners.completed.remove(&key);
                                owners.reserved.remove(&key);
                            }
                        }
                        while active.get() < 64 {
                            match receiver.try_recv() {
                                Ok(lane) => {
                                    active.set(active.get().saturating_add(1));
                                    let active = Rc::clone(&active);
                                    let registry = Rc::clone(&registry);
                                    tokio::task::spawn_local(async move {
                                        let result = owned_lane::serve(
                                            lane.admission,
                                            lane.stream,
                                            lane.cancel,
                                            registry,
                                        )
                                        .await;
                                        let _ = lane.completion.send(result);
                                        active.set(active.get().saturating_sub(1));
                                    });
                                }
                                Err(mpsc::TryRecvError::Empty) => break,
                                Err(mpsc::TryRecvError::Disconnected) => {
                                    disconnected = true;
                                    break;
                                }
                            }
                        }
                        if disconnected && active.get() == 0 {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                });
            })?;
        started.recv().map_err(io::Error::other)??;
        Ok(Self {
            custody: Arc::new(Custody {
                sender: Some(sender),
                thread: Some(thread),
            }),
        })
    }
    pub(super) fn serve(
        &self,
        admission: ExternalSourceAdmission,
        stream: UnixStream,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<()> {
        let (completion, result) = mpsc::sync_channel(1);
        self.custody
            .sender
            .as_ref()
            .ok_or_else(refused)?
            .try_send(Lane {
                admission,
                stream,
                cancel,
                completion,
            })
            .map_err(|_| refused())?;
        result.recv().map_err(io::Error::other)?
    }
}
