//! What a device's own workspace host answers a key of its person, beside its
//! governance: the record actions of the workspaces it hosts.
//!
//! Every Tilo host has one external surface. A key of the person (another of
//! their devices, an agent's grant, a server key of theirs) opens a session
//! with the host, enters a workspace there, and runs that workspace's actions
//! addressed to the host's own device. A server answers them with its app
//! workers; a person's device answers them with its own action broker, which
//! is what [`KeySessions`] puts behind the host
//! ([`ServiceConfig::apps`](tilo_workspace_service::service::ServiceConfig::apps)).
//!
//! Each key works in a run of its own ([`Sessions::open`]). Before every message the key is
//! checked against the person's devices, as their own workspace lists them
//! ([`PersonalDevices::admit`]: theirs, current, not revoked), and its grant's scope against
//! the workspace; a key that no longer holds loses its run. A key of the person's runs as an
//! agent of theirs, which the device's broker admits as it admits one at its MCP server.
//!
//! A device of another member of the workspace, which the workspace's roster admitted on the
//! link, runs as that member: what it asks is decided by the workspace's grants for them, and a
//! search answers what they may read, never what this device's person may. What is this
//! device's own ([`crate::personal_devices::own_devices_only`]) stays its person's.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tilo_actions::remote::{ActionCallError, ActionProvider, Caller, Pending, RemoteActionMessage};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::app_serving::AppActionServer;

use crate::personal_devices::{PersonalDevices, Refusal, Whose};
use collab_types::WorkspaceId;

mod pages;
pub use pages::{PageViews, Pages, PersonNotices};

/// The most sessions one device keeps for its person's keys at once.
const MAX_SESSIONS: usize = 256;

/// An action host every call of one run goes through, as the caller it
/// admitted.
pub type SessionHost = Arc<dyn ActionProvider + Send + Sync>;

/// A device's action broker as a key's sessions reach it.
pub trait Sessions: Send + Sync {
    /// The workspace (its UUID) the host's session calls `workspace_id`, when
    /// this device answers its actions.
    fn workspace(&self, workspace_id: WorkspaceId) -> Option<String>;

    /// A session for `key`, `whose` it is, in `workspace`: the host its calls go through and
    /// the caller they go as. A key of the person's is admitted as the broker admits an agent
    /// at this device's MCP server; a member's device as that member, its calls decided by
    /// the workspace's grants for them. A refusal says why.
    ///
    /// # Errors
    /// The workspace does not admit a session for the key.
    fn open(
        &self,
        workspace: &str,
        key: &DeviceCert,
        whose: Whose,
    ) -> Result<(SessionHost, Caller), String>;

    /// `workspace`'s dispatch, which admits each call as the caller it carries: what a web page
    /// of the workspace asks as its person ([`Pages`]). `None` when this device answers no
    /// actions there.
    fn dispatch(&self, workspace: &str) -> Option<SessionHost>;
}

/// The record actions of the workspaces a device hosts, answered for its
/// person's keys by its own broker: the device's
/// [`AppActionServer`](tilo_workspace_service::app_serving::AppActionServer).
pub struct KeySessions {
    /// The person's devices and the broker, once they run: a host opens
    /// before the store and the broker it serves.
    serving: OnceLock<Serving>,
    open: Arc<OpenSessions>,
    /// The workspaces' apps as web pages, served from the same dispatch.
    pages: Pages,
}

struct Serving {
    devices: Arc<PersonalDevices>,
    sessions: Arc<dyn Sessions>,
}

struct Session {
    cert: DeviceCert,
    host: SessionHost,
    caller: Caller,
}

/// The sessions open, by workspace and key.
type OpenSessions = std::sync::Mutex<HashMap<(WorkspaceId, [u8; 32]), Session>>;

/// What the session a key works in needs, to be opened: a key's first call, or its first under
/// a renewed certificate.
struct Opening {
    sessions: Arc<dyn Sessions>,
    workspace: String,
    workspace_id: WorkspaceId,
    key: DeviceCert,
    whose: Whose,
}

