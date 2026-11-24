//! A retained component's local media queue for in-process hosts. Requests retain their exact component
//! lifetime; a reply cannot reach a replacement.
use super::FrameHarness;
use crate::bound_dispatch::{CallbackBinding, ComponentRequest};
use crate::collaboration::{CollabEvent, CollabRequest};
use crate::native_media::{Request, Snapshot};
use std::collections::BTreeMap;

#[derive(Clone, serde::Serialize)]
pub struct MediaRequest {
    pub ticket: u64,
    pub owner: String,
    pub retired: bool,
    pub app: String,
    pub instance: String,
    pub workspace: String,
    pub record: String,
    pub command: Request,
}

#[derive(Default)]
pub struct MediaQueue {
    next: u64,
    pending: BTreeMap<u64, (CallbackBinding, u64)>,
    owners: BTreeMap<String, (CallbackBinding, MediaRequest)>,
    ready: Vec<MediaRequest>,
    catalogues: Vec<(CallbackBinding, tilo_actions::remote::RemoteActionMessage)>,
    /// What the components asked to read of other apps' documents, kept for a holder that
    /// answers such reads from the documents it holds itself ([`Self::take_resource_reads`]).
    reads: Vec<(CallbackBinding, tilo_actions::remote::RemoteActionMessage)>,
    /// Whether the holder keeps no other app's document: a read of one is answered so as it is
    /// asked ([`Self::refuse_resource_reads`]).
    reads_refused: bool,
}

/// How many reads of other apps' documents wait for the holder at once; one asked beyond that is
/// answered that it cannot be read now, and its reader asks again.
const MAX_WAITING_READS: usize = 64;

impl MediaQueue {
    /// Take local media commands from each retained component under its bound callback context.
    pub fn take(&mut self, frame: &mut FrameHarness) -> Vec<MediaRequest> {
        self.pump(frame);
        std::mem::take(&mut self.ready)
    }

