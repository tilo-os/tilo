//! Caller authority and UI receipt adapter for the platform editor capability.
pub(crate) use native::Socket;
use native::{Failure, Lease, Request, Status};
use std::sync::{Arc, Mutex};
use tilo_process::editor as native;
use ui::framework::EffectReply;
use ui::text_input::{Receipt, Resolver};
pub(crate) struct Capture {
    pub lease: Lease,
    epoch: u64,
    active: Receipt,
}
fn ime_failure(failure: Failure) -> ui::text_input::Failure {
    use ui::text_input::Failure as Ime;
    match failure {
        Failure::Rejected => Ime::Rejected,
        Failure::Busy => Ime::Busy,
        Failure::Retired => Ime::Retired,
        Failure::Unknown => Ime::Unknown,
    }
}
enum Answer {
    Ime {
        receipt: Receipt,
        resolver: Resolver,
        capture: Receipt,
    },
    Effect(Box<EffectReply>),
}
impl native::Completion for Answer {
    fn send(&self, send: &mut dyn FnMut() -> std::io::Result<()>) -> Result<(), Failure> {
        match self {
            Self::Ime {
                receipt, capture, ..
            } => {
                if receipt.was_canceled() || capture.was_canceled() || receipt.poll().is_some() {
                    return Err(Failure::Rejected);
                }
                send().map_err(|_| Failure::Unknown)
            }
            Self::Effect(reply) => reply
                .progress()
                .with_effect(|| Ok(send()))
                .map_err(|_| Failure::Rejected)?
                .map_err(|_| Failure::Unknown),
        }
    }
    fn finish(self: Box<Self>, result: Result<(), Failure>) {
        match *self {
            Self::Ime { resolver, .. } => {
                resolver.complete(result.map(|()| None).map_err(ime_failure));
            }
            Self::Effect(reply) => {
                (*reply).answer(
                    result
                        .map(|()| ui::tilo_actions::ActionOutput::default())
                        .map_err(|failure| ui::tilo_actions::ActionFailure {
                            code: if failure == Failure::Unknown {
                                "native_edit_uncertain"
                            } else {
                                "native_edit_refused"
                            }
                            .into(),
                            message: if failure == Failure::Unknown {
                                "The original editor did not acknowledge this edit"
                            } else {
                                "The original editor lease is no longer available"
                            }
                            .into(),
                            details: None,
                            retryable: false,
                        }),
                );
            }
        }
    }
}

pub(crate) struct Client(native::Client);
impl Client {
    pub fn new(socket: Socket, waker: Arc<Mutex<ui::WakeHandle>>) -> std::io::Result<Self> {
        native::Client::new(
            socket,
            Arc::new(move || crate::inner::request_terminal_wake(&waker)),
        )
        .map(Self)
    }
    pub fn snapshot(&self) -> Option<native::Snapshot> {
        self.0.snapshot()
    }
    pub fn revoke(&self) {
        self.0.revoke();
    }
    pub fn capture(&self) -> Option<(Capture, Receipt)> {
        let snapshot = self.snapshot()?;
        let (active, resolver) = Receipt::channel();
        let capture = Capture {
            lease: snapshot.lease,
            epoch: self.0.epoch(),
            active: active.clone(),
        };
        resolver.complete(Ok(None));
        Some((capture, active))
    }
    pub fn commit(&self, capture: Capture, text: String) -> Receipt {
        let (receipt, resolver) = Receipt::channel();
        self.0.submit(
            capture.epoch,
            Request::Replace {
                lease: capture.lease,
                transaction: 1,
                text,
            },
            Status::Applied,
            Box::new(Answer::Ime {
                receipt: receipt.clone(),
                resolver,
                capture: capture.active,
            }),
        );
        receipt
    }
    pub fn effect(&self, operation: Request, expected: Status, reply: EffectReply) {
        self.0.submit(
            self.0.epoch(),
            operation,
            expected,
            Box::new(Answer::Effect(Box::new(reply))),
        );
    }
    pub fn history(&self, direction: ui::undo::UndoRequest) -> Receipt {
        let Some(snapshot) = self.snapshot() else {
            return Receipt::failed(ime_failure(Failure::Rejected));
        };
        let (allowed, operation, expected) = match direction {
            ui::undo::UndoRequest::Undo => (
                snapshot.can_undo,
                Request::Undo {
                    lease: snapshot.lease,
                    transaction: snapshot.transaction,
                },
                Status::Undone,
            ),
            ui::undo::UndoRequest::Redo => (
                snapshot.can_redo,
                Request::Redo {
                    lease: snapshot.lease,
                    transaction: snapshot.transaction,
                },
                Status::Redone,
            ),
        };
        if !allowed {
            return Receipt::failed(ime_failure(Failure::Rejected));
        }
        let (receipt, resolver) = Receipt::channel();
        let (capture, acknowledged) = Receipt::channel();
        acknowledged.complete(Ok(None));
        self.0.submit(
            self.0.epoch(),
            operation,
            expected,
            Box::new(Answer::Ime {
                receipt: receipt.clone(),
                resolver,
                capture,
            }),
        );
        receipt
    }
}
