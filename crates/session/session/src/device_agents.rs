//! Agents on the person's other devices working here, and agents here working on theirs, over the
//! one action frame between devices.
//!
//! An agent on another of the person's devices (an MCP client there) works here as their agent:
//! that device names it on every request it sends
//! ([`HandedAgent`](neighbour_wire::actions::HandedAgent)), and this device's action service takes
//! each request as that agent's, for the person, never as the person themselves
//! ([`tilo_peer_state::actions`]). Here it is the person's agent member for its client in the
//! workspace, in an agent session of its own, which the shell lists with the device beside its
//! name and the person's Stop ends: from then on its requests are refused. Its steps are decided
//! as any agent's are, by its grants, asking the person on this device's sheet where those say
//! ask ([`crate::step_questions`]). This device's settings decide whether such agents work here
//! at all ("Accept agents from my other devices") and whether their steps go unasked.
//!
//! The other way round, a client of this login asks with a [`Forward`] line to work on another of
//! the person's devices (`tilo agent connect --device`): it is served MCP here, every request it
//! makes sent to that device as one naming it ([`DeviceActions::forward`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_action_policy::AgentSessionId;
use tilo_actions::remote::{ActionCallError, Caller, Completer, Pending, pending};
use tilo_broker_core::services::AgentSessions as _;
use tilo_lock::MutexRecover as _;
use tilo_mcp::devices::{DeviceDirectory, Forward, Reach};
use tilo_peer_state::actions::ActionService;
use tilo_service::Wake;
use tilo_user_identity::DeviceCert;

use crate::agent_sessions::AgentSessions;
use crate::device_actions::DeviceActions;
use crate::i18n::t;

/// How long an agent session from another device stays here after its last request: its client
/// leaving says nothing to this device.
const IDLE: Duration = Duration::from_secs(600);

/// Whether the person lets agents from their other devices work here, as this device's settings
/// say now.
pub(crate) fn accepts_own_devices() -> bool {
    settings_store::mcp_server::accepts_own_devices(&settings_store::load())
}

/// Whether the person lets such an agent take a step it would ask them about here without
/// asking, as this device's settings say now: only while they let it work here too.
pub(crate) fn acts_without_asking() -> bool {
    settings_store::mcp_server::acts_without_asking(&settings_store::load())
}

/// Whether `caller` is an agent another of the person's devices handed its requests to.
pub(crate) fn handed(caller: &Caller) -> bool {
    caller.is_session_delegated()
        && caller.acting().is_agent()
        && caller.device_public_key().is_some()
}

/// What the session's MCP server is asked for an agent another device hands requests to: the
/// person's agent member for its client in `workspace`, and an agent session of it here, from
/// `origin`; or why it does not work here.
pub(crate) struct HandedAsk {
    pub(crate) workspace: String,
    pub(crate) client: String,
    pub(crate) origin: tilo_actions::InvokerDevice,
    pub(crate) answer: Completer<Result<(AgentSessionId, String), String>>,
}

/// The agents the person's other devices hand requests to, as this device works with them: each
/// session one of those devices names is an agent session here.
pub(crate) struct HandedAgents {
    /// What the MCP server is asked on its next pass.
    asks: Mutex<Vec<HandedAsk>>,
    /// Starts that pass.
    wake: Wake,
    sessions: Arc<AgentSessions>,
    /// Each session a device named, by that device and the session's name there.
    seen: Mutex<HashMap<([u8; 32], String), Seen>>,
    /// This device's action service, whose rosters name the device an agent came from.
    actions: ActionService,
    /// Where a request goes on once its agent works here, off the session's own thread.
    runtime: tokio::runtime::Handle,
}

/// Where a session another device named stands here.
enum Seen {
    /// The MCP server is asked about it; the requests that wait for it.
    Asking(Vec<(Caller, Completer<Result<Caller, ActionCallError>>)>),
    /// It works here as `session` of the agent member `agent` in `workspace`.
    Working {
        workspace: String,
        agent: String,
        session: AgentSessionId,
        used: Instant,
    },
    /// The person stopped it here.
    Stopped { used: Instant },
}