    /// Drain read-only catalogue requests without stealing pending media commands. The transport must
    /// authenticate them; closed component owners are discarded.
    pub fn take_catalogue_requests(
        &mut self,
        frame: &mut FrameHarness,
    ) -> Vec<tilo_actions::remote::RemoteActionMessage> {
        self.pump(frame);
        std::mem::take(&mut self.catalogues)
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    /// Drain what the components asked to read of other apps' documents, as the last call that
    /// took their requests kept it ([`Self::take`], [`Self::take_catalogue_requests`]): a holder
    /// takes those every frame, and this visits no component again. A holder that keeps the
    /// documents itself answers each read as its person may read it, and hands the answer back
    /// as a collaboration event; the transport takes none of them.
    pub fn take_resource_reads(&mut self) -> Vec<tilo_actions::remote::RemoteActionMessage> {
        std::mem::take(&mut self.reads)
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    /// Answer every read of another app's document, as it is asked, that it cannot be read
    /// here: for a holder that keeps no such document and reaches nobody who does.
    pub fn refuse_resource_reads(&mut self) {
        self.reads_refused = true;
    }

    fn pump(&mut self, frame: &mut FrameHarness) {
        let requests = &mut self.ready;
        self.owners.retain(|_, (binding, original)| {
            if binding.closing.get() || !binding.lifetime.is_active() {
                let mut retirement = original.clone();
                retirement.ticket = 0;
                retirement.retired = true;
                retirement.command = Request::new(crate::native_media::Command::Leave);
                requests.push(retirement);
                false
            } else {
                true
            }
        });
        self.pending
            .retain(|_, (binding, _)| !binding.closing.get() && binding.lifetime.is_active());
        requests.retain(|request| request.retired || self.pending.contains_key(&request.ticket));
        self.catalogues
            .retain(|(binding, _)| !binding.closing.get() && binding.lifetime.is_active());
        self.reads
            .retain(|(binding, _)| !binding.closing.get() && binding.lifetime.is_active());
        let mut took = false;
        let reads_refused = self.reads_refused;
        let mut visit = |binding: &CallbackBinding,
                         widget: &mut dyn crate::widget::AppWidget,
                         _: &tilo_actions::ActionRegistry| {
            // Every app may ask the workspace's host to run an action; only
            // Chat holds a call, so only it reaches the media queue.
            let media = binding.app_id == "so.tilo.chat";
            let route = binding.broker_route()?;
            for request in crate::widget::take_collab_requests(widget) {
                took = true;
                if let CollabRequest::NativeMediaCommand {
                    request_id,
                    command,
                } = request
                {
                    let stop = matches!(
                        command.command,
                        crate::native_media::Command::Leave
                            | crate::native_media::Command::SetMuted { muted: true }
                            | crate::native_media::Command::SetDeafened { deafened: true }
                    );
                    if !media || (!stop && self.pending.len() >= 16) || !command.valid() {
                        crate::widget::handle_collab_event(
                            widget,
                            &CollabEvent::NativeMediaCommandResult {
                                request_id,
                                result: Err("local media queue is unavailable".into()),
                            },
                        );
                        continue;
                    }
                    let Some(ticket) = self.next.checked_add(1) else {
                        continue;
                    };
                    self.next = ticket;
                    self.pending.insert(ticket, (binding.clone(), request_id));
                    let request = MediaRequest {
                        ticket,
                        owner: binding.lifetime_id.to_string(),
                        retired: false,
                        app: route.app_id.clone(),
                        instance: binding.instance_id.to_string(),
                        workspace: route.cluster_id.to_string(),
                        record: route.document.clone().unwrap_or_default(),
                        command,
                    };
                    self.owners
                        .insert(request.owner.clone(), (binding.clone(), request.clone()));
                    requests.push(request);
                } else if let CollabRequest::RemoteAction { message } = request {
                    if matches!(
                        message,
                        tilo_actions::remote::RemoteActionMessage::ResourceRead { .. }
                    ) {
                        queue_read(&mut self.reads, reads_refused, binding, widget, message);
                    } else {
                        queue_catalogue(&mut self.catalogues, binding, widget, message);
                    }
                }
            }
            None
        };
        let _ = crate::bound_dispatch::visit_components(
            &mut *frame.root,
            &mut ComponentRequest::new(None, &mut visit),
        );
        if took {
            frame.invalidate_local();
        }
    }

    /// Complete only the exact retained owner that asked.
    pub fn complete(
        &mut self,
        frame: &mut FrameHarness,
        ticket: u64,
        result: Result<Snapshot, String>,
    ) -> bool {
        let Some((binding, request_id)) = self.pending.remove(&ticket) else {
            return false;
        };
        if binding.enter().is_err() {
            return false;
        }
        let event = CollabEvent::NativeMediaCommandResult { request_id, result };
        let mut visit = |_: &CallbackBinding,
                         widget: &mut dyn crate::widget::AppWidget,
                         _: &tilo_actions::ActionRegistry| {
            crate::widget::handle_collab_event(widget, &event);
            None
        };
        let mut request = ComponentRequest::new(Some(&binding), &mut visit);
        let _ = crate::bound_dispatch::visit_components(&mut *frame.root, &mut request);
        frame.relayout();
        request.found
    }
}

/// Whether the workspace's host runs `provider`'s actions: a workspace record's
/// (`record:<app>:<record>`), the workspace's own (`workspace:<workspace>`) and its governance
/// resource's (`workspace.permissions:<workspace>`).
fn host_runs(provider: &str) -> bool {
    provider.starts_with("record:")
        || tilo_actions::remote::provider_workspace(provider).is_some()
        || provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
}

/// Why the media queue refuses an invocation of `provider` with `queued` already waiting for the host, if it
/// does.
fn refusal(provider: &str, queued: usize) -> Option<&'static str> {
    if !host_runs(provider) {
        Some("Only the workspace's host runs another app's actions here")
    } else if queued >= 16 {
        Some("The workspace's host is busy; try again")
    } else {
        None
    }
}

/// Why the queue keeps no further read of another app's document while `waiting` of them wait
/// for the holder, if it keeps none: the holder answers no such read (`refused`), or too many
/// wait already.
fn read_refusal(refused: bool, waiting: usize) -> Option<&'static str> {
    if refused {
        Some("another app's data cannot be read here")
    } else if waiting >= MAX_WAITING_READS {
        Some("too many reads are waiting here")
    } else {
        None
    }
}

