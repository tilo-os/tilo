//! Agents on the person's other devices working here, over the one action frame between devices.
//!
//! An agent on another of the person's devices (an MCP client there) works here as their agent:
//! that device names it on every request it sends
//! ([`HandedAgent`](neighbour_wire::actions::HandedAgent)), and this device's action service takes
//! each request as that agent's, for the person, never as the person themselves
//! ([`tilo_peer_state::actions`]). Here it is the person's agent member for its client in the
//! workspace, in an agent session of its own, which the shell lists with the device beside its
//! name and the person's Stop ends: from then on its requests are refused. It works through the
//! same broker connection an agent of this login's MCP server does, so its steps are decided as
//! any agent's are, by its grants, asking the person on this device's sheet where those say ask
//! ([`crate::step_questions`]). This device's settings decide whether such agents work here
//! at all ("Accept agents from my other devices") and whether their steps go unasked.
//!
//! The other way round, a client of this login that works on another of the person's devices
//! (`tilo agent connect --device`) is served as [`tilo_runtime::other_devices`] serves it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_action_policy::AgentSessionId;
use tilo_actions::remote::{ActionCallError, ActionProvider, Caller, Completer, Pending, pending};
use tilo_broker_core::services::AgentSessions as _;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::ActionService;
use tilo_service::Wake;

use crate::agent_sessions::AgentSessions;
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
    pub(crate) answer: Completer<Result<Handed, String>>,
}

/// What answers an agent another device hands its requests to.
pub(crate) type Host = Arc<dyn ActionProvider + Send + Sync>;

/// An agent another device hands its requests to, as the MCP server let it work here: the agent
/// member `agent` in its session `session`, acting as `caller` through `host`, as an agent of this
/// login's MCP server does.
pub(crate) struct Handed {
    pub(crate) session: AgentSessionId,
    pub(crate) agent: String,
    pub(crate) caller: Caller,
    pub(crate) host: Host,
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
    Asking(Vec<Completer<Result<(Caller, Host), ActionCallError>>>),
    /// It works here in `workspace`, as the MCP server let it.
    Working {
        workspace: String,
        handed: Box<Handed>,
        used: Instant,
    },
    /// The person stopped it here.
    Stopped { used: Instant },
}

/// A handed caller, as this device works with it: the caller it acts as here and what answers it,
/// now or once the MCP server answered.
pub(crate) enum Working {
    Now(Box<Result<(Caller, Host), ActionCallError>>),
    Later(Pending<Result<(Caller, Host), ActionCallError>>),
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
            Seen::Working { handed, used, .. } if now.duration_since(*used) > IDLE => {
                handed.caller.retire_execution();
                ended.push(handed.session.clone());
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
    /// session here, and what answers it; refused while this device does not accept agents from
    /// the person's other devices, and once the person stopped its session.
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
                handed,
                used,
            }) => {
                if at != workspace {
                    return Working::Now(Box::new(Err(ActionCallError::InvalidRequest {
                        reason: "an agent session works in one workspace".into(),
                    })));
                }
                if self
                    .sessions
                    .going(at, &handed.agent, handed.session.as_str())
                    .is_none()
                {
                    handed.caller.retire_execution();
                    seen.insert(key, Seen::Stopped { used: now });
                    return Working::Now(Box::new(Err(stopped())));
                }
                *used = now;
                Working::Now(Box::new(Ok((
                    handed.caller.clone(),
                    Arc::clone(&handed.host),
                ))))
            }
            Some(Seen::Stopped { used }) => {
                *used = now;
                Working::Now(Box::new(Err(stopped())))
            }
            Some(Seen::Asking(waiting)) => {
                let (done, answer) = pending();
                waiting.push(done);
                Working::Later(answer)
            }
            None => {
                let (done, answer) = pending();
                seen.insert(key.clone(), Seen::Asking(vec![done]));
                drop(seen);
                self.ask(key, workspace.to_owned(), caller);
                Working::Later(answer)
            }
        }
    }

    /// `caller` as it works here already, and what answers it, for a read that does not wait:
    /// `None` until its session works here.
    pub(crate) fn known(&self, caller: &Caller) -> Option<(Caller, Host)> {
        let device = caller.device_public_key()?;
        let named = caller.acting().agent_session.as_ref()?;
        match self
            .seen
            .lock_recover()
            .get(&(*device, named.as_str().to_owned()))
        {
            Some(Seen::Working { handed, .. }) => {
                Some((handed.caller.clone(), Arc::clone(&handed.host)))
            }
            _ => None,
        }
    }

    /// Run `act` as `caller` works here, on what answers it ([`Self::working`]), on the runtime
    /// once that waited.
    pub(crate) fn through<T: Send + 'static>(
        self: &Arc<Self>,
        caller: &Caller,
        act: impl FnOnce(&Host, &Caller) -> Pending<Result<T, ActionCallError>> + Send + 'static,
    ) -> Pending<Result<T, ActionCallError>> {
        match self.working(caller) {
            Working::Now(now) => match *now {
                Ok((caller, host)) => act(&host, &caller),
                Err(error) => Pending::ready(Err(error)),
            },
            Working::Later(later) => {
                let (done, answer) = pending();
                let runtime = self.runtime.clone();
                later.then(move |working| match working {
                    Some(Ok((caller, host))) => {
                        runtime.spawn(async move {
                            act(&host, &caller)
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
            answered.then(move |answer| this.settle(key, workspace, answer));
        });
    }

    /// The MCP server's `answer` about the session `key` names: the requests that waited for it go
    /// on, or are refused with why.
    fn settle(
        &self,
        key: ([u8; 32], String),
        workspace: String,
        answer: Option<Result<Handed, String>>,
    ) {
        let mut seen = self.seen.lock_recover();
        let Some(Seen::Asking(waiting)) = seen.remove(&key) else {
            if let Some(Ok(handed)) = answer {
                handed.caller.retire_execution();
                self.sessions.left(&handed.session);
            }
            return;
        };
        let answered = match answer {
            Some(Ok(handed)) => {
                let working = Ok((handed.caller.clone(), Arc::clone(&handed.host)));
                seen.insert(
                    key,
                    Seen::Working {
                        workspace,
                        handed: Box::new(handed),
                        used: Instant::now(),
                    },
                );
                working
            }
            Some(Err(reason)) => Err(ActionCallError::Refused { reason }),
            None => gone(),
        };
        drop(seen);
        for done in waiting {
            done.complete(answered.clone());
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
