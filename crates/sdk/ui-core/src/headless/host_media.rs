//! A retained component's local media lane for in-process hosts. Requests
//! retain their exact component lifetime; a reply cannot reach a replacement.
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
    pub room: String,
    pub record: String,
    pub command: Request,
}

#[derive(Default)]
pub struct MediaLane {
    next: u64,
    pending: BTreeMap<u64, (CallbackBinding, u64)>,
    owners: BTreeMap<String, (CallbackBinding, MediaRequest)>,
    ready: Vec<MediaRequest>,
    catalogues: Vec<(CallbackBinding, tilo_actions::remote::RemoteActionMessage)>,
}
impl MediaLane {
    /// Take local media commands from each retained component under its
    /// bound callback context. This lane carries no identity or remote authority.
    /// The host handles document replication separately.
    pub fn take(&mut self, frame: &mut FrameHarness) -> Vec<MediaRequest> {
        self.pump(frame);
        std::mem::take(&mut self.ready)
    }

    /// Drain read-only catalogue requests without stealing pending media commands.
    /// The transport must authenticate them; closed component owners are discarded.
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
        // Whether a component handed over a request: taking it (and the
        // answers some get at once) changes that component.
        let mut took = false;
        let mut visit = |binding: &CallbackBinding,
                         widget: &mut dyn crate::widget::AppWidget,
                         _: &tilo_actions::ActionRegistry| {
            // Every app may ask the room's host to run an action; only
            // Chat holds a call, so only it reaches the media lane.
            let media = binding.app_id == "so.tilo.chat";
            let route = binding.broker_route()?;
            for request in crate::widget::take_collab_requests(widget) {
                took = true;
                if let CollabRequest::RealtimeCommand { request_id, .. } = request {
                    crate::widget::handle_collab_event(
                        widget,
                        &CollabEvent::RealtimeCommandResult {
                            request_id,
                            result: Err("Continuing on another device isn't available here".into()),
                        },
                    );
                } else if let CollabRequest::NativeMediaCommand {
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
                                result: Err("local media lane is unavailable".into()),
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
                        room: route.cluster_id.to_string(),
                        record: route.record.clone().unwrap_or_default(),
                        command,
                    };
                    self.owners
                        .insert(request.owner.clone(), (binding.clone(), request.clone()));
                    requests.push(request);
                } else if let CollabRequest::RemoteAction { message } = request {
                    queue_catalogue(&mut self.catalogues, binding, widget, message);
                }
            }
            None
        };
        let _ = frame.root.visit_components(&mut ComponentRequest {
            target: None,
            found: false,
            visit: &mut visit,
        });
        if took {
            frame.invalidate_local();
        }
    }

    /// Complete only the exact retained owner that asked. Closed components
    /// and duplicate completions are refused.
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
        let mut request = ComponentRequest {
            target: Some(&binding),
            found: false,
            visit: &mut visit,
        };
        let _ = frame.root.visit_components(&mut request);
        frame.relayout();
        request.found
    }
}

/// Whether the room's host runs `provider`'s actions: a room record's
/// (`record:<app>:<record>`), the room's own (`workspace:<room>`) and its
/// governance resource's (`workspace.permissions:<room>`).
fn host_runs(provider: &str) -> bool {
    provider.starts_with("record:")
        || tilo_actions::remote::workspace_provider_room(provider).is_some()
        || provider == tilo_actions::remote::ROOM_GOVERNANCE_PROVIDER
}

/// Why the lane refuses an invocation of `provider` with `queued` already
/// waiting for the host, if it does.
fn refusal(provider: &str, queued: usize) -> Option<&'static str> {
    if !host_runs(provider) {
        Some("Only the room's host runs another app's actions here")
    } else if queued >= 16 {
        Some("The room's host is busy; try again")
    } else {
        None
    }
}

fn queue_catalogue(
    catalogues: &mut Vec<(CallbackBinding, tilo_actions::remote::RemoteActionMessage)>,
    binding: &CallbackBinding,
    widget: &mut dyn crate::widget::AppWidget,
    message: tilo_actions::remote::RemoteActionMessage,
) {
    use tilo_actions::remote::{
        ActionHostError, InvocationReceipt, ReceiptStatus, RemoteActionMessage,
    };
    // An action the room's host runs travels this lane to it, and the host
    // admits it by the person's own standing in the room: a page cannot
    // grant itself anything. The room's membership actions become the
    // host's governed room requests, decided by the room's policy there.
    if let RemoteActionMessage::Invoke(request) = &message {
        if let Some(reason) = refusal(&request.target.provider, catalogues.len()) {
            crate::widget::handle_collab_event(
                widget,
                &CollabEvent::RemoteAction {
                    message: RemoteActionMessage::Receipt(InvocationReceipt {
                        invocation: request.invocation,
                        status: ReceiptStatus::Rejected {
                            error: ActionHostError::Unavailable {
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
                    result: Err(ActionHostError::Unavailable {
                        reason: "local catalogue lane is busy".into(),
                    }),
                },
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::refusal;

    #[test]
    fn only_actions_the_rooms_host_runs_travel_to_it() {
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
            Some("The room's host is busy; try again")
        );
        for provider in ["so.tilo.chat", "tilo.own-device", "workspaces", ""] {
            assert_eq!(
                refusal(provider, 0),
                Some("Only the room's host runs another app's actions here"),
                "{provider}"
            );
        }
    }
}