/// A handed caller, as this device works with it: now, or once the MCP server answered.
pub(crate) enum Working {
    Now(Box<Result<Caller, ActionCallError>>),
    Later(Pending<Result<Caller, ActionCallError>>),
}

impl HandedAgents {
    pub(crate) fn new(
        wake: Wake,
        sessions: Arc<AgentSessions>,
        actions: ActionService,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        Arc::new(Self {
            asks: Mutex::default(),
            wake,
            sessions,
            seen: Mutex::default(),
            actions,
            runtime,
        })
    }

    /// What the MCP server is asked now.
    pub(crate) fn take(&self) -> Vec<HandedAsk> {
        std::mem::take(&mut *self.asks.lock_recover())
    }

    /// End the sessions no request came for in a while, and forget those stopped as long ago.
    pub(crate) fn sweep(&self) {
        let now = Instant::now();
        let mut ended = Vec::new();
        self.seen.lock_recover().retain(|_, seen| match seen {
            Seen::Working { session, used, .. } if now.duration_since(*used) > IDLE => {
                ended.push(session.clone());
                false
            }
            Seen::Stopped { used } => now.duration_since(*used) <= IDLE,
            _ => true,
        });
        for session in ended {
            self.sessions.left(&session);
        }
    }

    /// `caller`, a handed agent ([`handed`]), as it works here: the person's agent member in its
    /// session here; refused while this device does not accept agents from the person's other
    /// devices, and once the person stopped its session.
    pub(crate) fn working(self: &Arc<Self>, caller: &Caller) -> Working {
        let (Some(device), Some(workspace), Some(named)) = (
            caller.device_public_key().copied(),
            caller.workspace(),
            caller.acting().agent_session.clone(),
        ) else {
            return Working::Now(Box::new(Err(ActionCallError::NotPermitted)));
        };
        if !accepts_own_devices() {
            return Working::Now(Box::new(Err(not_accepted())));
        }
        let now = Instant::now();
        let key = (device, named.as_str().to_owned());
        let mut seen = self.seen.lock_recover();
        match seen.get_mut(&key) {
            Some(Seen::Working {
                workspace: at,
                agent,
                session,
                used,
            }) => {
                if at != workspace {
                    return Working::Now(Box::new(Err(ActionCallError::InvalidRequest {
                        reason: "an agent session works in one workspace".into(),
                    })));
                }
                if self.sessions.going(at, agent, session.as_str()).is_none() {
                    seen.insert(key, Seen::Stopped { used: now });
                    return Working::Now(Box::new(Err(stopped())));
                }
                *used = now;
                Working::Now(Box::new(Ok(caller
                    .clone()
                    .handed_as(agent.clone(), session.as_str()))))
            }
            Some(Seen::Stopped { used }) => {
                *used = now;
                Working::Now(Box::new(Err(stopped())))
            }
            Some(Seen::Asking(waiting)) => {
                let (done, answer) = pending();
                waiting.push((caller.clone(), done));
                Working::Later(answer)
            }
            None => {
                let (done, answer) = pending();
                seen.insert(key.clone(), Seen::Asking(vec![(caller.clone(), done)]));
                drop(seen);
                self.ask(key, workspace.to_owned(), caller);
                Working::Later(answer)
            }
        }
    }

    /// `caller` as it works here already, for a read that does not wait: unchanged until its
    /// session works here.
    pub(crate) fn known(&self, caller: &Caller) -> Caller {
        let (Some(device), Some(named)) = (
            caller.device_public_key(),
            caller.acting().agent_session.as_ref(),
        ) else {
            return caller.clone();
        };
        match self
            .seen
            .lock_recover()
            .get(&(*device, named.as_str().to_owned()))
        {
            Some(Seen::Working { agent, session, .. }) => {
                caller.clone().handed_as(agent.clone(), session.as_str())
            }
            _ => caller.clone(),
        }
    }

