//! Using another of the person's devices' camera, microphone or scanner here, and sending this
//! device's game controller to another of theirs.
//!
//! Each use is asked and decided in the record of the person's devices
//! ([`tilo_workspace_service::workspaces::person_workspace::remote_use`]): this session asks the
//! uses the person starts here, for the app they have in front. The device whose hardware it is
//! asks its person, naming the app, and a yes grants five minutes, renewed while it sends. A
//! controller goes the other way: the device it is sent to asks its person with the app they have
//! in front there, and their yes binds the controller to that very app session for one five-minute
//! lease. What was captured, and the controller's events, cross the live link between the two
//! devices ([`crate::live::OwnDevices`], [`crate::live::Lane::RemoteUse`]); a virtual camera,
//! microphone, scanner inbox or game pad takes their place where they are used
//! ([`edge`]). A use ends when either person stops it, its app leaves the front where it is
//! used, its grant runs out, a device leaves, or the person turns off realtime sessions or game
//! controllers (Settings, Continuity) on either device.

mod edge;
mod engine;
mod wire;

#[cfg(test)]
mod engine_tests;

use std::cell::RefCell;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tilo_device_surfaces::devices::RemoteUseView;
use tilo_service::{Asked, Question as Ask};
use tilo_ui_base::i18n::ArgValue;
use tilo_workspace_service::workspaces::person_workspace::devices::{
    Devices, RemoteKind, RemoteStage, RemoteUse,
};
use tilo_workspace_service::workspaces::person_workspace::remote_use::{self, Profile};

use crate::Shared;
use crate::followed_resource::FollowedResource;
use crate::i18n::{t, t_args};
use crate::live::{Lane, OwnDevices};
use crate::platform::fabric::{Continuity, SharedLink};
use crate::platform::notice::Notice;

pub(crate) use engine::Front;
use engine::{Engine, Question, World};

/// The question's kind, as the deciding device's person is asked it ([`crate::program_asks`]).
pub(crate) const KIND: &str = "devices.remote-use";

/// What the question shows.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct UseQuestion {
    /// The device that asks, as the person calls it.
    pub(crate) device: String,
    /// What it asks for: `camera`, `microphone`, `scanner` or `controller`.
    pub(crate) kind: String,
    /// The app it is for: the asking device's for a camera, microphone or scanner, the one in
    /// front here for a controller.
    pub(crate) app: String,
}

/// What the person asked for on this device.
enum Wanted {
    /// Use the device `device`'s camera, microphone or scanner here, or send it this device's
    /// controller.
    Use { device: String, kind: RemoteKind },
    /// Stop the use `id`.
    Stop(String),
}

/// This session's part in the remote uses between the person's devices.
pub(crate) struct RemoteUses {
    own: Option<OwnDevices>,
    continuity: Continuity,
    /// When this session started, ms since the epoch: a use asked before it was told then.
    since_ms: i64,
    engine: RefCell<Option<Engine>>,
    wanted: RefCell<Vec<Wanted>>,
    /// The questions this device's person was asked, until they answer.
    asking: RefCell<Vec<(Question, Asked)>>,
    /// The uses it told the person about, by id and stage.
    told: RefCell<BTreeSet<(String, RemoteStage)>>,
}

impl RemoteUses {
    /// The remote uses of this session, carried over `own`.
    pub(crate) fn new(own: Option<OwnDevices>, continuity: Continuity) -> Self {
        Self {
            own,
            continuity,
            since_ms: now_ms(),
            engine: RefCell::default(),
            wanted: RefCell::default(),
            asking: RefCell::default(),
            told: RefCell::default(),
        }
    }

    /// Use `device`'s camera, microphone or scanner (`kind`) here for the app in front, or send it
    /// this device's game controller.
    pub(crate) fn start(&self, device: &str, kind: RemoteKind) {
        self.wanted.borrow_mut().push(Wanted::Use {
            device: device.to_owned(),
            kind,
        });
    }