/// The session a key's call runs in: open, or opened on a thread of its own because the call
/// came on the app runtime's thread, which never waits on the workspaces it is admitted by.
enum Run {
    Open(SessionHost, Caller),
    Opening(Opening),
}

impl Opening {
    /// Open the session and keep it in `open`: the one another call opened meanwhile, when one
    /// did, and this one retired.
    fn open(self, open: &OpenSessions) -> Result<(SessionHost, Caller), ActionCallError> {
        let (host, caller) = self
            .sessions
            .open(&self.workspace, &self.key, self.whose)
            .map_err(|reason| ActionCallError::Refused { reason })?;
        let mut open = open.lock_recover();
        let at = (self.workspace_id, self.key.device_public_key);
        if let Some(run) = open.get(&at)
            && run.cert == self.key
        {
            let kept = (Arc::clone(&run.host), run.caller.clone());
            drop(open);
            host.retire_execution(&caller);
            return Ok(kept);
        }
        open.insert(
            at,
            Session {
                cert: self.key,
                host: Arc::clone(&host),
                caller: caller.clone(),
            },
        );
        drop(open);
        Ok((host, caller))
    }
}

impl KeySessions {
    /// Sessions nobody answers yet: every message is refused as not running
    /// until [`Self::serve`].
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            serving: OnceLock::new(),
            open: Arc::default(),
            pages: Pages::default(),
        })
    }

    /// Answer the keys `devices` says are the person's through `sessions` from
    /// now on. A second call is ignored.
    pub fn serve(&self, devices: Arc<PersonalDevices>, sessions: Arc<dyn Sessions>) {
        self.pages
            .serve(Arc::clone(&sessions), Arc::clone(&devices));
        let _ = self.serving.set(Serving { devices, sessions });
    }

    /// The workspaces' apps as web pages, which show apps' views through what
    /// [`Pages::show_through`] is handed.
    #[must_use]
    pub const fn pages_served(&self) -> &Pages {
        &self.pages
    }

    /// The session `key` works in on `workspace_id` to answer `message`, opened now if it has
    /// none, once its scope reaches the workspace and the key is the person's, or a member's
    /// asking for what is the workspace's: `caller` is who the workspace's roster admitted it
    /// as on the link.
    fn run(
        &self,
        workspace_id: WorkspaceId,
        (caller, key): (&Caller, &DeviceCert),
        message: &RemoteActionMessage,
    ) -> Result<Run, ActionCallError> {
        let Serving { devices, sessions } =
            self.serving
                .get()
                .ok_or_else(|| ActionCallError::Unavailable {
                    reason: "this device's actions are not running yet".into(),
                })?;
        let workspace = sessions
            .workspace(workspace_id)
            .ok_or(ActionCallError::UnknownAction)?;
        let whose = match devices.admit(key.device_public_key, key) {
            Ok(whose) => whose,
            Err(refusal) => {
                self.end(workspace_id, &key.device_public_key);
                return Err(ActionCallError::Refused {
                    reason: refusal.reason(),
                });
            }
        };
        if whose == Whose::Member {
            if crate::personal_devices::own_devices_only(message) {
                return Err(ActionCallError::Refused {
                    reason: Refusal::OtherPerson.reason(),
                });
            }
            // The member as the roster admitted their device: their person, on that device, and
            // never an agent or an app of theirs taken for them.
            let person = hex::encode(key.user_public_key);
            if !caller.acting().is_person()
                || caller.acting().app().is_some()
                || caller.acting().person_id() != Some(person.as_str())
                || caller.device_public_key() != Some(&key.device_public_key)
            {
                return Err(ActionCallError::Refused {
                    reason: "another person's agent or app acts on their own devices, not here"
                        .into(),
                });
            }
        }
        if key
            .capabilities
            .agent
            .as_ref()
            .is_some_and(|scope| !scope.reaches(&workspace))
            || !key.capabilities.reaches_workspace(&workspace)
        {
            return Err(ActionCallError::Refused {
                reason: Refusal::OutOfScope.reason(),
            });
        }
        let mut open = self.open.lock_recover();
        if let Some(run) = open.get(&(workspace_id, key.device_public_key))
            && run.cert == *key
        {
            return Ok(Run::Open(Arc::clone(&run.host), run.caller.clone()));
        }
        // A key that renewed its certificate starts its run again under the
        // one it shows now.
        let renewed = open.remove(&(workspace_id, key.device_public_key));
        if let Some(run) = renewed {
            run.host.retire_execution(&run.caller);
        }
        if open.len() >= MAX_SESSIONS {
            return Err(ActionCallError::Unavailable {
                reason: "this device sessions as many of its person's keys as it takes".into(),
            });
        }
        drop(open);
        let opening = Opening {
            sessions: Arc::clone(sessions),
            workspace,
            workspace_id,
            key: key.clone(),
            whose,
        };
        // Opening admits the key in its workspace, which waits on the workspaces: never on the
        // app runtime's own thread, whose tasks that wait would hold.
        if tilo_rt::app().is_runtime_thread() {
            return Ok(Run::Opening(opening));
        }
        let (host, caller) = opening.open(&self.open)?;
        Ok(Run::Open(host, caller))
    }

    /// End the run `device` works in on `workspace_id`, if it has one.
    fn end(&self, workspace_id: WorkspaceId, device: &[u8; 32]) {
        let ended = self.open.lock_recover().remove(&(workspace_id, *device));
        if let Some(run) = ended {
            run.host.retire_execution(&run.caller);
        }
    }
}