    /// Run `act` as `caller` works here ([`Self::working`]), on the runtime once that waited.
    pub(crate) fn through<T: Send + 'static>(
        self: &Arc<Self>,
        caller: &Caller,
        act: impl FnOnce(&Caller) -> Pending<Result<T, ActionCallError>> + Send + 'static,
    ) -> Pending<Result<T, ActionCallError>> {
        match self.working(caller) {
            Working::Now(now) => match *now {
                Ok(caller) => act(&caller),
                Err(error) => Pending::ready(Err(error)),
            },
            Working::Later(later) => {
                let (done, answer) = pending();
                let runtime = self.runtime.clone();
                later.then(move |working| match working {
                    Some(Ok(caller)) => {
                        runtime.spawn(async move {
                            act(&caller)
                                .then(move |answer| done.complete(answer.unwrap_or_else(gone)));
                        });
                    }
                    Some(Err(error)) => done.complete(Err(error)),
                    None => done.complete(gone()),
                });
                answer
            }
        }
    }

    /// Ask the MCP server about the session `key` names in `workspace`, for its first request,
    /// `caller`'s, naming the device it came from as the workspace's roster does.
    fn ask(self: &Arc<Self>, key: ([u8; 32], String), workspace: String, caller: &Caller) {
        let client = caller.agent_name().unwrap_or_default().to_owned();
        let this = Arc::clone(self);
        self.runtime.spawn(async move {
            let label = this
                .actions
                .certificates(&workspace)
                .await
                .unwrap_or_default()
                .into_iter()
                .find(|certificate| certificate.device_public_key == key.0)
                .map(|certificate| certificate.device_label);
            let origin =
                tilo_actions::InvokerDevice::certified(key.0, label.as_deref().unwrap_or_default());
            let (answer, answered) = pending();
            this.asks.lock_recover().push(HandedAsk {
                workspace: workspace.clone(),
                client,
                origin,
                answer,
            });
            (this.wake)();
            answered.then(move |answer| this.settle(key, workspace, answer.as_ref()));
        });
    }

    /// The MCP server's `answer` about the session `key` names: the requests that waited for it go
    /// on, or are refused with why.
    fn settle(
        &self,
        key: ([u8; 32], String),
        workspace: String,
        answer: Option<&Result<(AgentSessionId, String), String>>,
    ) {
        let waiting = {
            let mut seen = self.seen.lock_recover();
            let Some(Seen::Asking(waiting)) = seen.remove(&key) else {
                return;
            };
            if let Some(Ok((session, agent))) = answer {
                seen.insert(
                    key,
                    Seen::Working {
                        workspace,
                        agent: agent.clone(),
                        session: session.clone(),
                        used: Instant::now(),
                    },
                );
            }
            waiting
        };
        for (caller, done) in waiting {
            done.complete(match answer {
                Some(Ok((session, agent))) => Ok(caller.handed_as(agent.clone(), session.as_str())),
                Some(Err(reason)) => Err(ActionCallError::Refused {
                    reason: reason.clone(),
                }),
                None => gone(),
            });
        }
    }
}

/// Why an agent from another of the person's devices does not work here: this device's setting.
pub(crate) fn not_accepted() -> ActionCallError {
    ActionCallError::Refused {
        reason: t("device-agents-not-accepted"),
    }
}

/// Why an agent the person stopped here is refused.
fn stopped() -> ActionCallError {
    ActionCallError::Refused {
        reason: t("device-agent-stopped"),
    }
}

/// A request whose answer was dropped on this device.
fn gone<T>() -> Result<T, ActionCallError> {
    Err(ActionCallError::Unavailable {
        reason: "this device dropped the request before it answered".into(),
    })
}

