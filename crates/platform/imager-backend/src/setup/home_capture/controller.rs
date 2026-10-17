//! App-side owner for an already canonically elevated capture helper. No
//! snapshot descriptor or path crosses this bounded status/control protocol.
use super::super::portable_home_media::MediaGrant;
use super::super::portable_transfer::ArchiveProgress;
use super::{Command, RpcAction, RpcListener, RpcReply};
use super::{Status, failure};
use std::{
    io,
    process::Child,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

pub struct Controller {
    capture: crate::capture::Capture,
    actor: Option<mpsc::SyncSender<Pending>>,
    actor_cancel: Arc<AtomicBool>,
}

struct Pending {
    action: RpcAction,
    output: Option<Arc<MediaGrant>>,
    mounted: Option<Arc<super::super::GrantedHomeRoots>>,
    retained: Option<Arc<super::super::migration::PinnedDirectory>>,
    result: mpsc::SyncSender<io::Result<RpcReply>>,
    progress: Arc<Mutex<Option<super::super::portable_transfer::ArchiveProgress>>>,
}
pub struct Ticket {
    result: mpsc::Receiver<io::Result<RpcReply>>,
    progress: Arc<Mutex<Option<super::super::portable_transfer::ArchiveProgress>>>,
}
fn record_progress(
    slot: &Mutex<Option<ArchiveProgress>>,
    value: ArchiveProgress,
) -> io::Result<()> {
    *slot
        .lock()
        .map_err(|_| failure("capture progress poisoned"))? = Some(value);
    Ok(())
}
impl Ticket {
    pub fn progress(&self) -> io::Result<Option<super::super::portable_transfer::ArchiveProgress>> {
        Ok(self
            .progress
            .lock()
            .map_err(|_| failure("capture progress poisoned"))?
            .take())
    }
    pub fn poll(&self) -> io::Result<Option<RpcReply>> {
        match self.result.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(failure("capture RPC actor stopped")),
        }
    }
}

