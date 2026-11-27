//! The person's account with the server this session runs on, as a live read
//! ([`tilo_action_services::server_account::ACCOUNT`]).
//!
//! On a server that serves workspaces ([`linux_vm::hosted::account::ServerAccountSource`]),
//! the answer is what the server offers the session's account: its plans, the
//! account's subscription, usage and invoices, read from the server's plans file
//! and its hosted-VM service, and sent again whenever the plans file
//! changes (a card added, a plan changed). Anywhere else the session runs on
//! one of the person's own devices, which sells nothing.
//!
//! Billing is the person's own: an agent never reads it here. It learns what
//! it needs from the refusal of the action it took.

use std::sync::{Arc, Mutex};

use linux_vm::hosted::account::ServerAccountSource;
use tilo_action_services::server_account::{self as wire, ServerAccount, ServerKind};
use tilo_actions::remote::Pending;
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{Admitted, Caller, InvokeRequest, LiveRead, LiveView, Outcome};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, OperationKind,
};
use tilo_broker_core::BrokerActor;
use tilo_lock::MutexRecover as _;

/// Where the account is read from.
#[derive(Clone)]
pub(crate) enum Source {
    Hosted(ServerAccountSource),
    /// One of the person's devices, by its name.
    Device(String),
}

impl Source {
    /// The host this session runs on, as its environment names it: a server that
    /// serves workspaces, or one of the person's own devices.
    pub(crate) fn session() -> Self {
        match ServerAccountSource::from_env() {
            Some(hosted) => Self::Hosted(hosted),
            None => Self::Device(device_name()),
        }
    }

    /// The account as this host answers it now.
    pub(crate) fn read(&self, now_ms: u64) -> ServerAccount {
        match self {
            Self::Hosted(hosted) => hosted.read(now_ms),
            Self::Device(name) => ServerAccount::sells_nothing(name.clone(), ServerKind::Device),
        }
    }

    /// [`Self::read`], or why the host could not be asked.
    pub(crate) fn try_read(&self, now_ms: u64) -> Result<ServerAccount, String> {
        match self {
            Self::Hosted(hosted) => hosted.try_read(now_ms),
            Self::Device(name) => Ok(ServerAccount::sells_nothing(
                name.clone(),
                ServerKind::Device,
            )),
        }
    }
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
        Self::of(Source::session())
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
        self.source.read(tilo_clock::unix_now_ms())
    }
}

/// This device's name, as its host name gives it, said plainly without one.
fn device_name() -> String {
    tilo_session_core::desktop::about::device_name().unwrap_or_else(|| "This device".to_owned())
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
