//! What this device says of itself in each workspace's live session, and the calls it joins
//! there.
//!
//! The device has one word in each workspace, and its daemon says it ([`tilo_live::speaker`]):
//! what the person's shell tells it of them over the shell link, with the runs the device
//! executes there ([`ShellSpeaking::say`]); what the device's apps say of the person in their
//! records, as the broker's fan-out carries it ([`Speaking::hear_apps`]); and the call the device
//! is in. Everyone else's words in their apps are handed to the device's apps the same way.
//!
//! A call is joined and left by the typed actions [`JOIN`] and [`LEAVE`] of this device's live
//! sessions ([`PROVIDER`], [`tilo_live::link::SESSIONS`]): an app calls them through the device's
//! one dispatch, which admits, asks and records them as any action under the app's grants, and the
//! person's shell names the same through its link ([`ShellSpeaking::join`]). Joining holds the
//! device to the call's rule, and the daemon fetches the media keys of its generation, which never
//! leave it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use collab_types::presence::{CallPresence, Payload, PresenceKey, ShellPresence};
use runtime_collab::event::CollabEvent;
use runtime_collab::wire::shell::{ShellInCall, ShellWord, SpokenFor};
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{
    Admits, Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionKind, ActionOutput, ExecutionLocus, OperationKind,
    ResultSpec,
};
use tilo_desktop_model::WorkspaceId;
use tilo_live::presence::Own;
use tilo_live::speaker::Speaker;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::collab_broker::{FanoutState, WorkspaceSubscription};
use tilo_workspace_service::apps::shell::ShellSpeaking;

use super::live_relay::Live;

/// The provider the device's live sessions' actions are answered under, and whose grants decide
/// them.
pub(crate) const PROVIDER: &str = "tilo.live";
/// Join a call of the caller's workspace ([`Joining`]).
pub(crate) const JOIN: &str = "join";
/// Leave the call the device is in in the caller's workspace.
pub(crate) const LEAVE: &str = "leave";

/// What [`JOIN`] is asked: the call's channel, its name, and the service a bridge carries it to.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Joining {
    pub(crate) channel: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) bridge: Option<String>,
}

/// What [`JOIN`] answers: the call's channel, and the generation of its rule the media is sealed
/// under.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Joined {
    pub(crate) channel: String,
    pub(crate) generation: u64,
}

