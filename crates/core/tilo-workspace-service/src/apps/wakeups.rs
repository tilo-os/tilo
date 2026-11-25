//! The moments an app's triggers land on this device (`trigger … at r.when on r.device`), armed
//! as the device's own wakeups: an alarm set for a phone rings there when the phone is offline,
//! asleep, or has no window of the app open.
//!
//! What lands here is read from the app's record as this device holds it
//! ([`crate::workspaces::Workspaces::app_wakeups`]), each moment with the notices the trigger's
//! run will write, and handed to the platform's notifications as one scheduled notification each
//! ([`Notifier::schedule`]): `AlarmManager.setAlarmClock` on Android, the session's durable
//! scheduler on a Linux desktop, and a host with none says so once and arms nothing. When the
//! record changes the moment moves, or is taken back.
//!
//! The device that runs the trigger still runs it once and writes the notice. A wakeup the
//! platform no longer holds rang (this device takes back only what it cancels), so the notice it
//! told is one this device does not show again when it arrives ([`Wakeups::look`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

pub use tilo_notify::{
    Notification, Notifier, NotifyError, Scheduled, SystemTime, set_activation_handler,
};

use crate::tl_records::{Package, Raised};
use crate::workspaces::Workspaces;

/// The scheme of a wakeup's buttons: `tilo-wakeup:<tag>?<action>@<ms>` runs the action the
/// notice offers, on what it opens, for the wakeup `tag` armed at the moment `ms`.
pub const ACTIVATION: &str = "tilo-wakeup";

/// One notice a moment tells, as it was handed to the platform.
#[derive(Clone, Debug)]
struct Armed {
    workspace: String,
    app: String,
    at_ms: i64,
    told: Raised,
    notification: Notification,
}

/// A notice this device rang on its own, by the workspace, the app and the notice record's id.
pub type Rang = (String, String, String);

/// This device's wakeups for the moments the apps' triggers land on it.
pub struct Wakeups {
    workspaces: Workspaces,
    notifier: Arc<dyn Notifier>,
    /// The person at this device, and the device, as the records name them.
    person: String,
    device: String,
    /// What the platform holds, by its tag.
    armed: BTreeMap<String, Armed>,
    /// What rang, by its tag: a button of it may still be pressed.
    rung: BTreeMap<String, Armed>,
    /// The package each app's record is read through here, by workspace and app.
    packages: BTreeMap<(String, String), Package>,
    /// Whether the platform said it keeps no wakeups, which is said once.
    unsupported: bool,
}

impl Wakeups {
    /// The wakeups of `person`'s device `device`, handed to `notifier`.
    #[must_use]
    pub fn new(
        workspaces: Workspaces,
        notifier: Arc<dyn Notifier>,
        device: [u8; 32],
        person: [u8; 32],
    ) -> Self {
        Self {
            workspaces,
            notifier,
            person: hex::encode(person),
            device: hex::encode(device),
            armed: BTreeMap::new(),
            rung: BTreeMap::new(),
            packages: BTreeMap::new(),
            unsupported: false,
        }
    }

    /// Arm what `app`'s triggers land on this device in `workspace` after `now_ms`, read through
    /// `package` (`None` for the one this build carries) and shown as from `app_name`; move what
    /// moved and take back what lands no more. Answers how many it handed the platform.
    ///
    /// # Errors
    /// The app's record cannot be read.
    pub async fn arm(
        &mut self,
        workspace: &str,
        (app, app_name): (&str, &str),
        package: Option<&Package>,
        now_ms: i64,
    ) -> Result<usize, String> {
        let wanted = self
            .workspaces
            .app_wakeups(
                workspace,
                app,
                package,
                (&self.person, &self.device),
                now_ms,
            )
            .await?;
        self.packages.insert(
            (workspace.to_owned(), app.to_owned()),
            package.cloned().unwrap_or(Package::BuiltIn),
        );
        let mut kept = BTreeSet::new();
        let mut handed = 0;
        for wakeup in &wanted {
            for (ordinal, told) in wakeup.told.iter().enumerate() {
                let tag = tag_of(workspace, app, &wakeup.key, ordinal);
                kept.insert(tag.clone());
                let notification = notification_of(&tag, app_name, told, wakeup.at_ms);
                if self.armed.get(&tag).is_some_and(|armed| {
                    armed.at_ms == wakeup.at_ms && armed.notification == notification
                }) {
                    continue;
                }
                let at = SystemTime::UNIX_EPOCH
                    .checked_add(Duration::from_millis(
                        u64::try_from(wakeup.at_ms).unwrap_or(0),
                    ))
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                match self.notifier.schedule(&notification, at) {
                    Ok(()) => {
                        self.armed.insert(
                            tag,
                            Armed {
                                workspace: workspace.to_owned(),
                                app: app.to_owned(),
                                at_ms: wakeup.at_ms,
                                told: told.clone(),
                                notification,
                            },
                        );
                        handed += 1;
                    }
                    Err(error) => self.refused(&error),
                }
            }
        }
        let gone: Vec<String> = self
            .armed
            .iter()
            .filter(|(tag, armed)| {
                armed.workspace == workspace && armed.app == app && !kept.contains(*tag)
            })
            .map(|(tag, _)| tag.clone())
            .collect();
        for tag in gone {
            if let Err(error) = self.notifier.cancel_scheduled(&tag) {
                tracing::warn!(%tag, %error, "a wakeup that lands no more was not taken back");
            }
            self.armed.remove(&tag);
        }
        Ok(handed)
    }

