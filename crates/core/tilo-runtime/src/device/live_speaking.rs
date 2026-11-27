//! What this device says of itself in each workspace's live session, and the calls it joins
//! there.
//!
//! The device has one word in each workspace, and its daemon says it ([`tilo_live::speaker`]):
//! what the person's shell tells it of them over the shell link, with the runs the device
//! executes there ([`ShellSpeaking::say`]); what the device's apps say of the person in their
//! records, over the connection each app's launcher handed it ([`Speaking::say_app`]); and the
//! call the device is in. Everyone else's words in their apps are handed to the device's apps
//! the same way, as the workspace's live session hears them ([`Speaking::watch_app`]).
//!
//! A call is joined and left by the typed actions [`JOIN`] and [`LEAVE`] of this device's live
//! sessions ([`PROVIDER`], [`tilo_live::link::SESSIONS`]): an app calls them through the device's
//! one dispatch, which admits, asks and records them as any action under the app's grants, and the
//! person's shell names the same through its link ([`ShellSpeaking::join`]). Joining holds the
//! device to the call's rule, and the daemon fetches the media keys of its generation, which never
//! leave it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use collab_types::MemberId;
use collab_types::presence::{CallPresence, Payload, Presence, PresenceKey, ShellPresence};
use runtime_collab::wire::shell::{ShellInCall, ShellWord, SpokenFor};
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{
    Admits, Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionKind, ActionOutput, ExecutionLocus, OperationKind,
    ResultSpec,
};
use tilo_app_host::collab_broker::{AppPresence, CrdtRoute};
use tilo_app_host::shell::ShellSpeaking;
use tilo_desktop_model::WorkspaceId;
use tilo_live::presence::Own;
use tilo_live::speaker::Speaker;
use tilo_lock::MutexRecover as _;

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
        asks: false,
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

/// What the person is shown when `app`, which carries an outside service's call, asks to join
/// the workspace's calls with no grant there yet: their "Approve and grant" lets it join them from
/// then on, as the app acting by itself, and nothing else.
pub(crate) fn asked_to_join(app: &str) -> tilo_actions::remote::asked::AskedFacts {
    tilo_actions::remote::asked::AskedFacts {
        words: format!(
            "{app} asks to join this workspace's calls, to carry the people of an outside \
             service's call into them and what is said here back out. Approve and grant, and it \
             joins a call paired with one it carries whenever someone is in it."
        ),
        on: Some("this workspace's calls".to_owned()),
        verb: "join".to_owned(),
        plural: Some("this workspace's calls".to_owned()),
        kind: tilo_live::link::SESSIONS.to_owned(),
        effect: Some(ActionKind::Outside),
        ..tilo_actions::remote::asked::AskedFacts::default()
    }
}

/// What an app's join waits on while its person is asked whether it may join calls.
pub(crate) fn join_asked_first(app: &str) -> String {
    format!(
        "{app} may not join this workspace's calls yet: your person is asked whether it may, \
         once. It joins once they allowed it."
    )
}

/// What one app said of the person under one key: its app, the key, and the word.
type AppWord = (String, PresenceKey, Payload);

/// What the person's shell says in one workspace, and the runs the device executes there.
type ShellSaid = (Option<Own>, Vec<(String, String, ShellPresence)>);

/// What one app's connection hears: every word another member says in its workspace, as the
/// workspace's live session hears it. `id` is the connection, so a word is never handed back to
/// the app that said it.
pub(crate) struct AppWatch {
    id: u64,
    sink: Arc<dyn Fn(MemberId, Presence) + Send + Sync>,
}

/// The apps' connections watching one workspace, and how the next is numbered.
type Watchers = Arc<Mutex<BTreeMap<String, Vec<AppWatch>>>>;

/// An app's registration as a watcher, which hears the workspace's words until it is dropped:
/// what an app's connection holds for as long as it lives.
pub(crate) struct Watch {
    id: u64,
    workspace: String,
    watchers: Watchers,
}