impl AppActionServer for KeySessions {
    fn pages(&self) -> Option<&dyn tilo_workspace_service::app_serving::AppPages> {
        Some(&self.pages)
    }

    /// Only a key's certified session reaches a run: without the certificate
    /// there is no key to hold to its scope.
    fn serve(
        &self,
        _workspace_id: WorkspaceId,
        _caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        tilo_actions::remote::refuse(&message, ActionCallError::NotPermitted).map(Pending::ready)
    }

    fn serve_certified(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: &DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        match self.run(workspace_id, (caller, certificate), &message) {
            Ok(Run::Open(host, caller)) => tilo_actions::remote::serve(&host, &caller, message),
            Ok(Run::Opening(opening)) => {
                let open = Arc::clone(&self.open);
                let replies = tilo_actions::remote::refuse(
                    &message,
                    ActionCallError::Unavailable {
                        reason: String::new(),
                    },
                )
                .is_some();
                let (done, answer) = tilo_actions::remote::pending();
                std::thread::spawn(move || {
                    let served = match opening.open(&open) {
                        Ok((host, caller)) => tilo_actions::remote::serve(&host, &caller, message),
                        Err(refusal) => {
                            tilo_actions::remote::refuse(&message, refusal).map(Pending::ready)
                        }
                    };
                    if let Some(served) = served {
                        served.then(move |answered| {
                            if let Some(answered) = answered {
                                done.complete(answered);
                            }
                        });
                    }
                });
                replies.then_some(answer)
            }
            Err(refusal) => tilo_actions::remote::refuse(&message, refusal).map(Pending::ready),
        }
    }

    fn forget_workspace(&self, workspace_id: WorkspaceId) {
        let ended: Vec<Session> = {
            let mut open = self.open.lock_recover();
            let keys: Vec<_> = open
                .keys()
                .filter(|(workspace, _)| *workspace == workspace_id)
                .copied()
                .collect();
            let ended = keys.iter().filter_map(|key| open.remove(key)).collect();
            drop(open);
            ended
        };
        for run in ended {
            run.host.retire_execution(&run.caller);
        }
    }
}
