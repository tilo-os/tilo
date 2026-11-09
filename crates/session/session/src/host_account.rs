//! The person's account with the host this session runs on, as a live read
//! ([`tilo_actions::remote::host_account::ACCOUNT`]).
//!
//! On a host that serves workspaces ([`tilo_host_account::HostedAccount`]),
//! the answer is what the host offers the session's account: its plans, the
//! account's standing, usage and invoices, read from the host's plans file
//! and its hosted-VM service, and sent again whenever the plans file
//! changes (a card added, a plan changed). Anywhere else the session runs on
//! one of the person's own devices, which sells nothing.
//!
//! Billing is the person's own: an agent never reads it here. It learns what
//! it needs from the refusal of the action it took.

use std::sync::{Arc, Mutex};

use tilo_action_broker::services::{HostStepOutcome, HostStepRequest, HostStepServices, LiveView};
use tilo_actions::remote::Pending;
use tilo_actions::remote::host_account::{self as wire, HostAccount, HostKind};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, OperationKind,
};
use tilo_host_account::HostedAccount;
use tilo_lock::MutexRecover as _;

/// Where the account is read from.
enum Source {
    Hosted(HostedAccount),
    /// One of the person's devices, by its name.
    Device(String),
}

/// The broker's door to the person's account with this host.
pub(crate) struct HostAccountDoor {
    source: Source,
    changes: tokio::sync::watch::Sender<u64>,
    /// Held while the session runs: it moves `changes`.
    watch: Mutex<Option<tilo_files::native::ChangeWatch>>,
}

impl HostAccountDoor {
    /// The door of this session's host, as its environment names it.
    pub(crate) fn session() -> Arc<Self> {
        let source = match HostedAccount::from_env() {
            Some(hosted) => Source::Hosted(hosted),
            None => Source::Device(device_name()),
        };
        Self::of(source)
    }

    fn of(source: Source) -> Arc<Self> {
        let door = Arc::new(Self {
            source,
            changes: tokio::sync::watch::channel(0).0,
            watch: Mutex::new(None),
        });
        if let Source::Hosted(hosted) = &door.source {
            let changes = door.changes.clone();
            match hosted.watch(move || {
                changes.send_modify(|revision| *revision = revision.wrapping_add(1));
            }) {
                Ok(watch) => *door.watch.lock_recover() = Some(watch),
                Err(error) => {
                    tracing::warn!(%error, "host account: the plans file is not followed");
                }
            }
        }
        door
    }

    /// The account as the host knows it now.
    fn read(&self) -> HostAccount {
        match &self.source {
            Source::Hosted(hosted) => hosted.read(tilo_clock::unix_now_ms()),
            Source::Device(name) => HostAccount::sells_nothing(name.clone(), HostKind::Device),
        }
    }
}

/// This device's name, as its host name gives it.
fn device_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        // ignored-failure: a device without a host name is still this device, said plainly.
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "This device".to_owned())
}

fn descriptor() -> ActionDescriptor {
    ActionDescriptor {
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

fn person_only(request: &HostStepRequest<'_>) -> Result<(), String> {
    request
        .actor
        .direct_person()
        .map(|_| ())
        .ok_or_else(|| "only the person reads their hosting account".to_owned())
}

fn answer(account: &HostAccount) -> serde_json::Value {
    serde_json::to_value(account).unwrap_or_default()
}

impl HostStepServices for HostAccountDoor {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &str {
        settings_sections::APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        vec![descriptor()]
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        person_only(request)?;
        if request.address.action != wire::ACCOUNT {
            return Err("unknown hosting step".into());
        }
        Ok(Pending::ready(Ok(tilo_actions::ActionOutput {
            value: Some(answer(&self.read())),
            resources: Vec::new(),
        })))
    }

    fn watch(&self, request: &HostStepRequest<'_>) -> Result<LiveView, String> {
        person_only(request)?;
        if request.address.action != wire::ACCOUNT {
            return Err("this step has no live result".into());
        }
        let read = match &self.source {
            Source::Hosted(hosted) => {
                let hosted = hosted.clone();
                Box::new(move || {
                    // The service is asked over its socket: off the broker's thread.
                    let (answer_with, answered) = tilo_actions::remote::pending();
                    let hosted = hosted.clone();
                    std::thread::spawn(move || {
                        answer_with.complete(Ok(answer(&hosted.read(tilo_clock::unix_now_ms()))));
                    });
                    answered
                }) as Box<dyn FnMut() -> Pending<_> + Send>
            }
            Source::Device(name) => {
                let account = answer(&HostAccount::sells_nothing(name.clone(), HostKind::Device));
                Box::new(move || Pending::ready(Ok(account.clone())))
            }
        };
        Ok(LiveView {
            read,
            changes: self.changes.subscribe(),
        })
    }
}

#[cfg(test)]
mod tests;

/// The person's account with this session's host, as the overview of their Tilo shows it: read on
/// a worker of its own whenever the host's plans file changes, so the frame never waits on the
/// hosted-VM service.
struct Followed {
    account: tilo_files::native::Watched<HostAccount>,
    /// The answer the chrome was last handed.
    shown: Mutex<Option<std::sync::Arc<HostAccount>>>,
}

fn followed() -> Option<&'static Followed> {
    static FOLLOWED: std::sync::OnceLock<Option<Followed>> = std::sync::OnceLock::new();
    FOLLOWED
        .get_or_init(|| {
            let watched = if let Some(hosted) = HostedAccount::from_env() {
                let dir = hosted.plans.parent().map_or_else(
                    || std::path::PathBuf::from("."),
                    std::path::Path::to_path_buf,
                );
                let unread = HostAccount::sells_nothing(hosted.name.clone(), HostKind::Provider);
                tilo_files::native::Watched::start_unread(
                    "host-account",
                    vec![dir],
                    unread,
                    move || hosted.read(tilo_clock::unix_now_ms()),
                )
            } else {
                let device = HostAccount::sells_nothing(device_name(), HostKind::Device);
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
pub(crate) fn changed() -> Option<HostAccount> {
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
