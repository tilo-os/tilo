//! Bounded native clipboard commands on the existing authenticated host lane.
//! Descriptors remain owned until Capture answers. Invocation ids are inert:
//! the broker matches them to its exact retained peer/caller before each command.

use super::FileExplorerWidget;
use crate::i18n::t;
use host_fs::RetainedObject;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use ui::collaboration::{CollabEvent, CollabRequest, file_clipboard};
use ui::tilo_actions::InvocationId;
use ui::time::{Duration, Instant};

use file_clipboard::{Command, Offer, Reply, SelectedDescriptor};

mod transfer;
pub use transfer::Receipt;

struct Publication {
    source_invocation: Option<InvocationId>,
    cut: bool,
    retained: Vec<Arc<RetainedObject>>,
    tickets: Vec<String>,
    published: Option<Offer>,
    failure: Option<String>,
    phase: Phase,
}

struct UndoJob {
    redo: bool,
    source_invocation: Option<InvocationId>,
    operation: String,
    reply: ui::framework::EffectReply,
    expires: Instant,
}

pub(super) struct UndoRequest {
    pub redo: bool,
    pub source_invocation: Option<InvocationId>,
    pub operation: String,
    pub reply: ui::framework::EffectReply,
}

#[derive(Clone, Copy)]
enum Phase {
    Snapshot,
    Recover(uuid::Uuid),
    Capture,
    Publish,
    Release,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Completion {
    Published(Offer),
    Snapshot(Option<Offer>),
    Recovered {
        operations: Vec<String>,
        completed: usize,
        error: Option<String>,
    },
    Transferred(Receipt),
}

#[derive(Default)]
pub(super) struct Client {
    native_service: bool,
    next_request: u64,
    awaiting: Option<u64>,
    request: Option<CollabRequest>,
    publication: Option<Publication>,
    deadline: Option<Instant>,
    transfer: Option<transfer::Transfer>,
    ready: Option<Instant>,
    undo_queue: Rc<RefCell<Vec<UndoRequest>>>,
    undo: Option<UndoJob>,
    diagnostic_ever_active: bool,
    diagnostic_ever_source_present: bool,
}

impl Client {
    pub(super) fn new() -> Self {
        Self {
            native_service: runtime_portal::service_fd("collab").is_some(),
            ..Self::default()
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.publication.is_some() || self.transfer.is_some() || self.undo.is_some()
    }

    pub(super) fn diagnostic_phase(&self) -> (&'static str, bool, bool, bool) {
        if let Some(publication) = &self.publication {
            let phase = match publication.phase {
                Phase::Snapshot => "snapshot",
                Phase::Recover(_) => "recover",
                Phase::Capture => "capture",
                Phase::Publish => "publish",
                Phase::Release => "release",
            };
            return (
                phase,
                publication.source_invocation.is_some(),
                self.diagnostic_ever_active,
                self.diagnostic_ever_source_present,
            );
        }
        if let Some(transfer) = &self.transfer {
            return (
                "transfer",
                transfer.source_invocation.is_some(),
                self.diagnostic_ever_active,
                self.diagnostic_ever_source_present,
            );
        }
        if let Some(undo) = &self.undo {
            return (
                "compensation",
                undo.source_invocation.is_some(),
                self.diagnostic_ever_active,
                self.diagnostic_ever_source_present,
            );
        }
        (
            "idle",
            false,
            self.diagnostic_ever_active,
            self.diagnostic_ever_source_present,
        )
    }

    /// Forget only this caller's publication/transfer. Late request ids remain
    /// distinct, and the host owns ticket expiry and interrupted I/O recovery.
    pub(super) fn abandon(&mut self) {
        self.publication = None;
        self.transfer = None;
        self.awaiting = None;
        self.request = None;
        self.deadline = None;
        self.ready = None;
    }

    fn send(
        &mut self,
        source_invocation: Option<InvocationId>,
        command: Command,
    ) -> Result<(), String> {
        if !command.valid() || self.awaiting.is_some() || self.request.is_some() {
            return Err("invalid or overlapping clipboard command".into());
        }
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or("clipboard request identity exhausted")?;
        self.awaiting = Some(self.next_request);
        self.deadline = Some(ui::time::now() + Duration::from_secs(20));
        self.request = Some(CollabRequest::LocalFileClipboard {
            request_id: self.next_request,
            command,
            on_behalf_of: source_invocation,
        });
        self.diagnostic_ever_active = true;
        self.diagnostic_ever_source_present |= source_invocation.is_some();
        Ok(())
    }

    pub(super) fn publish(
        &mut self,
        source_invocation: Option<InvocationId>,
        cut: bool,
        sources: Vec<(SelectedDescriptor, Arc<RetainedObject>)>,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("another clipboard operation is still running".into());
        }
        let (sources, retained) = sources.into_iter().unzip();
        self.send(
            source_invocation,
            Command::Capture {
                sources,
                destination: None,
            },
        )?;
        self.publication = Some(Publication {
            source_invocation,
            cut,
            retained,
            tickets: Vec::new(),
            published: None,
            failure: None,
            phase: Phase::Capture,
        });
        Ok(())
    }

