//! One transfer keeps its receiving descriptor and operation id across replies.

use super::{Arc, Command, Duration, Instant, Offer, Reply, RetainedObject, SelectedDescriptor};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub operation: String,
    pub completed: usize,
    pub cut: bool,
    #[serde(default)]
    pub restore: bool,
    pub error: Option<String>,
}

enum Phase {
    Snapshot,
    Restore,
    Capture,
    Paste,
    Observe,
    Release,
}

pub(super) enum Step {
    Send(Command),
    Done(Result<Receipt, String>),
}

pub(super) struct Transfer {
    pub invocation: ui::tilo_actions::InvocationId,
    selected: SelectedDescriptor,
    retained: Option<Arc<RetainedObject>>,
    offer: Option<Offer>,
    destination: Option<String>,
    operation: String,
    receipt: Option<Receipt>,
    phase: Phase,
    restore: bool,
    expires: Instant,
}

impl Transfer {
    pub fn new(
        invocation: ui::tilo_actions::InvocationId,
        selected: SelectedDescriptor,
        retained: Arc<RetainedObject>,
    ) -> Self {
        Self {
            invocation,
            selected,
            retained: Some(retained),
            offer: None,
            destination: None,
            operation: uuid::Uuid::new_v4().to_string(),
            receipt: None,
            phase: Phase::Snapshot,
            restore: false,
            expires: ui::time::now() + Duration::from_secs(120),
        }
    }

    pub(super) fn restoring(
        invocation: ui::tilo_actions::InvocationId,
        selected: SelectedDescriptor,
        retained: Arc<RetainedObject>,
    ) -> Self {
        let mut transfer = Self::new(invocation, selected, retained);
        transfer.restore = true;
        transfer.phase = Phase::Restore;
        transfer.offer = Some(Offer {
            id: transfer.operation.clone(),
            count: 1,
            cut: true,
        });
        transfer
    }

    pub(super) fn restore_command(&self) -> Command {
        Command::RestoreTrash {
            source: self.selected.clone(),
            operation: self.operation.clone(),
        }
    }

    fn paste(&self) -> Command {
        Command::Paste {
            offer: self
                .offer
                .as_ref()
                .map_or_else(String::new, |offer| offer.id.clone()),
            destination: self.destination.clone().unwrap_or_default(),
            operation: self.operation.clone(),
        }
    }

    pub(super) fn rejected(&mut self, reason: String) -> Step {
        if matches!(self.phase, Phase::Restore) {
            Step::Done(Err(reason))
        } else {
            self.failed(reason)
        }
    }

    pub fn failed(&mut self, reason: String) -> Step {
        if let Some(mut receipt) = self.receipt.take() {
            receipt.error.get_or_insert(reason);
            return Step::Done(Ok(receipt));
        }
        if ui::time::now() >= self.expires {
            return Step::Done(Err(format!(
                "{reason}; transfer receipt {} must be observed before retrying",
                self.operation
            )));
        }
        match self.phase {
            // Repeat the same fingerprint, never generate another operation id.
            Phase::Paste => Step::Send(self.paste()),
            Phase::Restore => Step::Send(self.restore_command()),
            Phase::Observe => Step::Send(Command::Observe {
                operation: self.operation.clone(),
            }),
            _ => Step::Done(Err(reason)),
        }
    }