/// The call of `action` of the device's live sessions with `argument`.
pub(crate) fn request(action: &str, argument: serde_json::Value) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(tilo_live::link::SESSIONS, action),
        },
        argument: match argument {
            serde_json::Value::Object(fields) => InvokeArgument::Json(fields),
            _ => InvokeArgument::None,
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// What one app said of the person under one key: its app, the key, and the word.
type AppWord = (String, PresenceKey, Payload);

/// What the person's shell says in one workspace, and the runs the device executes there.
type ShellSaid = (Option<Own>, Vec<(String, String, ShellPresence)>);

/// What the device's apps and the person's shell say in each workspace, kept for a live session
/// that opens after they said it, and each workspace's speaker while its live session is open.
pub(crate) struct Speaking {
    fanout: Arc<FanoutState>,
    apps: Arc<Mutex<BTreeMap<String, Vec<AppWord>>>>,
    shell: Mutex<BTreeMap<String, ShellSaid>>,
    speakers: Arc<Mutex<BTreeMap<String, Weak<Speaker>>>>,
    /// Held so the device's apps' words keep reaching the speakers.
    subscription: OnceLock<Arc<WorkspaceSubscription>>,
}

impl Speaking {
    pub(crate) fn new(fanout: Arc<FanoutState>) -> Self {
        Self {
            fanout,
            apps: Arc::default(),
            shell: Mutex::default(),
            speakers: Arc::default(),
            subscription: OnceLock::new(),
        }
    }

    /// Take what the device's apps say of its person, as the broker's fan-out carries it, into
    /// the word of the workspace each says it in.
    pub(crate) fn hear_apps(&self, live: &Arc<Live>) {
        let person = hex::encode(live.person());
        let (apps, speakers) = (Arc::clone(&self.apps), Arc::clone(&self.speakers));
        let subscription = self.fanout.subscribe_workspaces(
            Vec::new(),
            Box::new(move |event, route| {
                // A word an app here said names the app; what is handed to the apps of someone
                // else's names only the workspace.
                let CollabEvent::RemotePresenceUpdate {
                    member,
                    key,
                    payload,
                } = event
                else {
                    return;
                };
                if route.app_id.is_empty() || member.person_id() != Some(person.as_str()) {
                    return;
                }
                let word = payload.to_live();
                let workspace = route.cluster_id.clone();
                {
                    let mut apps = apps.lock_recover();
                    let words = apps.entry(workspace.clone()).or_default();
                    words.retain(|(app, held, _)| *app != route.app_id || held != key);
                    if let Some(word) = word.clone().filter(|word| *word != Payload::Left) {
                        words.push((route.app_id.clone(), key.clone(), word));
                    }
                }
                let speaker = speakers
                    .lock_recover()
                    .get(&workspace)
                    .and_then(Weak::upgrade);
                if let Some(speaker) = speaker {
                    speaker.set_app_word(&route.app_id, key.clone(), word);
                }
            }),
        );
        let _ = self.subscription.set(subscription);
    }

    /// The device's apps' words are followed in `held`, the workspaces the device holds.
    pub(crate) fn follow_workspaces(&self, held: &[WorkspaceId]) {
        if let Some(subscription) = self.subscription.get() {
            subscription.set_workspaces(held.iter().map(ToString::to_string).collect());
        }
    }

    /// `workspace`'s live session opened, and `speaker` says the device's word in it: everything
    /// already said there is said in it, and everyone else's words in their apps reach the
    /// device's apps.
    pub(crate) fn speak_in(&self, workspace: WorkspaceId, speaker: &Arc<Speaker>) {
        let workspace = workspace.to_string();
        let shell = self.shell.lock_recover().get(&workspace).cloned();
        if let Some((word, agents)) = shell {
            speaker.set_shell(word);
            speaker.set_agents(agents);
        }
        let apps = self.apps.lock_recover().get(&workspace).cloned();
        for (app, key, word) in apps.unwrap_or_default() {
            speaker.set_app_word(&app, key, Some(word));
        }
        let fanout = Arc::clone(&self.fanout);
        speaker.on_heard(Arc::new(move |member, frame| fanout.heard(member, frame)));
        self.speakers
            .lock_recover()
            .insert(workspace, Arc::downgrade(speaker));
    }

    /// The person's shell says `word` in `workspace`, with `agents`.
    fn say(
        &self,
        workspace: &str,
        word: Option<Own>,
        agents: Vec<(String, String, ShellPresence)>,
    ) {
        self.shell
            .lock_recover()
            .insert(workspace.to_owned(), (word.clone(), agents.clone()));
        let speaker = self
            .speakers
            .lock_recover()
            .get(workspace)
            .and_then(Weak::upgrade);
        if let Some(speaker) = speaker {
            speaker.set_shell(word);
            speaker.set_agents(agents);
        }
    }
}

impl Live {
    /// Join `call` in `workspace`, or leave the call the device is in there (`None`).
    async fn join_in(
        &self,
        workspace: &str,
        call: Option<CallPresence>,
    ) -> Result<Option<(String, u64)>, String> {
        let id = WorkspaceId::parse(workspace).ok_or("that is no workspace")?;
        let session = self
            .call_session(id)
            .ok_or("this device takes part in no live session of that workspace")?;
        let gate = self.call_gate(id);
        session
            .enter_call(self.workspaces().copies(), call, &gate)
            .await
    }
}

impl ShellSpeaking for Live {
    fn say(
        &self,
        workspace: &str,
        word: Option<ShellWord>,
        agents: Vec<SpokenFor>,
    ) -> Result<(), String> {
        WorkspaceId::parse(workspace).ok_or("that is no workspace")?;
        self.speaking.say(
            workspace,
            word.map(super::live_call::own_of),
            agents
                .into_iter()
                .map(|agent| (agent.session, agent.member, agent.face))
                .collect(),
        );
        Ok(())
    }

    fn join(
        &self,
        workspace: &str,
        call: Option<ShellInCall>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<(String, u64)>, String>> + Send>,
    > {
        let live = self.weak();
        let workspace = workspace.to_owned();
        Box::pin(async move {
            let live = live.upgrade().ok_or("this device's daemon is stopping")?;
            live.join_in(&workspace, call.map(|call| call.call)).await
        })
    }
}

/// The device's live sessions' actions, answered through its one dispatch.
pub(crate) struct LiveCalls(pub(crate) Weak<Live>);

impl Admits for LiveCalls {
    const PROVIDER: &'static str = PROVIDER;
    const APP: &'static str = PROVIDER;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(vec![
            descriptor(
                JOIN,
                "Join",
                "Join a call of this workspace on this device.",
            ),
            descriptor(LEAVE, "Leave", "Leave the call this device is in here."),
        ])
    }

    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let address = &request.target.address;
        if address.resource != tilo_live::link::SESSIONS {
            return Err("unknown action of the device's live sessions".to_owned());
        }
        let workspace = caller
            .workspace()
            .ok_or("a call is joined in a workspace")?
            .to_owned();
        let call = match address.action.as_str() {
            JOIN => {
                let InvokeArgument::Json(fields) = &request.argument else {
                    return Err("a join names its call as an object".to_owned());
                };
                let joining: Joining =
                    serde_json::from_value(serde_json::Value::Object(fields.clone()))
                        .map_err(|error| format!("a join does not read: {error}"))?;
                if joining.channel.trim().is_empty() {
                    return Err("a join names its call's channel".to_owned());
                }
                Some(CallPresence {
                    channel: joining.channel,
                    name: joining.name,
                    bridge: joining.bridge,
                })
            }
            LEAVE => None,
            _ => return Err("unknown action of the device's live sessions".to_owned()),
        };
        let live = self.0.upgrade().ok_or("this device's daemon is stopping")?;
        let (done, pending) = tilo_actions::remote::pending();
        tilo_rt::app().spawn(async move {
            done.complete(answered(live.join_in(&workspace, call).await));
        });
        Ok(pending)
    }
}