    pub(super) fn snapshot(
        &mut self,
        source_invocation: Option<InvocationId>,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("another clipboard operation is still running".into());
        }
        self.send(source_invocation, Command::Snapshot)?;
        self.publication = Some(Publication {
            source_invocation,
            cut: false,
            retained: Vec::new(),
            tickets: Vec::new(),
            published: None,
            failure: None,
            phase: Phase::Snapshot,
        });
        Ok(())
    }

    pub(super) fn recover(
        &mut self,
        source_invocation: Option<InvocationId>,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("another clipboard operation is still running".into());
        }
        let operation = uuid::Uuid::new_v4();
        self.send(
            source_invocation,
            Command::Recover {
                operation: operation.to_string(),
            },
        )?;
        self.publication = Some(Publication {
            source_invocation,
            cut: false,
            retained: Vec::new(),
            tickets: Vec::new(),
            published: None,
            failure: None,
            phase: Phase::Recover(operation),
        });
        Ok(())
    }

    pub(super) fn take_request(&mut self) -> Option<CollabRequest> {
        self.start_undo();
        if self.ready.is_some_and(|ready| ui::time::now() < ready) {
            return None;
        }
        self.ready = None;
        self.request.take()
    }

    pub(super) fn next_wake(&self) -> Option<Instant> {
        if !self.busy() && !self.undo_queue.borrow().is_empty() {
            return Some(ui::time::now());
        }
        if self.request.is_some() {
            Some(self.ready.unwrap_or_else(ui::time::now))
        } else {
            self.deadline
        }
    }

    /// An unobservable host reply is a failure, never an optimistic success.
    /// Host tickets independently expire; late replies cannot answer a new job.
    pub(super) fn timeout(&mut self, now: Instant) -> Option<Result<Completion, String>> {
        if self.deadline.is_none_or(|deadline| now < deadline) {
            return None;
        }
        self.deadline = None;
        self.awaiting = None;
        self.request = None;
        if let Some(job) = self.undo.take() {
            if now >= job.expires || job.reply.is_cancelled() {
                job.reply.finish(Err(format!(
                    "clipboard compensation {} could not be observed",
                    job.operation
                )));
            } else if let Err(error) = self.send(
                job.source_invocation,
                if job.redo {
                    Command::Redo {
                        operation: job.operation.clone(),
                    }
                } else {
                    Command::Undo {
                        operation: job.operation.clone(),
                    }
                },
            ) {
                job.reply.finish(Err(error));
            } else {
                self.undo = Some(job);
            }
            return None;
        }
        if let Some(mut transfer) = self.transfer.take() {
            let step = transfer.failed(t("files-clipboard-unavailable"));
            return self.transfer_step(transfer, step);
        }
        let publication = self.publication.take()?;
        self.release_after_failure(publication, t("files-clipboard-unavailable"))
    }

    fn release_after_failure(
        &mut self,
        mut publication: Publication,
        reason: String,
    ) -> Option<Result<Completion, String>> {
        if matches!(publication.phase, Phase::Release) || publication.tickets.is_empty() {
            return Some(Err(reason));
        }
        publication.failure = Some(reason);
        publication.phase = Phase::Release;
        let command = Command::Release {
            tickets: publication.tickets.clone(),
        };
        if let Err(error) = self.send(publication.source_invocation, command) {
            return Some(Err(error));
        }
        self.publication = Some(publication);
        None
    }

    /// Answer only after the host has released its temporary capture tickets.
    /// A release failure cannot be presented as a successful publication.
    pub(super) fn event(&mut self, event: &CollabEvent) -> Option<Result<Completion, String>> {
        let CollabEvent::LocalFileClipboardResult { request_id, result } = event else {
            return None;
        };
        if self.awaiting != Some(*request_id) {
            return None;
        }
        self.awaiting = None;
        self.deadline = None;
        if let Some(job) = self.undo.take() {
            self.undo_answer(job, result);
            return None;
        }
        if let Some(mut transfer) = self.transfer.take() {
            let step = match result {
                Ok(Reply::Rejected { reason }) => transfer.rejected(reason.clone()),
                Err(reason) => transfer.failed(reason.clone()),
                Ok(reply) => transfer.answer(reply),
            };
            return self.transfer_step(transfer, step);
        }
        let mut publication = self.publication.take()?;
        let reply = match result {
            Ok(Reply::Rejected { reason }) | Err(reason) => {
                return self.release_after_failure(publication, reason.clone());
            }
            Ok(reply) => reply,
        };
        let command = match (publication.phase, reply) {
            (
                Phase::Recover(_),
                Reply::Recovered {
                    operations,
                    completed,
                    error,
                },
            ) if operations.len() == *completed && operations.len() <= 256 => {
                return Some(Ok(Completion::Recovered {
                    operations: operations.clone(),
                    completed: *completed,
                    error: error.clone(),
                }));
            }
            (Phase::Recover(id), Reply::Running { operation }) if operation == &id.to_string() => {
                publication.phase = Phase::Recover(id);
                self.ready = Some(ui::time::now() + Duration::from_millis(50));
                Command::Observe {
                    operation: operation.clone(),
                }
            }
            (Phase::Snapshot, Reply::Snapshot { offer }) => {
                return Some(Ok(Completion::Snapshot(offer.clone())));
            }
            (
                Phase::Capture,
                Reply::Captured {
                    sources,
                    destination: None,
                },
            ) if sources.len() == publication.retained.len() => {
                // The acknowledged host tickets now retain the actual objects.
                publication.retained.clear();
                publication.tickets.clone_from(sources);
                publication.phase = Phase::Publish;
                Command::Publish {
                    sources: sources.clone(),
                    cut: publication.cut,
                }
            }
            (Phase::Publish, Reply::Published { offer })
                if offer.count == publication.tickets.len() && offer.cut == publication.cut =>
            {
                publication.published = Some(offer.clone());
                publication.phase = Phase::Release;
                Command::Release {
                    tickets: publication.tickets.clone(),
                }
            }
            (Phase::Release, Reply::Released) => {
                if let Some(reason) = publication.failure {
                    return Some(Err(reason));
                }
                return Some(
                    publication
                        .published
                        .map(Completion::Published)
                        .ok_or_else(|| "clipboard publication absent".into()),
                );
            }
            _ => {
                return self.release_after_failure(
                    publication,
                    "clipboard host reply does not match the pending command".into(),
                );
            }
        };
        if let Err(error) = self.send(publication.source_invocation, command) {
            return Some(Err(error));
        }
        self.publication = Some(publication);
        None
    }

    fn undo_answer(&mut self, job: UndoJob, result: &Result<Reply, String>) {
        match result {
            Ok(Reply::Settled {
                operation,
                error: None,
                undone,
                ..
            }) if operation == &job.operation && *undone != job.redo => {
                job.reply.answer(Ok(ui::tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({"operation": operation})),
                    resources: Vec::new(),
                }));
            }
            Ok(Reply::Running { operation }) if operation == &job.operation => {
                if let Err(error) = self.send(
                    job.source_invocation,
                    Command::Observe {
                        operation: operation.clone(),
                    },
                ) {
                    job.reply.finish(Err(error));
                } else {
                    self.ready = Some(ui::time::now() + Duration::from_millis(50));
                    self.undo = Some(job);
                }
            }
            Ok(Reply::Rejected { reason }) | Err(reason) => {
                job.reply.finish(Err(reason.clone()));
            }
            Ok(Reply::Settled {
                error: Some(reason),
                ..
            }) => job.reply.finish(Err(reason.clone())),
            _ => job.reply.finish(Err(
                "clipboard compensation did not settle its exact receipt".into(),
            )),
        }
    }

    pub(super) fn paste(
        &mut self,
        source_invocation: Option<InvocationId>,
        destination: SelectedDescriptor,
        retained: Arc<RetainedObject>,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("another clipboard operation is still running".into());
        }
        self.send(source_invocation, Command::Snapshot)?;
        self.transfer = Some(transfer::Transfer::new(
            source_invocation,
            destination,
            retained,
        ));
        Ok(())
    }

    pub(super) fn restore(
        &mut self,
        source_invocation: Option<InvocationId>,
        source: SelectedDescriptor,
        retained: Arc<RetainedObject>,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("another clipboard operation is still running".into());
        }
        let transfer = transfer::Transfer::restoring(source_invocation, source, retained);
        self.send(source_invocation, transfer.restore_command())?;
        self.transfer = Some(transfer);
        Ok(())
    }

    fn transfer_step(
        &mut self,
        transfer: transfer::Transfer,
        step: transfer::Step,
    ) -> Option<Result<Completion, String>> {
        match step {
            transfer::Step::Done(result) => Some(result.map(Completion::Transferred)),
            transfer::Step::Send(command) => {
                let paced = matches!(
                    command,
                    Command::Observe { .. } | Command::Paste { .. } | Command::RestoreTrash { .. }
                );
                if let Err(error) = self.send(transfer.source_invocation, command) {
                    return Some(Err(error));
                }
                if paced {
                    self.ready = Some(ui::time::now() + Duration::from_millis(50));
                }
                self.transfer = Some(transfer);
                None
            }
        }
    }

    fn start_undo(&mut self) {
        if self.busy() {
            return;
        }
        let queued = self.undo_queue.borrow_mut().pop();
        let Some(queued) = queued else {
            return;
        };
        if queued.reply.is_cancelled() {
            return;
        }
        match self.send(
            queued.source_invocation,
            if queued.redo {
                Command::Redo {
                    operation: queued.operation.clone(),
                }
            } else {
                Command::Undo {
                    operation: queued.operation.clone(),
                }
            },
        ) {
            Ok(()) => {
                self.undo = Some(UndoJob {
                    redo: queued.redo,
                    source_invocation: queued.source_invocation,
                    operation: queued.operation,
                    reply: queued.reply,
                    expires: ui::time::now() + Duration::from_secs(120),
                });
            }
            Err(error) => queued.reply.finish(Err(error)),
        }
    }
}