    pub fn answer(&mut self, reply: &Reply) -> Step {
        match (&self.phase, reply) {
            (Phase::Snapshot, Reply::Snapshot { offer: Some(offer) })
                if offer.count > 0 && offer.count <= super::file_clipboard::MAX_ENTRIES =>
            {
                self.offer = Some(offer.clone());
                self.phase = Phase::Capture;
                Step::Send(Command::Capture {
                    sources: Vec::new(),
                    destination: Some(self.selected.clone()),
                })
            }
            (Phase::Snapshot, Reply::Snapshot { offer: None }) => {
                Step::Done(Err(crate::i18n::t("files-clipboard-empty")))
            }
            (
                Phase::Capture,
                Reply::Captured {
                    sources,
                    destination: Some(destination),
                },
            ) if sources.is_empty() => {
                self.retained = None;
                self.destination = Some(destination.clone());
                self.phase = Phase::Paste;
                Step::Send(self.paste())
            }
            (Phase::Restore | Phase::Paste | Phase::Observe, Reply::Running { operation })
                if operation == &self.operation =>
            {
                self.retained = None;
                self.phase = Phase::Observe;
                Step::Send(Command::Observe {
                    operation: self.operation.clone(),
                })
            }
            (
                Phase::Restore | Phase::Paste | Phase::Observe,
                Reply::Settled {
                    operation,
                    completed,
                    error,
                    undone: false,
                },
            ) if operation == &self.operation
                && self
                    .offer
                    .as_ref()
                    .is_some_and(|offer| *completed <= offer.count) =>
            {
                self.receipt = Some(Receipt {
                    operation: operation.clone(),
                    completed: *completed,
                    cut: self.offer.as_ref().is_some_and(|offer| offer.cut),
                    restore: self.restore,
                    error: error.clone(),
                });
                self.retained = None;
                if self.restore {
                    return Step::Done(
                        self.receipt
                            .take()
                            .ok_or_else(|| "settled Restore receipt absent".into()),
                    );
                }
                self.phase = Phase::Release;
                Step::Send(Command::Release {
                    tickets: self.destination.iter().cloned().collect(),
                })
            }
            (Phase::Release, Reply::Released) => Step::Done(
                self.receipt
                    .take()
                    .ok_or_else(|| "settled transfer receipt absent".into()),
            ),
            _ => self.failed("clipboard host reply does not match the pending transfer".into()),
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Transfer, Arc<RetainedObject>) {
        let directory = tempfile::tempdir().unwrap();
        let retained = crate::files_app::shown_selection::folder(directory.path()).unwrap();
        let selected = super::super::selected(directory.path(), &retained).unwrap();
        let transfer = Transfer::new(
            ui::tilo_actions::InvocationId::new(),
            selected,
            retained.clone(),
        );
        (directory, transfer, retained)
    }

    fn command(step: Step) -> Command {
        let Step::Send(command) = step else {
            panic!("transfer unexpectedly finished")
        };
        command
    }

    #[test]
    fn paste_holds_receiving_object_and_retries_the_same_operation_until_receipted() {
        let (_directory, mut transfer, retained) = fixture();
        let offer = Offer {
            id: uuid::Uuid::new_v4().to_string(),
            count: 2,
            cut: true,
        };
        let capture = command(transfer.answer(&Reply::Snapshot {
            offer: Some(offer.clone()),
        }));
        let Command::Capture {
            sources,
            destination: Some(destination),
        } = capture
        else {
            panic!("must capture receiving folder")
        };
        assert!(sources.is_empty());
        assert_eq!(destination.descriptor, retained.descriptor().unwrap());
        assert_eq!(Arc::strong_count(&retained), 2);
        let ticket = uuid::Uuid::new_v4().to_string();
        let paste = command(transfer.answer(&Reply::Captured {
            sources: Vec::new(),
            destination: Some(ticket.clone()),
        }));
        assert_eq!(Arc::strong_count(&retained), 1);
        let Command::Paste {
            offer: actual_offer,
            destination,
            operation,
        } = &paste
        else {
            panic!("must paste exact captured objects")
        };
        assert_eq!(actual_offer, &offer.id);
        assert_eq!(destination, &ticket);
        assert_eq!(
            command(transfer.failed("lost acknowledgement".into())),
            paste
        );
        let observe = command(transfer.answer(&Reply::Running {
            operation: operation.clone(),
        }));
        assert_eq!(
            observe,
            Command::Observe {
                operation: operation.clone()
            }
        );
        assert_eq!(command(transfer.failed("lost observation".into())), observe);
        let release = command(transfer.answer(&Reply::Settled {
            operation: operation.clone(),
            completed: 1,
            error: Some("second entry refused".into()),
            undone: false,
        }));
        assert_eq!(
            release,
            Command::Release {
                tickets: vec![ticket]
            }
        );
        let Step::Done(Ok(receipt)) = transfer.answer(&Reply::Released) else {
            panic!("receipt must settle after release")
        };
        assert_eq!(receipt.operation, *operation);
        assert_eq!(receipt.completed, 1);
        assert!(receipt.cut);
        assert_eq!(receipt.error.as_deref(), Some("second entry refused"));
    }

    #[test]
    fn unobserved_or_mismatched_transfer_never_claims_a_successful_receipt() {
        let (_directory, mut transfer, _) = fixture();
        assert!(matches!(
            transfer.answer(&Reply::Released),
            Step::Done(Err(_))
        ));
        let (_directory, mut transfer, _) = fixture();
        transfer.phase = Phase::Observe;
        let original = transfer.operation.clone();
        assert_eq!(
            command(transfer.answer(&Reply::Settled {
                operation: uuid::Uuid::new_v4().to_string(),
                completed: 1,
                error: None,
                undone: false
            })),
            Command::Observe {
                operation: original.clone()
            }
        );
        transfer.expires = ui::time::now();
        let Step::Done(Err(error)) = transfer.failed("host cannot be observed".into()) else {
            panic!("unobserved transfer must fail")
        };
        assert!(error.contains(&original));
    }
}
