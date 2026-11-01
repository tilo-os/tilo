//! The service over logind and `UPower`, published beside the session's
//! other services: `power:sleep` (the policy and the holds) and
//! `power:battery` (the pack as it reads).

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_service::{Service, Started, decode, offer, reading};

use crate::PowerLink;
use crate::effects::{BatteryAction, SleepAction};

/// The resource holding the policy in force and what keeps the machine
/// awake.
pub const SLEEP: &str = "power:sleep";
/// The kind of [`SLEEP`].
pub const SLEEP_KIND: &str = "so.tilo.power.sleep";
/// The resource holding the battery's charge.
pub const BATTERY: &str = "power:battery";
/// The kind of [`BATTERY`].
pub const BATTERY_KIND: &str = "power:battery";

/// The session's power service: the policy, and the machine's logind and
/// `UPower` behind it.
///
/// Dropping it stops the service with the runtime it runs on.
pub struct SessionPower {
    link: PowerLink,
    runtime: tokio::runtime::Runtime,
}

impl SessionPower {
    /// The service over this machine's system bus: logind for sleeping,
    /// `UPower` for the lid and the battery. The link is marked attached
    /// once the daemons answer; until then (or where they never do) the
    /// session keeps its policy and suspends nothing itself.
    ///
    /// # Errors
    /// The runtime it runs on did not start.
    pub fn start(link: PowerLink) -> std::io::Result<Self> {
        let runtime = runtime()?;
        let connecting = link.clone();
        runtime.spawn(async move {
            let policy = connecting.policy();
            match tilo_power_mgmt::PowerService::connect(
                connecting.bridge().clone(),
                connecting.registry().clone(),
                policy,
            )
            .await
            {
                Ok(service) => {
                    connecting.attached();
                    service.run().await;
                }
                Err(error) => tracing::info!(
                    target: "tilo::power",
                    %error,
                    "no logind here: this session keeps its idle policy but sleeps nothing",
                ),
            }
        });
        Ok(Self { link, runtime })
    }

    /// The service over `connection`, where logind and `UPower` answer: a
    /// test's stand-in bus, or the system bus another part already reached.
    ///
    /// # Errors
    /// The runtime it runs on did not start, or the daemons do not answer
    /// on `connection`.
    pub fn with_connection(connection: zbus::Connection, link: PowerLink) -> std::io::Result<Self> {
        let runtime = runtime()?;
        let connecting = link.clone();
        let policy = link.policy();
        let service = runtime
            .block_on(tilo_power_mgmt::PowerService::with_connection(
                &connection,
                connecting.bridge().clone(),
                connecting.registry().clone(),
                policy,
            ))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        link.attached();
        runtime.spawn(async move {
            let _connection = connection;
            service.run().await;
        });
        Ok(Self { link, runtime })
    }

    /// The policy and its resources with nothing behind them: no logind and
    /// no `UPower`, so nothing here sleeps, locks or reads a pack. What a
    /// test drives, and what a session on a machine with neither falls back
    /// to — its own actions are then what sleeping is.
    ///
    /// # Errors
    /// The runtime it runs on did not start.
    pub fn without_daemons(link: PowerLink) -> std::io::Result<Self> {
        Ok(Self {
            link,
            runtime: runtime()?,
        })
    }

    /// The policy and the mailbox behind it.
    #[must_use]
    pub const fn link(&self) -> &PowerLink {
        &self.link
    }

    /// The runtime it runs on.
    #[must_use]
    pub const fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("tilo-power")
        .enable_all()
        .build()
}

/// The `power:sleep` resource, with the actions a person can take on it.
#[must_use]
pub fn sleep_resource() -> ResourceSpec {
    offer::<SleepAction>(ResourceSpec::new(SLEEP, SLEEP_KIND, "Power and sleep"))
}

/// The `power:battery` resource: the pack as it reads, and the charge limit
/// where its driver offers one.
#[must_use]
pub fn battery_resource() -> ResourceSpec {
    offer::<BatteryAction>(ResourceSpec::new(BATTERY, BATTERY_KIND, "Battery"))
}

impl Service for SessionPower {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![sleep_resource(), battery_resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            SLEEP => Some(reading(&self.link.sleep())),
            BATTERY => Some(reading(&self.link.battery())),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource == BATTERY {
            let published = self.resources();
            let action = decode::<BatteryAction>(&published, resource, action, arguments)?;
            match action {
                BatteryAction::SetChargeLimit { percent } => {
                    if self.link.battery().charge_limit.is_none() {
                        return Err(DoorError::Unavailable {
                            code: "no-charge-limit".into(),
                            reason: "this battery's driver offers no charge limit".into(),
                        });
                    }
                    let limit = self
                        .link
                        .set_charge_limit(percent)
                        .map_err(DoorError::InvalidArguments)?;
                    return Ok(Started::Done(ActionOutput {
                        value: Some(
                            serde_json::to_value(&limit)
                                .map_err(|error| DoorError::InvalidArguments(error.to_string()))?,
                        ),
                        resources: Vec::new(),
                    }));
                }
            }
        }
        if resource != SLEEP {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        let published = self.resources();
        let action = decode::<SleepAction>(&published, resource, action, arguments)?;
        match action {
            SleepAction::KeepAwake { reason } => {
                let reason = reason.trim();
                if reason.is_empty() {
                    return Err(DoorError::InvalidArguments(
                        "a hold says why the machine must stay awake".into(),
                    ));
                }
                let ticket = self.link.keep_awake(reason);
                Ok(Started::Done(ActionOutput {
                    value: Some(serde_json::json!({ "ticket": ticket.to_string() })),
                    resources: Vec::new(),
                }))
            }
            SleepAction::Release { ref ticket } => {
                let ticket = ticket.trim().parse::<u64>().map_err(|_| {
                    DoorError::InvalidArguments(format!("{ticket} is no hold of this session"))
                })?;
                if !self.link.release(ticket) {
                    return Err(DoorError::Unknown(ActionAddress::new(
                        resource,
                        action.id(),
                    )));
                }
                Ok(Started::Done(ActionOutput::default()))
            }
            SleepAction::PowerKey => {
                let resolved = self.link.power_key().map(|action| self.link.act(action));
                Ok(done(resolved))
            }
            SleepAction::Lid { closed } => {
                let resolved = self.link.lid(closed).map(|action| self.link.act(action));
                Ok(done(resolved))
            }
        }
    }
}

/// What the policy resolved `action` to, as the output of an invocation
/// that only reports it: `null` when it resolves to nothing.
fn done(action: Option<&'static str>) -> Started {
    Started::Done(ActionOutput {
        value: action.map(|action| serde_json::json!({ "action": action })),
        resources: Vec::new(),
    })
}