fn selected(path: &Path, retained: &RetainedObject) -> Result<SelectedDescriptor, String> {
    let path = host_fs::names::unix_path_bytes(path)
        .ok_or_else(|| t("files-clipboard-source-unavailable"))?
        .to_vec();
    let descriptor = retained
        .descriptor()
        .ok_or_else(|| t("files-clipboard-source-unavailable"))?;
    let observed = retained.observed();
    Ok(SelectedDescriptor {
        path,
        descriptor,
        expected: file_clipboard::ObjectIdentity {
            device: observed.device(),
            inode: observed.inode(),
        },
    })
}

impl FileExplorerWidget {
    pub(crate) fn attach_clipboard_compensation(
        &self,
        cx: &ui::framework::SurfaceCx<crate::resource::FileExplorer>,
    ) {
        let Some(device) = cx.device().map(str::to_owned) else {
            return;
        };
        let queue = self.local_clipboard.undo_queue.clone();
        cx.drain_causal(
            move |state: &crate::resource::FilesState| state.pending_clipboard_for(&device),
            move |(id, operation, undone), cx| {
                cx.invocation()
                    .ok_or("clipboard Undo has no current invocation")?;
                if queue.borrow().len() >= 16 {
                    return Err("clipboard compensation queue is full".into());
                }
                let expected = operation.clone();
                let reply = cx.reply_then(move |(), output, cx| {
                    if output
                        .value
                        .as_ref()
                        .and_then(|value| value.get("operation"))
                        .and_then(serde_json::Value::as_str)
                        != Some(expected.as_str())
                    {
                        return Err(
                            "clipboard Undo receipt differs from its current consequence".into(),
                        );
                    }
                    let settlement = if undone {
                        crate::resource::FileExplorer::SETTLE_CLIPBOARD_REDO
                    } else {
                        crate::resource::FileExplorer::SETTLE_CLIPBOARD_UNDO
                    };
                    cx.invoke(settlement.with((id, expected)));
                    Ok(ui::framework::Effected::done())
                });
                queue.borrow_mut().push(UndoRequest {
                    redo: undone,
                    source_invocation: cx.source_invocation(),
                    operation,
                    reply,
                });
                Ok(ui::framework::Effected::Pending)
            },
        );
    }
    pub(super) fn clipboard_unavailable<A>(
        effect: ui::tilo_resource::ActionRef<crate::effects::FilesEffect, A>,
    ) -> ui::framework::EffectStatus {
        ui::framework::EffectStatus::unavailable(
            effect,
            "clipboard_unavailable",
            t("files-clipboard-unavailable"),
        )
    }