impl Controller {
    /// The backend owns helper revocation, status framing and request custody.
    pub fn attach(child: Child, request: tempfile::TempDir, operation: &str) -> io::Result<Self> {
        Ok(Self {
            capture: crate::capture::Capture::attach(child, request, operation)
                .map_err(io::Error::from)?,
            actor: None,
            actor_cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn attach_with_rpc(
        child: Child,
        request: tempfile::TempDir,
        operation: &str,
        listener: RpcListener,
    ) -> io::Result<Self> {
        let root_pid = child.id();
        let mut controller = Self::attach(child, request, operation)?;
        let operation = operation.to_owned();
        let (sender, receiver) = mpsc::sync_channel::<Pending>(1);
        let cancel = controller.actor_cancel.clone();
        std::thread::spawn(move || {
            let mut session = match listener.accept(root_pid, &operation, &cancel) {
                Ok(value) => value,
                Err(_) => return,
            };
            while let Ok(pending) = receiver.recv() {
                let result = (|| {
                    let output = pending
                        .output
                        .as_ref()
                        .map(|grant| grant.clone_reviewed_directory())
                        .transpose()?;
                    let reply = if let Some(source) = &pending.retained {
                        if !matches!(&pending.action, RpcAction::RegisterRetainedCaller)
                            || pending.output.is_some()
                            || pending.mounted.is_some()
                        {
                            return Err(failure(
                                "Retained source cannot join unrelated grant bundles",
                            ));
                        }
                        session.call_with_retained(source, &cancel, &mut |value| {
                            record_progress(&pending.progress, value)
                        })?
                    } else {
                        session.call_with_mounted(
                            pending.action,
                            output,
                            pending.mounted.as_deref(),
                            Some(&cancel),
                            &mut |value| record_progress(&pending.progress, value),
                        )?
                    };
                    if let Some(grant) = pending.output {
                        grant.clone_reviewed_directory()?;
                    }
                    Ok(reply)
                })();
                let failed = result.is_err();
                let _ = pending.result.try_send(result);
                if failed {
                    break;
                }
            }
            session.shutdown();
        });
        controller.actor = Some(sender);
        Ok(controller)
    }

    pub fn submit(&self, command: Command, output: Option<Arc<MediaGrant>>) -> io::Result<Ticket> {
        if command.original_authority().is_some() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let needs_output = !matches!(&command, Command::Review);
        if needs_output != output.is_some() {
            return Err(failure("capture command output grant differs"));
        }
        self.enqueue(
            RpcAction::Run {
                command,
                has_output: needs_output,
            },
            output,
            None,
        )
    }
    /// Explicit registration only; it does not begin an immutable Home epoch.
    pub fn register_original_async(&self) -> io::Result<Ticket> {
        self.enqueue(RpcAction::RegisterOriginal, None, None)
    }
    pub fn register_retained_caller_async(
        &self,
        source: Arc<super::super::migration::PinnedDirectory>,
    ) -> io::Result<Ticket> {
        source.validate()?;
        self.enqueue_with_retained(RpcAction::RegisterRetainedCaller, None, None, Some(source))
    }
    pub fn validate_async(&self) -> io::Result<Ticket> {
        self.enqueue(RpcAction::Validate, None, None)
    }
    /// Revokes an active export immediately without waiting for its actor queue.
    /// The owner continues polling status until acknowledged release/exit.
    pub fn cancel_now(&mut self) {
        self.actor_cancel.store(true, Ordering::Release);
        self.capture.cancel_now();
    }
    /// Existing original-source helper only: transfer the consumed matching
    /// permission generation; a witness is returned only after actual Store ACK.
    pub fn register_original_mounted_async(
        &self,
        roots: Arc<super::super::GrantedHomeRoots>,
    ) -> io::Result<Ticket> {
        let entries = roots.with_roots(&self.actor_cancel, |granted| {
            Ok(granted[1..]
                .iter()
                .map(super::rpc::MountedInput::from_granted)
                .collect())
        })?;
        self.enqueue(
            RpcAction::RegisterOriginalMounted {
                generation: roots.generation(),
                entries,
            },
            None,
            Some(roots),
        )
    }
    fn enqueue(
        &self,
        action: RpcAction,
        output: Option<Arc<MediaGrant>>,
        mounted: Option<Arc<super::super::GrantedHomeRoots>>,
    ) -> io::Result<Ticket> {
        self.enqueue_with_retained(action, output, mounted, None)
    }
    fn enqueue_with_retained(
        &self,
        action: RpcAction,
        output: Option<Arc<MediaGrant>>,
        mounted: Option<Arc<super::super::GrantedHomeRoots>>,
        retained: Option<Arc<super::super::migration::PinnedDirectory>>,
    ) -> io::Result<Ticket> {
        let sender = self
            .actor
            .as_ref()
            .ok_or_else(|| failure("capture RPC actor is not connected"))?;
        let (result, ticket) = mpsc::sync_channel(1);
        let progress = Arc::new(Mutex::new(None));
        sender
            .try_send(Pending {
                action,
                output,
                mounted,
                retained,
                result,
                progress: progress.clone(),
            })
            .map_err(|_| failure("capture RPC actor is busy or closed"))?;
        Ok(Ticket {
            result: ticket,
            progress,
        })
    }

    pub fn poll(&mut self) -> io::Result<Option<Status>> {
        self.capture.poll()
    }

    pub fn cancel_and_wait(&mut self) -> io::Result<()> {
        self.capture.cancel_and_wait()
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.actor_cancel.store(true, Ordering::Release);
        self.actor = None;
        self.capture.cancel_now();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::portable_transfer::ArchivePhase;
    #[test]
    fn slow_ticket_consumer_keeps_one_latest_progress_and_unique_final_reply() {
        let (send, result) = mpsc::sync_channel(1);
        let progress = Arc::new(Mutex::new(None));
        let ticket = Ticket {
            result,
            progress: progress.clone(),
        };
        for completed in 0..10_000 {
            record_progress(
                &progress,
                ArchiveProgress {
                    phase: ArchivePhase::Archiving,
                    completed,
                    total: None,
                },
            )
            .unwrap();
        }
        assert!(send.try_send(Ok(RpcReply::Validated)).is_ok());
        assert_eq!(ticket.progress().unwrap().unwrap().completed, 9999);
        assert!(ticket.progress().unwrap().is_none());
        assert!(matches!(ticket.poll().unwrap(), Some(RpcReply::Validated)));
        assert!(ticket.poll().unwrap().is_none());
    }
}
