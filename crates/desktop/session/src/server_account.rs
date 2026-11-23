//! The person's account with the server this session runs on, as a live read
//! ([`tilo_actions::remote::server_account::ACCOUNT`]).
//!
//! On a server that serves workspaces ([`tilo_server_account::ServerAccountSource`]),
//! the answer is what the server offers the session's account: its plans, the
//! account's subscription, usage and invoices, read from the server's plans file
//! and its hosted-VM service, and sent again whenever the plans file
//! changes (a card added, a plan changed). Anywhere else the session runs on
//! one of the person's own devices, which sells nothing.
//!
//! Billing is the person's own: an agent never reads it here. It learns what
//! it needs from the refusal of the action it took.

use std::sync::{Arc, Mutex};

use tilo_actions::remote::Pending;
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::server_account::{self as wire, ServerAccount, ServerKind};
use tilo_actions::remote::{
    ActionProvider, Admitted, Caller, InvokeRequest, LiveRead, LiveView, Outcome,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, OperationKind,
};
use tilo_broker_core::BrokerActor;
use tilo_lock::MutexRecover as _;
use tilo_server_account::ServerAccountSource;
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

/// Where the account is read from.
enum Source {
    Hosted(ServerAccountSource),
    /// One of the person's devices, by its name.
    Device(String),
}

/// The broker's API to the person's account with this host.
pub(crate) struct ServerAccountApi {
    source: Source,
    changes: tokio::sync::watch::Sender<u64>,
    /// Held while the session runs: it moves `changes`.
    watch: Mutex<Option<tilo_files::native::ChangeWatch>>,
}

impl ServerAccountApi {
    /// The API of this session's host, as its environment names it.
    pub(crate) fn session() -> Arc<Self> {
        let source = match ServerAccountSource::from_env() {
            Some(hosted) => Source::Hosted(hosted),
            None => Source::Device(device_name()),
        };
        Self::of(source)
    }

    fn of(source: Source) -> Arc<Self> {
        let api = Arc::new(Self {
            source,
            changes: tokio::sync::watch::channel(0).0,
            watch: Mutex::new(None),
        });
        if let Source::Hosted(hosted) = &api.source {
            let changes = api.changes.clone();
            match hosted.watch(move || {
                changes.send_modify(|revision| *revision = revision.wrapping_add(1));
            }) {
                Ok(watch) => *api.watch.lock_recover() = Some(watch),
                Err(error) => {
                    tracing::warn!(%error, "host account: the plans file is not followed");
                }
            }
        }
        api
    }

    /// The account as the host knows it now.
    fn read(&self) -> ServerAccount {
        match &self.source {
            Source::Hosted(hosted) => hosted.read(tilo_clock::unix_now_ms()),
            Source::Device(name) => ServerAccount::sells_nothing(name.clone(), ServerKind::Device),
        }
    }

    /// Answer the account's read from now on, admitted by the grants of the app that shows it,
    /// for the person at this device alone.
    pub(crate) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(
            wire::PROVIDER,
            settings_sections::APP_ID,
            Reach::ThisDevice,
            self,
        );
    }
}

/// This device's name, as its host name gives it, said plainly without one.
fn device_name() -> String {
    tilo_desktop::about::device_name().unwrap_or_else(|| "This device".to_owned())
}

fn descriptor() -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(wire::RESOURCE, wire::ACCOUNT),
        resource_kind: wire::RESOURCE.into(),
        resource_label: "Hosting account".into(),
        action_label: "Account".into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: ExecutionLocus::default(),
        kind: OperationKind::View,
        contract: ActionContract::live_view(serde_json::json!({ "type": "object" })),
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

fn person_only(caller: &Caller) -> Result<(), String> {
    BrokerActor::for_caller(caller)
        .direct_person()
        .map(|_| ())
        .ok_or_else(|| "only the person reads their hosting account".to_owned())
}

fn answer(account: &ServerAccount) -> serde_json::Value {
    serde_json::to_value(account).unwrap_or_default()
}