impl Watch {
    /// The connection this watch is for.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Ok(mut watchers) = self.watchers.lock() {
            if let Some(watching) = watchers.get_mut(&self.workspace) {
                watching.retain(|watch| watch.id != self.id);
            }
            if watchers.get(&self.workspace).is_some_and(Vec::is_empty) {
                watchers.remove(&self.workspace);
            }
        }
    }
}

/// What the device's apps and the person's shell say in each workspace, kept for a live session
/// that opens after they said it, and each workspace's speaker while its live session is open.
pub(crate) struct Speaking {
    apps: Arc<Mutex<BTreeMap<String, Vec<AppWord>>>>,
    shell: Mutex<BTreeMap<String, ShellSaid>>,
    speakers: Arc<Mutex<BTreeMap<String, Weak<Speaker>>>>,
    /// The apps' connections hearing each workspace's words.
    watchers: Watchers,
    next_watch: AtomicU64,
}

impl Speaking {
    pub(crate) fn new() -> Self {
        Self {
            apps: Arc::default(),
            shell: Mutex::default(),
            speakers: Arc::default(),
            watchers: Arc::default(),
            next_watch: AtomicU64::new(1),
        }
    }

    /// Watch `workspace`: `sink` hears every word another member says there until the watch is
    /// dropped. `id` names the connection, so a word it says itself is not handed back.
    pub(crate) fn watch_app(
        &self,
        workspace: &str,
        sink: impl Fn(MemberId, Presence) + Send + Sync + 'static,
    ) -> Watch {
        let id = self.next_watch.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut watchers) = self.watchers.lock() {
            watchers
                .entry(workspace.to_owned())
                .or_default()
                .push(AppWatch {
                    id,
                    sink: Arc::new(sink),
                });
        }
        Watch {
            id,
            workspace: workspace.to_owned(),
            watchers: Arc::clone(&self.watchers),
        }
    }

    /// What the app `app` says of its person in `workspace` under `key`: kept for a live session
    /// that opens later, said by the workspace's speaker for the device, and handed to the other
    /// apps of the workspace here but the connection `except`.
    pub(crate) fn say_app(
        &self,
        app: &str,
        workspace: &str,
        member: &MemberId,
        key: PresenceKey,
        payload: Payload,
        except: Option<u64>,
    ) {
        {
            let mut all = self.apps.lock_recover();
            let apps = all.entry(workspace.to_owned()).or_default();
            apps.retain(|(held_app, held, _)| held_app != app || *held != key);
            if payload != Payload::Left {
                apps.push((app.to_owned(), key.clone(), payload.clone()));
            }
            drop(all);
        }
        let speaker = self
            .speakers
            .lock_recover()
            .get(workspace)
            .and_then(Weak::upgrade);
        if let Some(speaker) = speaker {
            speaker.set_app_word(app, key.clone(), Some(payload.clone()));
        }
        self.tell(
            workspace,
            member,
            &Presence::app(app.to_owned(), workspace.to_owned(), key, payload),
            except,
        );
    }

    /// The app `app`'s connection in `workspace` ended, and it leaves every `key` it said there:
    /// nobody is shown at a caret whose window closed.
    pub(crate) fn left_app(
        &self,
        app: &str,
        workspace: &str,
        member: &MemberId,
        keys: &[PresenceKey],
    ) {
        let speaker = self
            .speakers
            .lock_recover()
            .get(workspace)
            .and_then(Weak::upgrade);
        for key in keys {
            {
                let mut all = self.apps.lock_recover();
                let apps = all.entry(workspace.to_owned()).or_default();
                apps.retain(|(held_app, held, _)| held_app != app || held != key);
                drop(all);
            }
            if let Some(speaker) = &speaker {
                speaker.set_app_word(app, key.clone(), Some(Payload::Left));
            }
            self.tell(
                workspace,
                member,
                &Presence::app(
                    app.to_owned(),
                    workspace.to_owned(),
                    key.clone(),
                    Payload::Left,
                ),
                None,
            );
        }
    }

    /// `frame`, what `member` said on another device, is handed to the apps of its workspace.
    fn tell(&self, workspace: &str, member: &MemberId, frame: &Presence, except: Option<u64>) {
        tell_watched(&self.watchers, workspace, member, frame, except);
    }

    /// `frame`, what `member` said on another device, reaches the apps watching `workspace`, as
    /// the workspace's live session hearing it does: what a test says for a device it does not
    /// run.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn hear_from_other_device(&self, member: &MemberId, frame: &Presence) {
        tell_watched(&self.watchers, &frame.cluster, member, frame, None);
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
        let watchers = Arc::clone(&self.watchers);
        speaker.on_heard(Arc::new(move |member, frame| {
            tell_watched(&watchers, &frame.cluster, member, frame, None);
        }));
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

    /// The person's shell ended: forget the word it said in every workspace, and answer the
    /// workspaces it said it in, so the device says only what it is there from now on.
    fn forget_shell(&self) -> Vec<String> {
        let said = std::mem::take(&mut *self.shell.lock_recover());
        let speakers = self.speakers.lock_recover();
        for workspace in said.keys() {
            if let Some(speaker) = speakers.get(workspace).and_then(Weak::upgrade) {
                speaker.set_shell(None);
                speaker.set_agents(Vec::new());
            }
        }
        said.into_keys().collect()
    }
}