    pub(super) fn publish_clipboard(
        &mut self,
        invocation: Option<InvocationId>,
        source_invocation: Option<InvocationId>,
        cut: bool,
    ) -> Result<super::Ran, String> {
        self.publish_clipboard_for(invocation, source_invocation, cut, None)
    }

    pub(super) fn publish_clipboard_row(
        &mut self,
        invocation: Option<InvocationId>,
        source_invocation: Option<InvocationId>,
        cut: bool,
        path: &Path,
    ) -> Result<super::Ran, String> {
        self.publish_clipboard_for(invocation, source_invocation, cut, Some(path))
    }

    fn publish_clipboard_for(
        &mut self,
        invocation: Option<InvocationId>,
        source_invocation: Option<InvocationId>,
        cut: bool,
        row: Option<&Path>,
    ) -> Result<super::Ran, String> {
        if !self.hosted_clipboard() {
            return Err(t("files-clipboard-unavailable"));
        }
        self.refuse_while_answering()?;
        invocation.ok_or_else(|| t("files-clipboard-unavailable"))?;
        let sources = match row {
            None => self.captured_selection()?,
            Some(path) => {
                let observed = self
                    .entries
                    .iter()
                    .find(|entry| entry.path == path)
                    .and_then(|entry| entry.observed)
                    .ok_or_else(|| t("files-clipboard-source-unavailable"))?;
                let retained = Arc::new(
                    RetainedObject::retain_shown(path, observed)
                        .map_err(|_| t("files-clipboard-source-unavailable"))?,
                );
                vec![(selected(path, &retained)?, retained)]
            }
        };
        self.local_clipboard
            .publish(source_invocation, cut, sources)?;
        self.structure_dirty = true;
        Ok(super::Ran::Pending)
    }

    pub(super) fn snapshot_clipboard(
        &mut self,
        invocation: Option<InvocationId>,
        source_invocation: Option<InvocationId>,
    ) -> Result<super::Ran, String> {
        if !self.hosted_clipboard() {
            return Err(t("files-clipboard-unavailable"));
        }
        self.refuse_while_answering()?;
        invocation.ok_or_else(|| t("files-clipboard-unavailable"))?;
        self.local_clipboard.snapshot(source_invocation)?;
        Ok(super::Ran::Pending)
    }

    pub(super) fn recover_clipboard(
        &mut self,
        invocation: Option<InvocationId>,
        source_invocation: Option<InvocationId>,
    ) -> Result<super::Ran, String> {
        if !self.hosted_clipboard() {
            return Err(t("files-clipboard-unavailable"));
        }
        self.refuse_while_answering()?;
        invocation.ok_or_else(|| t("files-clipboard-unavailable"))?;
        self.local_clipboard.recover(source_invocation)?;
        Ok(super::Ran::Pending)
    }

    pub(crate) fn hosted_clipboard(&self) -> bool {
        match &self.services {
            super::FileServices::Hosted(services) => services.local_clipboard,
            // Select once: a dead service must fail through the native lane,
            // never fall back to queuing legacy pathname operations.
            super::FileServices::Legacy => self.local_clipboard.native_service,
        }
    }