/// What a join or leave answers.
fn answered(joined: Result<Option<(String, u64)>, String>) -> Outcome {
    match joined {
        Ok(joined) => Ok(ActionOutput {
            value: joined.map(|(channel, generation)| {
                serde_json::to_value(Joined {
                    channel,
                    generation,
                })
                .unwrap_or_default()
            }),
            resources: Vec::new(),
        }),
        Err(message) => Err(Box::new(tilo_actions::ActionFailure {
            code: "call_refused".into(),
            message,
            details: None,
            retryable: false,
        })),
    }
}

/// `action` of the device's live sessions, as a catalogue lists it.
fn descriptor(action: &str, label: &str, detail: &str) -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(tilo_live::link::SESSIONS, action),
        resource_kind: tilo_live::link::SESSIONS.to_owned(),
        resource_label: "Live session".to_owned(),
        action_label: label.to_owned(),
        keywords: Vec::new(),
        state: tilo_actions::ActionState::default(),
        icon: None,
        description: Some(detail.to_owned()),
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Outside,
            result: ResultSpec::Json {
                schema: serde_json::json!({ "type": "object" }),
            },
            ..ActionContract::default()
        },
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
        effects: Vec::new(),
    }
}

#[cfg(test)]
#[path = "live_speaking_tests.rs"]
mod tests;