impl DeviceActions {
    /// Serve `stream`, a client of this login that asked to work on the device `forward` names as
    /// `person`'s agent: MCP here, every request it makes sent to that device naming it, which
    /// decides each. Refused, with why, where no device of the person's goes by that name.
    pub(crate) fn forward(&self, mut stream: UnixStream, forward: &Forward, person: &str) {
        let (device, workspace) = match self.runtime().block_on(self.target(forward)) {
            Ok(target) => target,
            Err(reason) => {
                tracing::info!(device = %forward.device, %reason, "MCP server: a client is not carried to another device");
                tilo_mcp::devices::refuse_forward(stream, reason);
                return;
            }
        };
        if let Err(error) = tilo_mcp::devices::open_forward(&mut stream) {
            tilo_failure::ignored!(
                "a client that left before it was carried needs no answer",
                tracing::debug!(%error, "MCP server: the client left before it was carried")
            );
            return;
        }
        let session = format!("device-agent-{}", uuid::Uuid::new_v4().simple());
        let directory = Arc::new(self.clone()) as Arc<dyn DeviceDirectory>;
        let served = tilo_mcp::session_api::serve_as(stream, None, |hello, _| {
            let agent = neighbour_wire::actions::HandedAgent {
                client: hello.client_name.clone(),
                session,
            };
            let remote = self.remote_for_agent(device, &workspace, agent)?;
            let id = tilo_mcp::session_api::proposed_agent_id(&hello.client_name);
            Ok(
                tilo_mcp::McpServer::for_agent(remote, workspace.clone(), &id, person)
                    .agent_named(&hello.client_name)
                    .with_devices(Some(directory)),
            )
        });
        if let Err(error) = served {
            tilo_failure::ignored!(
                "a client that disconnected needs no answer",
                tracing::debug!(%error, "MCP server: the carried client disconnected")
            );
        }
    }

    /// The device `forward` names and the workspace the agent works in there: the one it names,
    /// else the one of this device's workspaces that device is in. A device is named by its name
    /// or its key.
    async fn target(&self, forward: &Forward) -> Result<([u8; 32], String), String> {
        let asked = forward.device.trim();
        let key = tilo_failure::ignored!(
            "a device named by its name is not a key",
            hex::decode(asked).ok()
        )
        .and_then(|key| <[u8; 32]>::try_from(key).ok());
        if let (Some(key), Some(workspace)) = (key, &forward.workspace) {
            return Ok((key, workspace.clone()));
        }
        let workspaces = match &forward.workspace {
            Some(workspace) => vec![workspace.clone()],
            None => self.workspaces_here(),
        };
        let mut found = Vec::new();
        let mut known = BTreeSet::new();
        for workspace in workspaces {
            for device in self.devices_in(&workspace, false).await {
                let name = tilo_mcp::devices::device_name(&device);
                if key == Some(device.device_public_key) || name.eq_ignore_ascii_case(asked) {
                    found.push((device.device_public_key, workspace.clone()));
                }
                known.insert(name);
            }
        }
        match found.as_slice() {
            [one] => Ok(one.clone()),
            [] if known.is_empty() => Err(t("device-agents-no-device")),
            [] => Err(crate::i18n::t_args(
                "device-agents-unknown-device",
                &[
                    ("device", asked.to_owned().into()),
                    (
                        "devices",
                        known.into_iter().collect::<Vec<_>>().join(", ").into(),
                    ),
                ],
            )),
            _ => Err(crate::i18n::t_args(
                "device-agents-name-workspace",
                &[("device", asked.to_owned().into())],
            )),
        }
    }

    /// The person's devices in `workspace`'s roster as this device holds it, this one among them
    /// when `with_this`.
    async fn devices_in(&self, workspace: &str, with_this: bool) -> Vec<DeviceCert> {
        let (person, this) = self.whose();
        self.action_service()
            .certificates(workspace)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|device| {
                device.user_public_key == person && (with_this || device.device_public_key != this)
            })
            .collect()
    }
}

impl DeviceDirectory for DeviceActions {
    fn reach(&self) -> std::pin::Pin<Box<dyn Future<Output = Vec<Reach>> + Send + '_>> {
        Box::pin(async move {
            let this = self.whose().1;
            let mut reach: BTreeMap<[u8; 32], Reach> = BTreeMap::new();
            for workspace in self.workspaces_here() {
                for device in self.devices_in(&workspace, true).await {
                    let key = device.device_public_key;
                    let here = key == this || self.reachable(&workspace, key);
                    let answer = if here {
                        Ok(())
                    } else {
                        Err(t("device-agents-not-connected"))
                    };
                    match reach.get_mut(&key) {
                        Some(known) if known.answer.is_err() => known.answer = answer,
                        Some(_) => {}
                        None => {
                            reach.insert(
                                key,
                                Reach {
                                    device,
                                    this: key == this,
                                    answer,
                                },
                            );
                        }
                    }
                }
            }
            reach.into_values().collect()
        })
    }
}
