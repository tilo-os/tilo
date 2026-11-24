//! What the person's shell tells this device's apps about their windows
//! ([`window_follow`](tilo_actions::remote::window_follow)): whose view the person follows, and the
//! next step of moving one app's window to another device. The apps' connections are this
//! daemon's, so the daemon carries both out on its fan-out, to the apps' own routes.
//!
//! Only the person's own shell calls them: an app, an agent, or another device is refused. The
//! shell's calls arrive as the person its link was made for.

use std::sync::Arc;

use collab_types::handoff::HandoffRequest;
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::window_follow as wire;
use tilo_actions::remote::{
    Admits, Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending,
};
use tilo_workspace_service::apps::collab_broker::{FanoutState, SessionFollowDispatcher};
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

/// The shell's window steps, carried out on this daemon's fan-out.
pub(crate) struct WindowFollow(SessionFollowDispatcher);

impl WindowFollow {
    pub(crate) fn new(fanout: Arc<FanoutState>) -> Self {
        Self(SessionFollowDispatcher::new(fanout))
    }

    /// Take the shell's window steps from now on, on this device alone. No agent's catalogue
    /// lists a provider that reaches this device alone.
    pub(super) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(wire::PROVIDER, wire::APP, Reach::ThisDevice, self);
    }
}

/// Step `action`, described for the dispatch's catalogue.
fn descriptor(action: &str, label: &str, detail: &str) -> tilo_actions::ActionDescriptor {
    use crate::i18n::t;
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(wire::RESOURCE, action),
        resource_kind: wire::RESOURCE.into(),
        resource_label: t("window-follow-resource"),
        action_label: t(label),
        keywords: Vec::new(),
        icon: None,
        description: Some(t(detail)),
        locus: tilo_actions::ExecutionLocus::default(),
        // What the person's windows show, on this device: never asked about first, and nothing a
        // workspace's history keeps.
        kind: tilo_actions::OperationKind::View,
        contract: tilo_actions::ActionContract::view(),
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

/// The argument of `request`, read as `T`.
fn argument<T: serde::de::DeserializeOwned>(request: &InvokeRequest) -> Result<T, String> {
    let InvokeArgument::Json(fields) = &request.argument else {
        return Err("a window step takes its argument as an object".into());
    };
    serde_json::from_value(serde_json::Value::Object(fields.clone()))
        .map_err(|error| format!("a window step's argument does not read: {error}"))
}

/// Whether `caller` is the person themself, as their own shell on this device calls: not an app,
/// an agent, a session delegated to one, or another device.
fn is_shell(caller: &Caller) -> bool {
    let acting = caller.acting();
    acting.is_person()
        && acting.app().is_none()
        && !caller.is_session_delegated()
        && !caller.is_foreign_device()
}

impl Admits for WindowFollow {
    const PROVIDER: &'static str = wire::PROVIDER;
    const APP: &'static str = wire::APP;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(vec![
            descriptor(
                wire::FOLLOW,
                "window-follow-follow",
                "window-follow-follow-detail",
            ),
            descriptor(
                wire::HANDOFF,
                "window-follow-handoff",
                "window-follow-handoff-detail",
            ),
        ])
    }

    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let address = &request.target.address;
        if address.resource != wire::RESOURCE {
            return Err("unknown window step".into());
        }
        if !is_shell(caller) {
            return Err("only the person's own shell moves and follows their windows".into());
        }
        let workspace = caller
            .workspace()
            .ok_or("a window step is taken in a workspace")?;
        match address.action.as_str() {
            wire::FOLLOW => {
                let follow: wire::Follow = argument(request)?;
                self.0.set(workspace, follow.peer);
            }
            wire::HANDOFF => {
                let handoff: wire::Handoff<HandoffRequest> = argument(request)?;
                self.0
                    .request_handoff(workspace, &handoff.app, handoff.instance, handoff.request);
            }
            _ => return Err("unknown window step".into()),
        }
        Ok(Pending::ready(Ok(tilo_actions::ActionOutput::default())))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use collab_types::handoff::HandoffRequest;
    use runtime_collab::event::CollabEvent;
    use tilo_actions::remote::window_follow as wire;
    use tilo_actions::remote::{ActionCallError, ActionProvider, Caller, ReceiptStatus};
    use tilo_workspace_service::apps::collab_broker::FanoutState;

    use super::*;

    const WORKSPACE: &str = "ws-window-follow";

    /// What the apps of `WORKSPACE` on the fan-out heard, by the record each went to.
    type Heard = Arc<Mutex<Vec<(String, CollabEvent)>>>;

    fn listening() -> (WindowFollow, Heard, Arc<FanoutState>) {
        let fanout = Arc::new(FanoutState::new());
        let heard: Heard = Arc::default();
        let sink = Arc::clone(&heard);
        let _subscription = fanout.subscribe_workspaces(
            vec![WORKSPACE.to_owned()],
            Box::new(move |event, route| {
                sink.lock()
                    .unwrap()
                    .push((route.document_id.clone(), event.clone()));
            }),
        );
        (WindowFollow::new(Arc::clone(&fanout)), heard, fanout)
    }

    fn call(
        steps: &WindowFollow,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<ReceiptStatus, ActionCallError> {
        steps
            .invoke(caller, request)
            .wait_timeout(Duration::from_secs(5))
            .ok()
            .flatten()
            .expect("a window step answers at once")
            .map(|receipt| receipt.status)
    }

    fn person() -> Caller {
        Caller::authenticated([1; 32], [2; 32]).in_workspace(WORKSPACE)
    }

    #[test]
    fn the_shell_tells_its_apps_and_nobody_else_can() {
        let (steps, heard, _fanout) = listening();
        let follow = || {
            wire::request(
                wire::FOLLOW,
                &wire::Follow {
                    peer: Some(tilo_actions::MemberId::Agent("helper".into())),
                },
            )
        };

        assert!(matches!(
            call(&steps, &person(), follow()),
            Ok(ReceiptStatus::Completed { .. })
        ));
        let handoff = wire::request(
            wire::HANDOFF,
            &wire::Handoff {
                app: "so.tilo.writer".to_owned(),
                instance: 7,
                request: HandoffRequest::Checkpoint { token: 3 },
            },
        );
        assert!(matches!(
            call(&steps, &person(), handoff),
            Ok(ReceiptStatus::Completed { .. })
        ));
        {
            let heard = heard.lock().unwrap();
            assert_eq!(heard.len(), 2, "{heard:?}");
            assert!(matches!(
                &heard[0],
                (_, CollabEvent::FollowChanged { peer_id: Some(tilo_actions::MemberId::Agent(agent)) })
                    if agent == "helper"
            ));
            assert!(matches!(
                &heard[1],
                (record, CollabEvent::Handoff { request: HandoffRequest::Checkpoint { token: 3 } })
                    if record == "continuation:7"
            ));
        }

        // An app, an agent and another device naming the same step are refused, and no app
        // hears of it.
        for stranger in [
            Caller::local_app("so.tilo.writer").in_workspace(WORKSPACE),
            Caller::local_agent("helper").in_workspace(WORKSPACE),
            person().from_foreign_device(true),
        ] {
            assert!(matches!(
                call(&steps, &stranger, follow()),
                Err(ActionCallError::Refused { .. })
            ));
        }
        assert_eq!(heard.lock().unwrap().len(), 2);
    }
}