    pub(crate) fn clipboard_device(&self) -> Option<String> {
        self.device.clone()
    }

    pub(crate) fn follow_clipboard_receipt(&mut self, receipt: &Receipt) {
        self.awaiting_ops.insert(
            receipt.operation.clone(),
            super::OpFollowUp::Clipboard {
                completed: receipt.completed,
                restore: receipt.restore,
                error: receipt.error.clone(),
            },
        );
    }

    pub(super) fn can_paste_clipboard(&self) -> bool {
        if matches!(self.services, super::FileServices::Hosted(_)) || self.hosted_clipboard() {
            self.hosted_clipboard() && self.shown_folder.is_some()
        } else {
            !self.clipboard.is_empty()
        }
    }

    pub(super) fn paste_clipboard(
        &mut self,
        invocation: Option<InvocationId>,
        source_invocation: Option<InvocationId>,
    ) -> Result<super::Ran, String> {
        if !self.hosted_clipboard() {
            return Err(t("files-clipboard-unavailable"));
        }
        self.refuse_while_answering()?;
        let retained = self
            .shown_folder
            .clone()
            .ok_or_else(|| t("files-clipboard-source-unavailable"))?;
        let descriptor = selected(&self.current_path, &retained)?;
        invocation.ok_or_else(|| t("files-clipboard-unavailable"))?;
        self.local_clipboard
            .paste(source_invocation, descriptor, retained)?;
        self.structure_dirty = true;
        Ok(super::Ran::Pending)
    }

    pub(super) fn restore_clipboard_entry(
        &mut self,
        invocation: Option<InvocationId>,
        source_invocation: Option<InvocationId>,
        path: &Path,
    ) -> Result<super::Ran, String> {
        if !self.hosted_clipboard() {
            return Err(t("files-clipboard-unavailable"));
        }
        self.refuse_while_answering()?;
        let retained = if let Some((_, retained)) = self.shown_selection.objects.get(path) {
            retained
                .clone()
                .ok_or_else(|| t("files-clipboard-source-changed"))?
        } else {
            let observed = self
                .entries
                .iter()
                .find(|entry| entry.path == path)
                .and_then(|entry| entry.observed)
                .ok_or_else(|| t("files-clipboard-source-unavailable"))?;
            Arc::new(
                RetainedObject::retain_shown(path, observed)
                    .map_err(|_| t("files-clipboard-source-changed"))?,
            )
        };
        invocation.ok_or_else(|| t("files-clipboard-unavailable"))?;
        self.local_clipboard
            .restore(source_invocation, selected(path, &retained)?, retained)?;
        self.structure_dirty = true;
        Ok(super::Ran::Pending)
    }