impl tilo_actions::remote::Admits for ServerAccountApi {
    const PROVIDER: &'static str = wire::PROVIDER;
    const APP: &'static str = settings_sections::APP_ID;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(vec![descriptor()])
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        person_only(caller)?;
        if request.target.address.action != wire::ACCOUNT {
            return Err("unknown hosting step".into());
        }
        Ok(Pending::ready(Ok(tilo_actions::ActionOutput {
            value: Some(answer(&self.read())),
            resources: Vec::new(),
        })))
    }

    fn live(&self, caller: &Caller, request: &InvokeRequest) -> Result<LiveView, String> {
        person_only(caller)?;
        if request.target.address.action != wire::ACCOUNT {
            return Err("this step has no live result".into());
        }
        let read = match &self.source {
            Source::Hosted(hosted) => {
                let hosted = hosted.clone();
                Box::new(move || {
                    // The service is asked over its socket: off the calling thread.
                    let (answer_with, answered) = tilo_actions::remote::pending();
                    let hosted = hosted.clone();
                    std::thread::spawn(move || {
                        answer_with.complete(Ok(answer(&hosted.read(tilo_clock::unix_now_ms()))));
                    });
                    answered
                }) as Box<dyn FnMut() -> Pending<LiveRead> + Send>
            }
            Source::Device(name) => {
                let account = answer(&ServerAccount::sells_nothing(
                    name.clone(),
                    ServerKind::Device,
                ));
                Box::new(move || Pending::ready(Ok(account.clone())))
            }
        };
        let changes = self.changes.subscribe();
        Ok(LiveView {
            read,
            changes: Box::new(move || {
                let mut receiver = changes.clone();
                receiver.borrow_and_update();
                let (woken, awaited) = tilo_actions::remote::pending();
                drop(tilo_rt::app().spawn(async move {
                    if receiver.changed().await.is_ok() {
                        woken.complete(());
                    }
                }));
                awaited
            }),
        })
    }
}

#[cfg(test)]
mod tests;

/// The person's account with this session's host, as the overview of their Tilo shows it: read on
/// a worker of its own whenever the host's plans file changes, so the frame never waits on the
/// hosted-VM service.
struct Followed {
    account: tilo_files::native::Watched<ServerAccount>,
    /// The answer the chrome was last handed.
    shown: Mutex<Option<std::sync::Arc<ServerAccount>>>,
}

fn followed() -> Option<&'static Followed> {
    static FOLLOWED: std::sync::OnceLock<Option<Followed>> = std::sync::OnceLock::new();
    FOLLOWED
        .get_or_init(|| {
            let watched = if let Some(hosted) = ServerAccountSource::from_env() {
                let dir = hosted.plans.parent().map_or_else(
                    || std::path::PathBuf::from("."),
                    std::path::Path::to_path_buf,
                );
                let unread =
                    ServerAccount::sells_nothing(hosted.name.clone(), ServerKind::Provider);
                tilo_files::native::Watched::start_unread(
                    "host-account",
                    vec![dir],
                    unread,
                    move || hosted.read(tilo_clock::unix_now_ms()),
                )
            } else {
                let device = ServerAccount::sells_nothing(device_name(), ServerKind::Device);
                tilo_files::native::Watched::start_unread(
                    "host-account",
                    Vec::new(),
                    device.clone(),
                    move || device.clone(),
                )
            };
            match watched {
                Ok(account) => Some(Followed {
                    account,
                    shown: Mutex::new(None),
                }),
                Err(error) => {
                    tracing::warn!(%error, "host account: not followed for the overview");
                    None
                }
            }
        })
        .as_ref()
}

/// The account's answer when it changed since the chrome was last handed one.
pub(crate) fn changed() -> Option<ServerAccount> {
    let followed = followed()?;
    let now = followed.account.get();
    let previous = followed
        .shown
        .lock_recover()
        .replace(std::sync::Arc::clone(&now));
    if previous.is_some_and(|shown| std::sync::Arc::ptr_eq(&shown, &now)) {
        return None;
    }
    Some((*now).clone())
}