    /// Stop the use `id`.
    pub(crate) fn stop(&self, id: &str) {
        self.wanted.borrow_mut().push(Wanted::Stop(id.to_owned()));
    }

    /// Do what the person, the record and the other devices left for this pass.
    pub(crate) fn pump(&self, shared: &Shared, fabric: &SharedLink) {
        let (Some(devices), Some(own)) = (shared.owner_devices.as_ref(), self.own.as_ref()) else {
            self.wanted.borrow_mut().clear();
            return;
        };
        let (record, local) = devices.record();
        let now = now_ms();
        let front = shared.remote_front();
        let continuity = self.continuity.clone();
        let allowed = move |kind: RemoteKind| continuity.remote_use_permitted(kind);
        self.carry_out_wanted(shared, &record, &local, front.as_ref(), &allowed, now);

        let held = record.get();
        let online: BTreeSet<String> = fabric
            .online_device_ids()
            .iter()
            .map(|device| device.as_str().to_owned())
            .collect();
        let is_online = |device: &str| online.contains(device);
        let world = World {
            record: &held,
            now_ms: now,
            online: &is_online,
            allowed: &allowed,
            front,
        };
        let mut engine = self.engine.borrow_mut();
        let engine = engine.get_or_insert_with(|| {
            Engine::new(
                local.clone(),
                Box::new(edge::Machine),
                Box::new(os_actions::LinuxVirtualGamepadFactory::new(
                    os_actions::UinputGamepadDeviceProvider,
                )),
            )
        });
        self.answered(&record, engine, &world);

        let from: BTreeSet<[u8; 32]> = held
            .uses
            .iter()
            .filter(|(_, kept)| kept.under_way(now) && kept.to == local)
            .filter_map(|(_, kept)| device_key(&kept.from))
            .collect();
        let heard = own
            .heard(Lane::RemoteUse, &from)
            .into_iter()
            .map(|(sender, body)| (hex::encode(sender), body))
            .collect();
        let out = engine.pass(&world, heard);
        for write in &out.writes {
            invoke(&record, write);
        }
        for (to, body) in out.sent {
            if let Some(to) = device_key(&to) {
                own.send(to, Lane::RemoteUse, body);
            }
        }
        for question in out.questions {
            self.ask_person(shared, &held, question);
        }
        for (kept, why) in out.failed {
            let label = crate::wake::label(&held, &key(other(&kept, &local)));
            tell(
                shared,
                1,
                t("session-remote-not-shared"),
                t_args(
                    "session-remote-failed",
                    &[("device", label.into()), ("why", why.into())],
                ),
            );
        }
        // A question whose use ended meanwhile is taken back.
        self.asking.borrow_mut().retain(|(question, _)| {
            held.uses
                .get(&key(&question.id))
                .is_some_and(|kept| kept.stage == RemoteStage::Asked)
        });
        self.tell_stages(shared, &held, &local);
    }

    /// Ask the uses the person started here, and end the ones they stopped.
    fn carry_out_wanted(
        &self,
        shared: &Shared,
        record: &FollowedResource<Devices>,
        local: &str,
        front: Option<&Front>,
        allowed: &dyn Fn(RemoteKind) -> bool,
        now: i64,
    ) {
        for wanted in std::mem::take(&mut *self.wanted.borrow_mut()) {
            let held = record.get();
            let write = match wanted {
                Wanted::Use { device, kind } => {
                    match ask(&held, local, &device, kind, front, allowed, now) {
                        Ok(write) => {
                            let label = crate::wake::label(&held, &key(&device));
                            tell(
                                shared,
                                0,
                                t_args(
                                    "session-remote-asked",
                                    &[("device", label.into()), ("what", what(kind).into())],
                                ),
                                t("session-remote-asked-body"),
                            );
                            write
                        }
                        Err(why) => {
                            tell(shared, 1, t("session-remote-not-shared"), why);
                            continue;
                        }
                    }
                }
                Wanted::Stop(id) => remote_use::ending(&id, now),
            };
            invoke(record, &write);
        }
    }