impl AppPresence for Live {
    /// The broker's own account of an action as it runs, said by the device for the member it
    /// acts as, in the app and workspace it acts in: the same word an app's views say.
    fn said(
        &self,
        member: &MemberId,
        key: PresenceKey,
        payload: collab_types::presence::PresencePayload,
        origin: &CrdtRoute,
    ) {
        if let Some(live) = payload.to_live() {
            self.speaking
                .say_app(&origin.app_id, &origin.cluster_id, member, key, live, None);
        }
    }
}

/// Hand `frame`, what `member` said in `workspace`, to the apps watching it there but the
/// connection `except`.
fn tell_watched(
    watchers: &Watchers,
    workspace: &str,
    member: &MemberId,
    frame: &Presence,
    except: Option<u64>,
) {
    let sinks: Vec<Arc<dyn Fn(MemberId, Presence) + Send + Sync>> = {
        let Ok(watchers) = watchers.lock() else {
            return;
        };
        watchers
            .get(workspace)
            .into_iter()
            .flatten()
            .filter(|watch| Some(watch.id) != except)
            .map(|watch| Arc::clone(&watch.sink))
            .collect()
    };
    for sink in sinks {
        sink(member.clone(), frame.clone());
    }
}

impl Live {
    /// The person's shell ended: the word they said and the call they were in are no longer said
    /// in any workspace, and the device says only what it is there.
    async fn shell_gone(self: &Arc<Self>) {
        for workspace in self.speaking.forget_shell() {
            let _ = self.join_in(&workspace, None).await;
        }
    }

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

    fn gone(&self) {
        let live = self.weak().upgrade();
        if let Some(live) = live {
            tilo_rt::app().spawn(async move { live.shell_gone().await });
        }
    }
}

/// The device's live sessions' actions, answered through its one dispatch.
pub(crate) struct LiveCalls(pub(crate) Weak<Live>);

impl Admits for LiveCalls {
    const PROVIDER: &'static str = PROVIDER;
    const APP: &'static str = PROVIDER;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(vec![
            descriptor(JOIN, "live-call-join", "live-call-join-detail"),
            descriptor(LEAVE, "live-call-leave", "live-call-leave-detail"),
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

/// `action` of the device's live sessions, as a catalogue lists it: `label` and `detail` are the
/// keys of its words in the runtime's catalogue.
fn descriptor(action: &str, label: &str, detail: &str) -> ActionDescriptor {
    use crate::i18n::t;
    ActionDescriptor {
        did: None,
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(tilo_live::link::SESSIONS, action),
        resource_kind: tilo_live::link::SESSIONS.to_owned(),
        resource_label: t("live-call-resource"),
        action_label: t(label),
        keywords: Vec::new(),
        state: tilo_actions::ActionState::default(),
        icon: None,
        description: Some(t(detail)),
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
    }
}

#[cfg(test)]
#[path = "live_speaking_tests.rs"]
pub(super) mod tests;