    pub(super) fn complete_clipboard_publication(&mut self, result: Result<Completion, String>) {
        match result {
            Ok(Completion::Published(offer)) => {
                self.clipboard.clear();
                self.clipboard_cut = offer.cut;
                self.status = Some(crate::i18n::t_args(
                    if offer.cut {
                        "files-cut"
                    } else {
                        "files-copied"
                    },
                    &[crate::i18n::arg("count", super::fluent_count(offer.count))],
                ));
                self.answer_effect(Ok(()));
            }
            Ok(Completion::Snapshot(offer)) => {
                if let Some(reply) = self.effect_reply.take() {
                    let value = offer.map(|offer| serde_json::json!({"id": offer.id, "count": offer.count, "cut": offer.cut}));
                    reply.answer(Ok(ui::tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({"offer": value})),
                        resources: Vec::new(),
                    }));
                }
            }
            Ok(Completion::Recovered {
                operations,
                completed,
                error,
            }) => {
                self.refresh();
                self.status = Some(crate::i18n::t_args(
                    "files-clipboard-recovered",
                    &[crate::i18n::arg("count", super::fluent_count(completed))],
                ));
                if let Some(reply) = self.effect_reply.take() {
                    reply.answer(Ok(ui::tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({"operations": operations, "completed": completed, "error": error})),
                        resources: Vec::new(),
                    }));
                }
            }
            Ok(Completion::Transferred(receipt)) => {
                if let Some(reply) = self.effect_reply.take() {
                    reply.answer(Ok(ui::tilo_actions::ActionOutput {
                        value: Some(serde_json::json!(receipt)),
                        resources: Vec::new(),
                    }));
                }
                self.refresh();
            }
            Err(error) => {
                self.status = Some(error.clone());
                self.answer_effect(Err(ui::tilo_actions::ActionFailure {
                    code: "clipboard_failed".into(),
                    message: error,
                    details: None,
                    retryable: false,
                }));
            }
        }
        self.structure_dirty = true;
    }

    pub(super) fn captured_selection(
        &self,
    ) -> Result<Vec<(SelectedDescriptor, Arc<RetainedObject>)>, String> {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return Err(t("files-selection-empty"));
        }
        if paths.len() > file_clipboard::MAX_ENTRIES {
            return Err(t("files-selection-too-large"));
        }
        paths
            .into_iter()
            .map(|path| {
                let retained = self
                    .shown_selection
                    .objects
                    .get(&path)
                    .and_then(|(_, object)| object.clone())
                    .ok_or_else(|| {
                        t(if self.shown_selection.changed.contains(&path) {
                            "files-clipboard-source-changed"
                        } else {
                            "files-clipboard-source-unavailable"
                        })
                    })?;
                Ok((selected(&path, &retained)?, retained))
            })
            .collect()
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::super::{HostedFileServices, Ran, StartupInputs};
    use super::*;
    use crate::effects::FilesEffect;

    fn files(path: &Path) -> FileExplorerWidget {
        let mut files = FileExplorerWidget::with_startup_inputs_and_services(
            StartupInputs {
                initial_dir: path.into(),
                workspace_roots: vec![path.into()],
                record_roots: Vec::new(),
                remote_accounts: Vec::new(),
                trusted_humans: crate::identity_directory::TrustedHumanDirectory::default(),
            },
            HostedFileServices::deferred_document_open(),
        );
        files.navigate(path.into());
        files
    }

    fn request_for(
        files: &mut FileExplorerWidget,
        source_invocation: Option<InvocationId>,
    ) -> (u64, Command) {
        let CollabRequest::LocalFileClipboard {
            request_id,
            command,
            on_behalf_of,
        } = files
            .local_clipboard
            .take_request()
            .expect("pending native command")
        else {
            panic!("wrong host lane")
        };
        assert_eq!(on_behalf_of, source_invocation);
        assert!(command.valid());
        (request_id, command)
    }

    fn request(files: &mut FileExplorerWidget, invocation: InvocationId) -> (u64, Command) {
        request_for(files, Some(invocation))
    }

    fn reply(
        files: &mut FileExplorerWidget,
        id: u64,
        result: Result<Reply, String>,
    ) -> Option<Result<Completion, String>> {
        files
            .local_clipboard
            .event(&CollabEvent::LocalFileClipboardResult {
                request_id: id,
                result,
            })
    }

    #[test]
    fn ordinary_native_window_uses_service_clipboard_and_retains_original_caller() {
        use std::os::fd::AsRawFd as _;
        let home = tilo_test_env::TestHome::new().unwrap();
        let (service, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_SVC_COLLAB_FD", service.as_raw_fd().to_string());
        env.set(
            "TILO_RUNTIME_CONNECTION_OWNER_PID",
            std::process::id().to_string(),
        );
        env.set("TILO_USER_HOME", home.home());
        let mut files = FileExplorerWidget::new();
        assert!(matches!(files.services, super::super::FileServices::Legacy));
        assert!(files.hosted_clipboard());
        let invocation = InvocationId(uuid::Uuid::new_v4());
        assert!(matches!(
            files
                .run_effect_for(
                    FilesEffect::ClipboardStatus,
                    Some(invocation),
                    Some(invocation),
                )
                .unwrap(),
            Ran::Pending
        ));
        let (id, command) = request(&mut files, invocation);
        assert!(matches!(command, Command::Snapshot));
        assert!(matches!(
            reply(&mut files, id, Ok(Reply::Snapshot { offer: None })),
            Some(Ok(Completion::Snapshot(None)))
        ));
        drop(service);
        assert!(runtime_portal::service_fd("collab").is_none());
        assert!(
            files.hosted_clipboard(),
            "service loss must never select pathname fallback"
        );
        assert!(matches!(
            files
                .run_effect_for(
                    FilesEffect::ClipboardStatus,
                    Some(invocation),
                    Some(invocation),
                )
                .unwrap(),
            Ran::Pending
        ));
        let (_, command) = request(&mut files, invocation);
        assert!(matches!(command, Command::Snapshot));
        assert!(files.clipboard.is_empty());
    }

    #[test]
    fn local_clipboard_effect_keeps_correlation_private_from_host_authority() {
        use std::os::fd::AsRawFd as _;
        let home = tilo_test_env::TestHome::new().unwrap();
        let (service, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_SVC_COLLAB_FD", service.as_raw_fd().to_string());
        env.set(
            "TILO_RUNTIME_CONNECTION_OWNER_PID",
            std::process::id().to_string(),
        );
        env.set("TILO_USER_HOME", home.home());
        let mut files = FileExplorerWidget::new();
        let correlation = InvocationId::new();
        assert!(matches!(
            files
                .run_effect_for(FilesEffect::ClipboardStatus, Some(correlation), None)
                .unwrap(),
            Ran::Pending
        ));
        let (_, command) = request_for(&mut files, None);
        assert!(matches!(command, Command::Snapshot));
    }

    #[test]
    fn native_row_copy_preserves_selection_and_captures_only_the_requested_object() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("requested");
        let other = root.path().join("selected");
        std::fs::write(&target, b"row").unwrap();
        std::fs::write(&other, b"selection").unwrap();
        let mut files = files(root.path());
        files.select_entry(&other).unwrap();
        let entry = files
            .entries
            .iter()
            .find(|entry| entry.path == target)
            .unwrap();
        let row = FileExplorerWidget::entry_resource_id(entry);
        let observed = entry.observed.unwrap();
        let invocation = InvocationId(uuid::Uuid::new_v4());
        assert!(matches!(
            files
                .run_effect_for(
                    FilesEffect::Copy { entry: row },
                    Some(invocation),
                    Some(invocation),
                )
                .unwrap(),
            Ran::Pending
        ));
        assert_eq!(files.selected_paths(), vec![other]);
        let (
            _,
            Command::Capture {
                sources,
                destination,
            },
        ) = request(&mut files, invocation)
        else {
            panic!("row Copy must retain before publishing");
        };
        assert_eq!(sources.len(), 1);
        assert!(destination.is_none());
        assert_eq!(
            sources[0].path,
            host_fs::names::unix_path_bytes(&target).unwrap()
        );
        assert_eq!(sources[0].expected.device, observed.device());
        assert_eq!(sources[0].expected.inode, observed.inode());
    }

    #[test]
    fn native_row_copy_refuses_name_reuse_without_changing_selection() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("requested");
        let other = root.path().join("selected");
        std::fs::write(&target, b"shown").unwrap();
        std::fs::write(&other, b"selection").unwrap();
        let mut files = files(root.path());
        files.select_entry(&other).unwrap();
        let entry = files
            .entries
            .iter()
            .find(|entry| entry.path == target)
            .unwrap();
        let row = FileExplorerWidget::entry_resource_id(entry);
        std::fs::rename(&target, root.path().join("old")).unwrap();
        std::fs::write(&target, b"replacement").unwrap();
        let invocation = InvocationId(uuid::Uuid::new_v4());
        assert!(
            files
                .run_effect_for(
                    FilesEffect::Copy { entry: row },
                    Some(invocation),
                    Some(invocation),
                )
                .is_err()
        );
        assert_eq!(files.selected_paths(), vec![other]);
        assert!(files.local_clipboard.take_request().is_none());
        assert_eq!(std::fs::read(target).unwrap(), b"replacement");
    }

    struct CapturedSelectedCopy {
        _root: tempfile::TempDir,
        _destination: tempfile::TempDir,
        files: FileExplorerWidget,
        retained: Arc<RetainedObject>,
        invocation: InvocationId,
        ticket: String,
    }

    fn captured_selected_copy() -> CapturedSelectedCopy {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("selected");
        std::fs::write(&target, b"original").unwrap();
        let mut files = files(root.path());
        files.run_effect(FilesEffect::SelectAll).unwrap();
        let retained = files.shown_selection.objects[&target].1.clone().unwrap();
        let original = retained.observed();
        std::fs::rename(&target, root.path().join("old")).unwrap();
        std::fs::write(&target, b"replacement").unwrap();
        let invocation = InvocationId(uuid::Uuid::new_v4());
        assert!(matches!(
            files
                .run_effect_for(
                    FilesEffect::CopySelection,
                    Some(invocation),
                    Some(invocation),
                )
                .unwrap(),
            Ran::Pending
        ));
        let (capture_id, command) = request(&mut files, invocation);
        let Command::Capture {
            sources,
            destination,
        } = command
        else {
            panic!("capture first")
        };
        assert_eq!(sources.len(), 1);
        assert!(destination.is_none());
        assert_eq!(sources[0].descriptor, retained.descriptor().unwrap());
        assert_eq!(sources[0].expected.device, original.device());
        assert_eq!(sources[0].expected.inode, original.inode());
        assert_eq!(
            sources[0].path,
            host_fs::names::unix_path_bytes(&target).unwrap()
        );
        let destination = tempfile::tempdir().unwrap();
        files.navigate(destination.path().into());
        assert_eq!(
            Arc::strong_count(&retained),
            2,
            "job holds the source after navigation"
        );
        let ticket = uuid::Uuid::new_v4().to_string();
        assert!(reply(&mut files, capture_id + 1, Ok(Reply::Released)).is_none());
        assert!(files.local_clipboard.busy());
        assert!(
            reply(
                &mut files,
                capture_id,
                Ok(Reply::Captured {
                    sources: vec![ticket.clone()],
                    destination: None
                })
            )
            .is_none()
        );
        CapturedSelectedCopy {
            _root: root,
            _destination: destination,
            files,
            retained,
            invocation,
            ticket,
        }
    }

    #[test]
    fn selected_native_copy_keeps_its_descriptor_until_capture_and_waits_for_release() {
        let CapturedSelectedCopy {
            _root,
            _destination,
            mut files,
            retained,
            invocation,
            ticket,
        } = captured_selected_copy();
        assert_eq!(
            Arc::strong_count(&retained),
            1,
            "host capture acknowledgement releases the local loan"
        );
        let (publish_id, command) = request(&mut files, invocation);
        assert_eq!(
            command,
            Command::Publish {
                sources: vec![ticket.clone()],
                cut: false
            }
        );
        let offer = Offer {
            id: uuid::Uuid::new_v4().to_string(),
            count: 1,
            cut: false,
        };
        assert!(
            reply(
                &mut files,
                publish_id,
                Ok(Reply::Published {
                    offer: offer.clone()
                })
            )
            .is_none()
        );
        let (release_id, command) = request(&mut files, invocation);
        assert_eq!(
            command,
            Command::Release {
                tickets: vec![ticket]
            }
        );
        assert_eq!(
            reply(&mut files, release_id, Ok(Reply::Released)),
            Some(Ok(Completion::Published(offer)))
        );
        assert!(!files.local_clipboard.busy());
        assert!(
            files.clipboard.is_empty(),
            "native publication never fills the legacy path clipboard"
        );
    }

    #[test]
    fn native_copy_refuses_stale_selection_missing_caller_and_host_failure_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("selected");
        std::fs::write(&target, b"shown").unwrap();
        let mut files = files(root.path());
        std::fs::rename(&target, root.path().join("old")).unwrap();
        std::fs::write(&target, b"replacement").unwrap();
        files.run_effect(FilesEffect::SelectAll).unwrap();
        let invocation = InvocationId(uuid::Uuid::new_v4());
        assert!(
            files
                .run_effect_for(
                    FilesEffect::CopySelection,
                    Some(invocation),
                    Some(invocation),
                )
                .is_err()
        );
        assert!(files.local_clipboard.take_request().is_none());
        files.run_effect(FilesEffect::ClearSelection).unwrap();
        files.refresh();
        files.run_effect(FilesEffect::SelectAll).unwrap();
        assert!(files.run_effect(FilesEffect::CopySelection).is_err());
        assert!(files.local_clipboard.take_request().is_none());
        assert!(matches!(
            files
                .run_effect_for(
                    FilesEffect::CutSelection,
                    Some(invocation),
                    Some(invocation),
                )
                .unwrap(),
            Ran::Pending
        ));
        let (id, _) = request(&mut files, invocation);
        assert_eq!(
            reply(&mut files, id, Err("source grant revoked".into())),
            Some(Err("source grant revoked".into()))
        );
        assert!(files.clipboard.is_empty());
        assert!(!files.local_clipboard.busy());
        assert!(files.local_clipboard.take_request().is_none());
    }

    #[test]
    fn native_copy_timeout_refuses_success_and_cannot_retarget_a_late_reply() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("selected"), b"shown").unwrap();
        let mut files = files(root.path());
        files.run_effect(FilesEffect::SelectAll).unwrap();
        let invocation = InvocationId(uuid::Uuid::new_v4());
        files
            .run_effect_for(
                FilesEffect::CopySelection,
                Some(invocation),
                Some(invocation),
            )
            .unwrap();
        let (id, _) = request(&mut files, invocation);
        assert!(files.local_clipboard.timeout(ui::time::now()).is_none());
        assert!(matches!(
            files
                .local_clipboard
                .timeout(ui::time::now() + Duration::from_secs(21)),
            Some(Err(_))
        ));
        assert!(
            reply(
                &mut files,
                id,
                Ok(Reply::Captured {
                    sources: vec![uuid::Uuid::new_v4().to_string()],
                    destination: None
                })
            )
            .is_none()
        );
        assert!(!files.local_clipboard.busy());
        assert!(files.local_clipboard.take_request().is_none());
    }

    #[test]
    fn native_publication_failure_releases_captured_tickets_before_answering() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("selected"), b"shown").unwrap();
        let mut files = files(root.path());
        files.run_effect(FilesEffect::SelectAll).unwrap();
        let invocation = InvocationId::new();
        files
            .run_effect_for(
                FilesEffect::CutSelection,
                Some(invocation),
                Some(invocation),
            )
            .unwrap();
        let (capture, _) = request(&mut files, invocation);
        let ticket = uuid::Uuid::new_v4().to_string();
        assert!(
            reply(
                &mut files,
                capture,
                Ok(Reply::Captured {
                    sources: vec![ticket.clone()],
                    destination: None
                })
            )
            .is_none()
        );
        let (publish, command) = request(&mut files, invocation);
        assert_eq!(
            command,
            Command::Publish {
                sources: vec![ticket.clone()],
                cut: true
            }
        );
        assert!(
            reply(
                &mut files,
                publish,
                Err("Cut requires a current write grant".into())
            )
            .is_none()
        );
        let (release, command) = request(&mut files, invocation);
        assert_eq!(
            command,
            Command::Release {
                tickets: vec![ticket]
            }
        );
        assert_eq!(
            reply(&mut files, release, Ok(Reply::Released)),
            Some(Err("Cut requires a current write grant".into()))
        );
        assert!(files.clipboard.is_empty());
        assert!(!files.local_clipboard.busy());
    }

    #[test]
    fn native_clipboard_inspection_uses_its_current_caller_and_absence_never_falls_back() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("selected"), b"shown").unwrap();
        let mut files = files(root.path());
        let invocation = InvocationId::new();
        assert!(matches!(
            files
                .run_effect_for(
                    FilesEffect::ClipboardStatus,
                    Some(invocation),
                    Some(invocation),
                )
                .unwrap(),
            Ran::Pending
        ));
        let (id, command) = request(&mut files, invocation);
        assert_eq!(command, Command::Snapshot);
        assert_eq!(
            reply(&mut files, id, Ok(Reply::Snapshot { offer: None })),
            Some(Ok(Completion::Snapshot(None)))
        );
        let super::super::FileServices::Hosted(services) = &mut files.services else {
            panic!("host services")
        };
        services.local_clipboard = false;
        files.run_effect(FilesEffect::SelectAll).unwrap();
        assert!(
            files
                .run_effect_for(
                    FilesEffect::CopySelection,
                    Some(invocation),
                    Some(invocation),
                )
                .is_err()
        );
        assert!(files.local_clipboard.take_request().is_none());
        assert!(files.clipboard.is_empty());
    }
}