    /// Ask this device's person `question`.
    fn ask_person(&self, shared: &Shared, record: &Devices, question: Question) {
        let label = crate::wake::label(record, &key(question.kept.asker()));
        let app = match (&question.kept.kind, &question.front) {
            (RemoteKind::Controller, Some(front)) => front.name.clone(),
            _ => question.kept.label.clone(),
        };
        let asked = shared.asker.ask(Ask {
            handle: format!("{KIND}:{}", question.id),
            kind: KIND.to_owned(),
            app_id: None,
            label: label.clone(),
            details: serde_json::to_value(UseQuestion {
                device: label,
                kind: kind_word(question.kept.kind).to_owned(),
                app,
            })
            .unwrap_or_default(),
        });
        self.asking.borrow_mut().push((question, asked));
    }

    /// Take in the person's answers.
    fn answered(&self, record: &FollowedResource<Devices>, engine: &Engine, world: &World<'_>) {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut answered = Vec::new();
        self.asking.borrow_mut().retain_mut(|(question, asked)| {
            match std::pin::Pin::new(asked).poll(&mut context) {
                std::task::Poll::Ready(answer) => {
                    answered.push((question.clone(), answer.is_some()));
                    false
                }
                std::task::Poll::Pending => true,
            }
        });
        for (question, yes) in answered {
            invoke(record, &engine.answered(world, &question, yes));
        }
    }

    /// Tell the person how each use this device takes part in went, once the record says so.
    fn tell_stages(&self, shared: &Shared, record: &Devices, local: &str) {
        let mut told = self.told.borrow_mut();
        for (id, kept) in record.uses.iter() {
            if (kept.from != local && kept.to != local) || kept.asked_ms < self.since_ms {
                continue;
            }
            let asking = kept.asker() == local;
            let worth = match kept.stage {
                RemoteStage::Asked => false,
                RemoteStage::Granted | RemoteStage::Ended => true,
                RemoteStage::Declined | RemoteStage::GivenUp => asking,
            };
            if !worth || !told.insert((id.as_str().to_owned(), kept.stage)) {
                continue;
            }
            let label = crate::wake::label(record, &key(other(kept, local)));
            let args = [
                ("device", ArgValue::from(label)),
                ("what", what(kept.kind).into()),
                ("app", app_of(kept).into()),
            ];
            let (urgency, title, body) = match (kept.stage, asking) {
                (RemoteStage::Granted, true) => (
                    0,
                    t_args("session-remote-using", &args),
                    t_args("session-remote-using-body", &args),
                ),
                (RemoteStage::Granted, false) => (
                    0,
                    t_args("session-remote-lent", &args),
                    t("session-remote-lent-body"),
                ),
                (RemoteStage::Declined, _) => (
                    1,
                    t("session-remote-not-shared"),
                    t_args("session-remote-declined", &args),
                ),
                (RemoteStage::GivenUp, _) => (
                    1,
                    t("session-remote-not-shared"),
                    t_args("session-remote-given-up", &args),
                ),
                _ => (
                    0,
                    t("session-remote-ended"),
                    t_args("session-remote-ended-body", &args),
                ),
            };
            tell(shared, urgency, title, body);
        }
    }
}