    /// What rang since the last look: the wakeups the platform no longer holds, each notice by
    /// its record's id, which this device does not show again when it arrives. A platform that
    /// lists nothing it holds reports none.
    pub fn look(&mut self) -> Vec<Rang> {
        let held: BTreeSet<String> = match self.notifier.scheduled() {
            Ok(held) => held.into_iter().map(|scheduled| scheduled.tag).collect(),
            Err(_) => return Vec::new(),
        };
        let rang: Vec<String> = self
            .armed
            .keys()
            .filter(|tag| !held.contains(*tag))
            .cloned()
            .collect();
        let mut out = Vec::new();
        for tag in rang {
            if let Some(armed) = self.armed.remove(&tag) {
                out.push((
                    armed.workspace.clone(),
                    armed.app.clone(),
                    armed.told.doc.clone(),
                ));
                self.rung.insert(tag, armed);
            }
        }
        out
    }

    /// Carry out the button `uri` names (`tilo-wakeup:<tag>?<action>@<ms>`) of a wakeup this
    /// device armed: the action the notice offers, on what it opens, as the person, with no
    /// network. A wakeup this process did not arm (the app was ended since, and the platform rang
    /// it) is found again as what landed here just before its moment; a uri naming anything else
    /// runs nothing.
    ///
    /// # Errors
    /// The uri names no wakeup of this device's or an action its notice does not offer, or the
    /// action was refused.
    pub async fn chosen(&mut self, uri: &str) -> Result<(), String> {
        let (tag, action, at_ms) = uri
            .strip_prefix(ACTIVATION)
            .and_then(|rest| rest.strip_prefix(':'))
            .and_then(|rest| rest.split_once('?'))
            .and_then(|(tag, rest)| {
                let (action, at) = rest.rsplit_once('@')?;
                Some((tag, action, at.parse::<i64>().ok()?))
            })
            .ok_or_else(|| format!("`{uri}` is no wakeup's button"))?;
        let known = self.rung.get(tag).or_else(|| self.armed.get(tag)).cloned();
        let armed = match known {
            Some(armed) => armed,
            None => self
                .found(tag, at_ms)
                .await
                .ok_or_else(|| format!("no wakeup of this device's is `{tag}`"))?,
        };
        if !armed.told.actions.iter().any(|(key, _)| key == action) {
            return Err(format!("the wakeup `{tag}` offers no `{action}`"));
        }
        let package = self
            .packages
            .get(&(armed.workspace.clone(), armed.app.clone()))
            .cloned();
        self.workspaces
            .wakeup_action(
                &armed.workspace,
                (&armed.app, package.as_ref()),
                &self.person,
                (&armed.told.opens.0, &armed.told.opens.1),
                action,
            )
            .await
    }

    /// The wakeup `tag` at `at_ms`, read again from the records whose moments this device arms:
    /// what landed here just before that moment.
    async fn found(&self, tag: &str, at_ms: i64) -> Option<Armed> {
        for ((workspace, app), package) in &self.packages {
            let Ok(wanted) = self
                .workspaces
                .app_wakeups(
                    workspace,
                    app,
                    Some(package),
                    (&self.person, &self.device),
                    at_ms.saturating_sub(1),
                )
                .await
            else {
                continue;
            };
            for wakeup in wanted.into_iter().filter(|wakeup| wakeup.at_ms == at_ms) {
                for (ordinal, told) in wakeup.told.into_iter().enumerate() {
                    if tag_of(workspace, app, &wakeup.key, ordinal) == tag {
                        return Some(Armed {
                            workspace: workspace.clone(),
                            app: app.clone(),
                            at_ms,
                            notification: notification_of(tag, app, &told, at_ms),
                            told,
                        });
                    }
                }
            }
        }
        None
    }

    /// The tags this device has handed the platform, and the instant of each, in ms.
    #[must_use]
    pub fn armed(&self) -> Vec<(String, i64)> {
        self.armed
            .iter()
            .map(|(tag, armed)| (tag.clone(), armed.at_ms))
            .collect()
    }

    fn refused(&mut self, error: &NotifyError) {
        if matches!(
            error,
            NotifyError::Unsupported(_) | NotifyError::Unavailable
        ) {
            if !std::mem::replace(&mut self.unsupported, true) {
                tracing::info!(
                    %error,
                    "this device keeps no wakeups of its own: an app's moment reaches it as the notice the workspace sends"
                );
            }
        } else {
            tracing::warn!(%error, "a wakeup was not armed");
        }
    }
}

/// The platform's tag of the `ordinal`th notice the moment `key` of `app` in `workspace` tells:
/// the same while the moment moves, and short enough for every platform's tags.
fn tag_of(workspace: &str, app: &str, key: &str, ordinal: usize) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [workspace, app, key, &ordinal.to_string()] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    let hex = hasher.finalize().to_hex();
    format!("w{}", hex.get(..14).unwrap_or_default())
}

/// The notification `told` shows at its moment `at_ms`, from `app_name`, with a button for each
/// action it offers.
fn notification_of(tag: &str, app_name: &str, told: &Raised, at_ms: i64) -> Notification {
    told.actions.iter().fold(
        Notification::new(tag, &told.title, &told.body).from_app(app_name),
        |notification, (key, label)| {
            notification.with_button(label, format!("{ACTIVATION}:{tag}?{key}@{at_ms}"))
        },
    )
}
