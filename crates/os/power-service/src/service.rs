//! The service over logind and `UPower`, published beside the session's
//! other services: `power:sleep` (the policy and the holds) and
//! `power:battery` (the pack as it reads).

use os_actions::battery::{BATTERY, BATTERY_KIND};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, ApiError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect as _;
use tilo_service::{Service, Started, decode, offer, reading};

use std::time::{Duration, Instant};

use crate::PowerLink;
use crate::effects::{BatteryAction, SleepAction};

/// The resource holding the policy in force and what keeps the machine
/// awake.
pub const SLEEP: &str = "power:sleep";
/// The kind of [`SLEEP`].
pub const SLEEP_KIND: &str = "so.tilo.power.sleep";

/// The resource holding the battery's readings over the last month, for a
/// chart over the day or the week.
pub const HISTORY: &str = "power:history";
/// The kind of [`HISTORY`].
pub const HISTORY_KIND: &str = "power:history";

/// The session's power service: the policy, and the machine's logind and
/// `UPower` behind it.
///
/// Dropping it stops the service with the runtime it runs on.
pub struct SessionPower {
    link: PowerLink,
    runtime: tokio::runtime::Runtime,
    /// How often the readings had moved when the session last read them.
    seen: u64,
    /// The history as it last read, with when it read it: the readings change
    /// every quarter hour, so reading the file on each pass would read it far
    /// more often than it changes.
    history: std::sync::Mutex<Option<(Instant, crate::History)>>,
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
        Ok(Self::over(link, runtime))
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
        Ok(Self::over(link, runtime))
    }

    /// The policy and its resources with nothing behind them: no logind and
    /// no `UPower`, so nothing here sleeps, locks or reads a pack. What a
    /// test drives, and what a session on a machine with neither falls back
    /// to — its own actions are then what sleeping is.
    ///
    /// # Errors
    /// The runtime it runs on did not start.
    pub fn without_daemons(link: PowerLink) -> std::io::Result<Self> {
        Ok(Self::over(link, runtime()?))
    }

    const fn over(link: PowerLink, runtime: tokio::runtime::Runtime) -> Self {
        Self {
            link,
            runtime,
            seen: 0,
            history: std::sync::Mutex::new(None),
        }
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

/// The `power:sleep` resource's declared type: the policy in force and what
/// keeps the machine awake.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.power.sleep", label = "Power and sleep")]
pub struct PowerSleep;

/// The `power:battery` resource's declared type: the pack as it reads.
#[derive(tilo_resource::Row)]
#[row(kind = "power:battery", label = "Battery")]
pub struct PowerBattery;

/// The `power:history` resource's declared type: the pack's readings over the
/// last month.
#[derive(tilo_resource::Row)]
#[row(kind = "power:history", label = "Battery history")]
pub struct PowerHistory;

/// How often the history is read again: its readings land every quarter hour,
/// and a pass reads it far more often than that.
const HISTORY_READ_EVERY: Duration = Duration::from_secs(60);

/// The `power:sleep` resource, with the actions a person can take on it.
#[must_use]
pub fn sleep_resource() -> ResourceSpec {
    offer::<SleepAction>(tilo_resource::typed_resource::<PowerSleep>(
        SLEEP,
        crate::i18n::name(SLEEP_KIND, "Power and sleep"),
    ))
}

/// The `power:battery` resource: the pack as it reads, and the charge limit
/// where its driver offers one.
#[must_use]
pub fn battery_resource() -> ResourceSpec {
    offer::<BatteryAction>(tilo_resource::typed_resource::<PowerBattery>(
        BATTERY,
        crate::i18n::name(BATTERY_KIND, "Battery"),
    ))
}

/// The battery as a view of another language reads and changes it: its reading
/// ([`crate::Battery`]) as its fields and its actions as its verbs.
#[must_use]
pub fn battery_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<crate::Battery, BatteryAction>("battery", "Battery")
}

/// The `power:history` resource: the pack's readings over the last month.
#[must_use]
pub fn history_resource() -> ResourceSpec {
    tilo_resource::typed_resource::<PowerHistory>(
        HISTORY,
        crate::i18n::name(HISTORY_KIND, "Battery history"),
    )
}

/// The battery's history as a view of another language reads it.
#[must_use]
pub fn battery_history_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<crate::History, tilo_resource::NoEffect>(
        "battery_history",
        "Battery history",
    )
}

impl SessionPower {
    /// The history as it reads now, re-read when the last read is a minute
    /// old.
    fn history_reading(&self) -> Value {
        let history = self.history_now();
        serde_json::to_value(&history).unwrap_or(Value::Null)
    }

    /// The history as it reads now, read again when the kept read is a minute
    /// old. The lock is released before the caller serialises it.
    fn history_now(&self) -> crate::History {
        let mut kept = self.history.lock_recover();
        if kept
            .as_ref()
            .is_none_or(|(read, _)| read.elapsed() >= HISTORY_READ_EVERY)
        {
            *kept = Some((
                Instant::now(),
                tilo_power_mgmt::load_battery_history().into(),
            ));
        }
        kept.as_ref().map_or_else(crate::History::default, |(_, history)| history.clone())
    }

    /// Whether the history is old enough to read again.
    fn history_due(&self) -> bool {
        let kept = self.history.lock_recover();
        kept.as_ref()
            .is_none_or(|(read, _)| read.elapsed() >= HISTORY_READ_EVERY)
    }
}

impl Service for SessionPower {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![sleep_resource(), battery_resource(), history_resource()]
    }

    /// The daemons answer on the service's own thread: the battery lands after
    /// the session first read it, and moves while it runs. The history is read
    /// again once a minute, so its chart fills in as the readings land.
    fn poll(&mut self) -> bool {
        let moved = self.link.bridge().readings_moved();
        std::mem::replace(&mut self.seen, moved) != moved || self.history_due()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            SLEEP => Some(reading(&self.link.sleep())),
            BATTERY => Some(reading(&self.link.battery())),
            HISTORY => Some(self.history_reading()),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource == BATTERY {
            let published = self.resources();
            let action = decode::<BatteryAction>(&published, resource, action, arguments)?;
            match action {
                BatteryAction::SetChargeLimit { percent } => {
                    if self.link.battery().charge_limit.is_none() {
                        return Err(ApiError::Unavailable {
                            code: "no-charge-limit".into(),
                            reason: "this battery's driver offers no charge limit".into(),
                        });
                    }
                    let limit = self
                        .link
                        .set_charge_limit(percent)
                        .map_err(ApiError::InvalidArguments)?;
                    return Ok(Started::Done(ActionOutput {
                        value: Some(
                            serde_json::to_value(&limit)
                                .map_err(|error| ApiError::InvalidArguments(error.to_string()))?,
                        ),
                        resources: Vec::new(),
                    }));
                }
            }
        }
        if resource != SLEEP {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        let published = self.resources();
        let action = decode::<SleepAction>(&published, resource, action, arguments)?;
        match action {
            SleepAction::KeepAwake { reason } => {
                let reason = reason.trim();
                if reason.is_empty() {
                    return Err(ApiError::InvalidArguments(
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
                    ApiError::InvalidArguments(format!("{ticket} is no hold of this session"))
                })?;
                if !self.link.release(ticket) {
                    return Err(ApiError::Unknown(ActionAddress::new(resource, action.id())));
                }
                Ok(Started::Done(ActionOutput::default()))
            }
            SleepAction::Sleep => {
                self.link.act(tilo_power_mgmt::PowerAction::Suspend);
                Ok(done(Some("suspend")))
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