/// The ask for `device`'s `kind` the person started here at `now_ms`, or why it cannot be asked,
/// in their words.
fn ask(
    record: &Devices,
    local: &str,
    device: &str,
    kind: RemoteKind,
    front: Option<&Front>,
    allowed: &dyn Fn(RemoteKind) -> bool,
    now_ms: i64,
) -> Result<tilo_resource::Invoke<Devices>, String> {
    if !allowed(kind) {
        return Err(t(if kind == RemoteKind::Controller {
            "session-remote-controllers-off"
        } else {
            "session-remote-realtime-off"
        }));
    }
    let named = if kind == RemoteKind::Controller {
        let controller = edge::controllers()
            .into_iter()
            .next()
            .ok_or_else(|| t("session-remote-no-controller"))?;
        (String::new(), String::new(), controller)
    } else {
        let front = front.ok_or_else(|| t("session-remote-no-front"))?;
        (
            front.app.clone(),
            front.workspace.clone(),
            front.name.clone(),
        )
    };
    let id = tilo_network_actor::fabric::nonzero::<16>();
    remote_use::asking(
        record,
        id,
        local,
        device,
        &Profile::usual(kind),
        (&named.0, &named.1, &named.2),
        now_ms,
    )
    .map_err(|refusal| refusal.to_string())
}

/// The uses this device (`local`) takes part in that are under way at `now_ms`, as the Devices
/// section shows them.
pub(crate) fn uses(record: &Devices, local: &str) -> Vec<RemoteUseView> {
    let now = now_ms();
    record
        .uses
        .iter()
        .filter(|(_, kept)| kept.under_way(now) && (kept.from == local || kept.to == local))
        .map(|(id, kept)| {
            let device = other(kept, local);
            RemoteUseView {
                id: id.as_str().to_owned(),
                device: device.to_owned(),
                label: crate::wake::label(record, &key(device)),
                kind: kind_word(kept.kind).to_owned(),
                app: app_of(kept),
                asking: kept.asker() == local,
                granted: kept.stage == RemoteStage::Granted,
                until_ms: kept.until_ms,
            }
        })
        .collect()
}

/// The app `kept` is for, as the person reads it: a controller's once its yes bound one, else
/// what it was asked for.
fn app_of(kept: &RemoteUse) -> String {
    if kept.kind == RemoteKind::Controller && !kept.app.is_empty() {
        crate::app_names::app_name(&kept.app)
    } else {
        kept.label.clone()
    }
}

/// The device on the other end of `kept` from `local`.
fn other<'a>(kept: &'a RemoteUse, local: &str) -> &'a str {
    if kept.from == local {
        &kept.to
    } else {
        &kept.from
    }
}

/// How the reading and the question name `kind`.
pub(crate) const fn kind_word(kind: RemoteKind) -> &'static str {
    match kind {
        RemoteKind::Camera => "camera",
        RemoteKind::Microphone => "microphone",
        RemoteKind::Scanner => "scanner",
        RemoteKind::Controller => "controller",
    }
}

/// The kind a reading or a press names.
pub(crate) fn kind_named(word: &str) -> Option<RemoteKind> {
    [
        RemoteKind::Camera,
        RemoteKind::Microphone,
        RemoteKind::Scanner,
        RemoteKind::Controller,
    ]
    .into_iter()
    .find(|kind| kind_word(*kind) == word)
}

/// What `kind` is, in the person's words.
pub(crate) fn what(kind: RemoteKind) -> String {
    t(match kind {
        RemoteKind::Camera => "session-remote-camera",
        RemoteKind::Microphone => "session-remote-microphone",
        RemoteKind::Scanner => "session-remote-scanner",
        RemoteKind::Controller => "session-remote-controller",
    })
}

fn invoke(record: &FollowedResource<Devices>, write: &tilo_resource::Invoke<Devices>) {
    record.invoke_unwaited(write, |written| {
        if let Err(error) = written {
            tracing::warn!(%error, "a remote use of a device was not written");
        }
    });
}

/// The device key `device` (lowercase hex) names.
fn device_key(device: &str) -> Option<[u8; 32]> {
    hex::decode(device).ok()?.try_into().ok()
}

/// The record's key of the device or use `id` (lowercase hex).
fn key(id: &str) -> tilo_resource::Id {
    tilo_resource::Id::from_raw(id.to_owned())
}

fn tell(shared: &Shared, urgency: u8, title: String, body: String) {
    shared
        .notifications
        .borrow()
        .tell(&Notice::new(t("session-remote-app"), urgency, title, body));
}

fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX)
}
