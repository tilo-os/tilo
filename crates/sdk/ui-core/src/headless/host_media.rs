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
}
impl MediaLane {
    /// Take local media commands from each retained component under its
    /// bound callback context. This lane carries no identity or remote authority.
    /// The host handles document replication separately.
    pub fn take(&mut self, frame: &mut FrameHarness) -> Vec<MediaRequest> {
        let mut requests = Vec::new();
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
        let mut visit = |binding: &CallbackBinding,
                         widget: &mut dyn crate::widget::AppWidget,
                         _: &tilo_actions::ActionRegistry| {
            if binding.app_id != "so.tilo.chat" {
                return None;
            }
            let Some(route) = binding.broker_route() else {
                return None;
            };
            for request in crate::widget::take_collab_requests(widget) {
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
                }
            }
            None
        };
        let _ = frame.root.visit_components(&mut ComponentRequest {
            target: None,
            found: false,
            visit: &mut visit,
        });
        requests
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
