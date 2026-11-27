//! What the person answered about the devices of their machines that outlasts a session:
//! "Recorder may use the camera on this laptop, always". One entry per device, app and
//! capability, naming the device it is about: it answers only there, and any device of theirs
//! may take it back. [`tilo_peer_state::DeviceGrants`] decides by it.

use tilo_peer_state::DeviceAnswer;
use tilo_permissions::caps::DeviceCap;
use tilo_resource::{Collection, Id, Model, Refusal, resource};

/// The resource of the person's kept device answers in their own workspace.
pub const DEVICE_ANSWERS: (&str, &str) = ("so.tilo.devices", "answers");

/// The person's kept answers, by device, app and capability.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DeviceAnswers {
    pub answers: Collection<KeptAnswer>,
}

/// One kept answer.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct KeptAnswer {
    /// The device it answers on, by key (lowercase hex).
    pub device: String,
    /// The app it answers for.
    pub app: String,
    /// What the app may use, by its name ([`DeviceCap::name`]).
    pub cap: String,
    /// Allowed; else refused.
    pub allowed: bool,
    /// When it stops holding, Unix ms; `None` while it holds until taken back.
    pub until_ms: Option<i64>,
}

fn key(device: &str, app: &str, cap: &str) -> Id {
    Id::from_raw(format!("{device}:{app}:{cap}"))
}

#[resource(kind = "tilo.device-answers", read_content = tilo_resource::read::fields)]
impl DeviceAnswers {
    /// Keep what the person answered `app` of `cap` on `device`: allowed or refused, until
    /// `until_ms` or until taken back.
    #[action]
    pub fn answer(
        &mut self,
        device: String,
        app: String,
        cap: String,
        allowed: bool,
        until_ms: Option<i64>,
    ) -> Result<(), Refusal> {
        let mut bytes = [0; 32];
        hex::decode_to_slice(&device, &mut bytes)
            .map_err(|_| Refusal::new("a device is named by 32 bytes in hex"))?;
        if DeviceCap::from_name(&cap).is_none() || app.is_empty() {
            return Err(Refusal::new("an answer names an app and what it may use"));
        }
        let id = key(&device, &app, &cap);
        let kept = KeptAnswer {
            device,
            app,
            cap,
            allowed,
            until_ms,
        };
        match self.answers.get_mut(&id) {
            Some(entry) => *entry = kept,
            None => {
                self.answers.push(id, kept);
            }
        }
        Ok(())
    }

    /// Take back what `app` was answered of `cap` on `device`.
    #[action]
    pub fn take_back(&mut self, device: String, app: String, cap: String) -> Result<(), Refusal> {
        self.answers
            .remove(&key(&device, &app, &cap))
            .map(drop)
            .ok_or_else(|| Refusal::new("nothing was answered there"))
    }
}

impl DeviceAnswers {
    /// The action that keeps `answer`.
    #[must_use]
    pub fn answering(answer: &DeviceAnswer) -> tilo_resource::Invoke<Self> {
        let until_ms = match answer.lifetime {
            tilo_action_types::Lifetime::Until(ms) => Some(i64::try_from(ms).unwrap_or(i64::MAX)),
            _ => None,
        };
        Self::ANSWER.with((
            answer.device.clone(),
            answer.app.clone(),
            answer.cap.name().to_owned(),
            answer.answer == tilo_action_policy::Answer::Allow,
            until_ms,
        ))
    }

    /// The action that takes back what `app` was answered of `cap` on `device`.
    #[must_use]
    pub fn taking_back(device: &str, app: &str, cap: DeviceCap) -> tilo_resource::Invoke<Self> {
        Self::TAKE_BACK.with((device.to_owned(), app.to_owned(), cap.name().to_owned()))
    }

    /// Every answer kept, as [`tilo_peer_state::DeviceGrants`] reads them.
    #[must_use]
    pub fn kept(&self) -> Vec<DeviceAnswer> {
        self.answers
            .iter()
            .filter_map(|(_, kept)| {
                Some(DeviceAnswer {
                    device: kept.device.clone(),
                    app: kept.app.clone(),
                    cap: DeviceCap::from_name(&kept.cap)?,
                    answer: if kept.allowed {
                        tilo_action_policy::Answer::Allow
                    } else {
                        tilo_action_policy::Answer::Deny
                    },
                    lifetime: kept
                        .until_ms
                        .map_or(tilo_action_types::Lifetime::UntilRevoked, |ms| {
                            tilo_action_types::Lifetime::Until(u64::try_from(ms).unwrap_or(0))
                        }),
                })
            })
            .collect()
    }
}
