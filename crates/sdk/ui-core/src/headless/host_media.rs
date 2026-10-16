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
            if binding.app_id != "so.tilo.chat" {
                return None;
            }
            let route = binding.broker_route()?;
            for request in crate::widget::take_collab_requests(widget) {
                took = true;
                if let CollabRequest::RealtimeCommand { request_id, .. } = request {
                    crate::widget::handle_collab_event(
                        widget,
                        &CollabEvent::RealtimeCommandResult {
                            request_id,
                            result: Err("continuity is unavailable in this host".into()),
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
                    );
                    if (!stop && self.pending.len() >= 16) || !command.valid() {
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

/// The provider of a room's own governance resource
/// (`workspace.permissions:<room>`).
const GOVERNANCE_PROVIDER: &str = "tilo.workspace";

fn queue_catalogue(
    catalogues: &mut Vec<(CallbackBinding, tilo_actions::remote::RemoteActionMessage)>,
    binding: &CallbackBinding,
    widget: &mut dyn crate::widget::AppWidget,
    message: tilo_actions::remote::RemoteActionMessage,
) {
    use tilo_actions::remote::{ActionHostError, RemoteActionMessage};
    // The room's membership actions (removing, banning, unbanning someone)
    // travel this lane too: the host turns them into its governed room
    // requests and the room's policy decides them there. So does a record
    // resource's `share-link`, which reads where it is shown and grants
    // nothing: the page asks the room's host for it as the person.
    if let RemoteActionMessage::Invoke(request) = &message
        && (request.target.provider == GOVERNANCE_PROVIDER
            || (request.target.address.action == tilo_resource::share::SHARE_LINK
                && request.target.provider.starts_with("record:")))
    {
        if catalogues.len() < 16 {
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