/// Keep `message`, a read of another app's document, for the holder. One the queue does not
/// keep ([`read_refusal`]) is answered that it cannot be read.
fn queue_read(
    reads: &mut Vec<(CallbackBinding, tilo_actions::remote::RemoteActionMessage)>,
    refused: bool,
    binding: &CallbackBinding,
    widget: &mut dyn crate::widget::AppWidget,
    message: tilo_actions::remote::RemoteActionMessage,
) {
    use tilo_actions::remote::{ActionCallError, RemoteActionMessage};
    let Some(reason) = read_refusal(refused, reads.len()) else {
        reads.push((binding.clone(), message));
        return;
    };
    let RemoteActionMessage::ResourceRead { correlation, .. } = message else {
        return;
    };
    crate::widget::handle_collab_event(
        widget,
        &CollabEvent::RemoteAction {
            message: RemoteActionMessage::ResourceReading {
                correlation,
                result: Err(ActionCallError::Unavailable {
                    reason: reason.into(),
                }),
            },
        },
    );
}

fn queue_catalogue(
    catalogues: &mut Vec<(CallbackBinding, tilo_actions::remote::RemoteActionMessage)>,
    binding: &CallbackBinding,
    widget: &mut dyn crate::widget::AppWidget,
    message: tilo_actions::remote::RemoteActionMessage,
) {
    use tilo_actions::remote::{
        ActionCallError, InvocationReceipt, ReceiptStatus, RemoteActionMessage,
    };
    // An action the workspace's host runs travels this media queue to it, and the host
    // admits it by the person's own member access in the workspace: a page cannot
    // grant itself anything. The workspace's membership actions become the
    // host's governed workspace requests, decided by the workspace's policy there.
    if let RemoteActionMessage::Invoke(request) = &message {
        if let Some(reason) = refusal(&request.target.provider, catalogues.len()) {
            crate::widget::handle_collab_event(
                widget,
                &CollabEvent::RemoteAction {
                    message: RemoteActionMessage::Receipt(InvocationReceipt {
                        invocation: request.invocation,
                        status: ReceiptStatus::Rejected {
                            error: ActionCallError::Unavailable {
                                reason: reason.into(),
                            },
                        },
                    }),
                },
            );
        } else {
            catalogues.push((binding.clone(), message));
        }
        return;
    }
    let RemoteActionMessage::CatalogueRequest { correlation, .. } = &message else {
        return;
    };
    if catalogues.len() < 16 {
        catalogues.push((binding.clone(), message));
    } else {
        crate::widget::handle_collab_event(
            widget,
            &CollabEvent::RemoteAction {
                message: RemoteActionMessage::CatalogueResponse {
                    correlation: *correlation,
                    result: Err(ActionCallError::Unavailable {
                        reason: "local catalogue media queue is busy".into(),
                    }),
                },
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_WAITING_READS, read_refusal, refusal};

    #[test]
    fn a_holder_keeps_sixty_four_reads_waiting_and_none_where_it_answers_none() {
        assert_eq!(MAX_WAITING_READS, 64);
        assert_eq!(read_refusal(false, 0), None);
        assert_eq!(read_refusal(false, 63), None);
        assert_eq!(
            read_refusal(false, 64),
            Some("too many reads are waiting here")
        );
        assert_eq!(
            read_refusal(false, 65),
            Some("too many reads are waiting here")
        );
        assert_eq!(
            read_refusal(true, 0),
            Some("another app's data cannot be read here")
        );
    }

    #[test]
    fn only_actions_the_workspaces_host_runs_travel_to_it() {
        for provider in [
            "record:so.tilo.notes:n1",
            "record:so.tilo.notes:",
            "workspace:r",
            "tilo.workspace",
        ] {
            assert_eq!(refusal(provider, 0), None, "{provider}");
        }
        assert_eq!(
            refusal("record:so.tilo.notes:n1", 16),
            Some("The workspace's host is busy; try again")
        );
        for provider in ["so.tilo.chat", "tilo.own-device", "workspaces", ""] {
            assert_eq!(
                refusal(provider, 0),
                Some("Only the workspace's host runs another app's actions here"),
                "{provider}"
            );
        }
    }
}
