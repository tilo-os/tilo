//! Moving the person's call to another of their devices.
//!
//! The call card offers Continue on each other device of theirs that is online
//! ([`crate::call_card`]). Pressing one hands the call over through the record of the person's
//! devices: an offer made to that device alone ([`ContinuityOffer`]) that carries the call as it
//! stands here ([`HandedCall`]). That device asks its person whether to take the call, with the
//! system's own question. On their yes it hands the call to its call service
//! ([`NativeMediaService::hand_in`]) and shows the call's place in its app, which joins it the way
//! the app declares joining a call; the call comes in as it stood here, heard or muted, deafened,
//! on camera, sharing the screen. Once the call is live there, that device says in the record that
//! it took it, and only then does this device leave the call, so the person is never out of it in
//! between. A no, a device that does not answer in time, or a call that does not come up there
//! leave the call where it is, and the person is told why.
//!
//! Every app that holds a call through the system's call service takes part: the service tells
//! the app's record that a call was handed to it, and the app joins it as it joins any call.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::native_media::{Command, State};
use tilo_native_media::{NativeMediaService, OwnCall};
use tilo_service::{AskSender, Asked, Question};
use tilo_workspace_service::workspaces::person_workspace::devices::{
    ContinuityOffer, Devices, HandedCall,
};

use crate::call_card::Open;
use crate::i18n::{t, t_args};

/// The question's kind, as the person on the device a call is handed to is asked it
/// ([`crate::program_asks`]).
pub(crate) const KIND: &str = "calls.take-handed";

/// How long a call handed to another device waits there, ms: for the person to say yes, and for
/// the call to come up. Past it the call stays where it is.
const HAND_MS: i64 = 60 * 1_000;

/// What the question shows: the device the call comes from, and what the call is.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TakeQuestion {
    pub(crate) device: String,
    pub(crate) call: String,
}

/// What the session does for a hand-off, which the hand-off cannot do itself.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Write this offer in the record of the person's devices.
    Offer(ContinuityOffer),
    /// Say in the record that this device took the offer `from` made at `at_ms`, or did not, and
    /// why not.
    Answer {
        from: String,
        at_ms: i64,
        taken: bool,
        reason: Option<String>,
    },
    /// Show the call's place in the app the call is in.
    Open(Open),
    /// Tell the person how a hand-off went.
    Tell { title: String, body: String },
}

/// What a hand-off reads where it runs.
pub(crate) struct Here<'a> {
    /// The record of the person's devices, as it stands.
    pub(crate) record: &'a Devices,
    /// This device, by key (lowercase hex).
    pub(crate) local: &'a str,
    /// This device's calls.
    pub(crate) media: &'a NativeMediaService,
    /// Where the person here is asked.
    pub(crate) asker: &'a AskSender,
    /// Now, ms since the epoch.
    pub(crate) now_ms: i64,
}

impl Here<'_> {
    /// What the person calls their device `key`.
    fn called(&self, key: &str) -> String {
        crate::wake::label(self.record, &tilo_resource::Id::from_raw(key.to_owned()))
    }
}

/// The call this device hands to another of the person's devices.
struct Sending {
    /// The device, by key.
    to: String,
    /// What the person calls it.
    name: String,
    /// When the offer was made, which names it in the record.
    shown_ms: i64,
    until_ms: i64,
    /// The call it hands, by its record and channel.
    call: (String, String),
}

/// A call handed here that the person said yes to, which waits to be live here.
struct Taking {
    offer: ContinuityOffer,
}

/// This device's side of moving calls between the person's devices.
#[derive(Default)]
pub(crate) struct CallMoves {
    sending: Option<Sending>,
    /// The calls handed here the person is asked about, until they answer.
    asking: Vec<(ContinuityOffer, Asked)>,
    taking: Option<Taking>,
    /// The offers this device took up, by the device and moment: each is asked once.
    seen: BTreeSet<(String, i64)>,
}

impl CallMoves {
    /// The device the call is moving to, by what the person calls it, while it is.
    pub(crate) fn moving(&self) -> Option<&str> {
        self.sending.as_ref().map(|sending| sending.name.as_str())
    }

    /// Hand this device's `call` to the device `to` (by key): the offer goes to it through the
    /// record, and the call stays here until that device says it took it.
    pub(crate) fn send(&mut self, here: &Here<'_>, call: &OwnCall, to: &str) -> Vec<Effect> {
        if self.sending.is_some() || to == here.local {
            return Vec::new();
        }
        let shown_ms = here.now_ms;
        let until_ms = shown_ms.saturating_add(HAND_MS);
        let offer = ContinuityOffer {
            from: here.local.to_owned(),
            app_id: call.app.clone(),
            app_name: crate::app_names::app_name(&call.app),
            resource: tilolang_tilo::naming::resource_id(&call.app, &call.channel),
            shown_ms,
            until_ms,
            to: to.to_owned(),
            workspace: call.workspace.clone(),
            call: HandedCall {
                record: call.record.clone(),
                channel: call.channel.clone(),
                muted: call.muted,
                deafened: call.deafened,
                camera: call.camera,
                screen: call.screen,
            },
            ..ContinuityOffer::default()
        };
        self.sending = Some(Sending {
            to: to.to_owned(),
            name: here.called(to),
            shown_ms,
            until_ms,
            call: (call.record.clone(), call.channel.clone()),
        });
        vec![Effect::Offer(offer)]
    }

    /// Take every hand-off a step further, as the record and the calls stand `here`.
    pub(crate) fn follow(&mut self, here: &Here<'_>) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.follow_sending(here, &mut effects);
        self.ask_new(here);
        self.answered(here, &mut effects);
        self.follow_taking(here, &mut effects);
        effects
    }

    /// The call this device hands over: left once the other device took it, kept here with the
    /// person told why when it did not.
    fn follow_sending(&mut self, here: &Here<'_>, effects: &mut Vec<Effect>) {
        let Some(sending) = &self.sending else {
            return;
        };
        let own = here.media.own_call();
        // The call ended here meanwhile: there is nothing left to hand.
        if own
            .as_ref()
            .is_none_or(|call| (&call.record, &call.channel) != (&sending.call.0, &sending.call.1))
        {
            self.sending = None;
            return;
        }
        let answer = here
            .record
            .continuity
            .get(&tilo_resource::Id::from_raw(here.local.to_owned()))
            .filter(|kept| kept.shown_ms == sending.shown_ms)
            .and_then(|kept| kept.answered_by(&sending.to).cloned());
        let device = [("device", sending.name.clone().into())];
        match answer {
            Some(answer) if answer.taken => {
                if let Err(error) = here.media.command_own(Command::Leave) {
                    tracing::warn!(%error, "the call handed to another device was not left here");
                }
            }
            Some(answer) => effects.push(Effect::Tell {
                title: t_args("call-move-stayed-title", &device),
                body: if answer.reason.is_empty() {
                    t_args("call-move-reason-declined", &device)
                } else {
                    answer.reason
                },
            }),
            None if here.now_ms >= sending.until_ms => effects.push(Effect::Tell {
                title: t_args("call-move-stayed-title", &device),
                body: t_args("call-move-reason-no-answer", &device),
            }),
            None => return,
        }
        self.sending = None;
    }

    /// Ask the person here about each call another of their devices handed here that they were
    /// not asked about yet.
    fn ask_new(&mut self, here: &Here<'_>) {
        let offers: Vec<ContinuityOffer> = here
            .record
            .continuity
            .iter()
            .map(|(_, kept)| kept)
            .filter(|kept| {
                kept.call.is_call()
                    && kept.to == here.local
                    && kept.shown_at(here.now_ms)
                    && kept.answer_of(here.local).is_none()
            })
            .filter(|kept| self.seen.insert((kept.from.clone(), kept.shown_ms)))
            .cloned()
            .collect();
        for offer in offers {
            let device = here.called(&offer.from);
            let asked = here.asker.ask(Question {
                handle: format!("{KIND}:{}:{}", offer.from, offer.shown_ms),
                kind: KIND.to_owned(),
                app_id: Some(offer.app_id.clone()),
                label: device.clone(),
                details: serde_json::to_value(TakeQuestion {
                    device,
                    call: offer.app_name.clone(),
                })
                .unwrap_or_default(),
            });
            self.asking.push((offer, asked));
        }
    }

    /// Take in the person's answers: a yes hands the call to this device's call service and
    /// shows its place in its app; a no says so to the device it came from.
    fn answered(&mut self, here: &Here<'_>, effects: &mut Vec<Effect>) {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut answered = Vec::new();
        self.asking.retain_mut(|(offer, asked)| {
            match std::pin::Pin::new(asked).poll(&mut context) {
                std::task::Poll::Ready(answer) => {
                    answered.push((offer.clone(), answer.is_some()));
                    false
                }
                std::task::Poll::Pending => true,
            }
        });
        for (offer, yes) in answered {
            let device = [("device", here.called(here.local).into())];
            if !yes || !offer.shown_at(here.now_ms) || self.taking.is_some() {
                let reason = if yes {
                    t_args("call-move-reason-late", &device)
                } else {
                    t_args("call-move-reason-declined", &device)
                };
                effects.push(Effect::Answer {
                    from: offer.from.clone(),
                    at_ms: offer.shown_ms,
                    taken: false,
                    reason: Some(reason),
                });
                continue;
            }
            here.media.hand_in(OwnCall {
                workspace: offer.workspace.clone(),
                app: offer.app_id.clone(),
                record: offer.call.record.clone(),
                channel: offer.call.channel.clone(),
                camera: offer.call.camera,
                screen: offer.call.screen,
                muted: offer.call.muted,
                deafened: offer.call.deafened,
                state: State::Joining,
            });
            effects.push(Effect::Open(Open {
                workspace: offer.workspace.clone(),
                app: offer.app_id.clone(),
                record: offer.call.record.clone(),
                resource: offer.resource.clone(),
            }));
            self.taking = Some(Taking { offer });
        }
    }

    /// The call the person took here: said taken once it is live here, which is when the device
    /// it came from leaves it; given back when it does not come up in time.
    fn follow_taking(&mut self, here: &Here<'_>, effects: &mut Vec<Effect>) {
        let Some(taking) = &self.taking else {
            return;
        };
        let offer = &taking.offer;
        let own = here.media.own_call().filter(|call| {
            (&call.workspace, &call.app, &call.record, &call.channel)
                == (
                    &offer.workspace,
                    &offer.app_id,
                    &offer.call.record,
                    &offer.call.channel,
                )
        });
        if own.as_ref().is_some_and(|call| call.state == State::Live) {
            effects.push(Effect::Answer {
                from: offer.from.clone(),
                at_ms: offer.shown_ms,
                taken: true,
                reason: None,
            });
        } else if !offer.shown_at(here.now_ms) {
            here.media.drop_handed();
            // A call still on its way in here stops: the person stays in it on the device it
            // came from, never in it twice.
            if own.is_some()
                && let Err(error) = here.media.command_own(Command::Leave)
            {
                tracing::warn!(%error, "a handed call that did not come up was not left");
            }
            let device = [("device", here.called(here.local).into())];
            effects.push(Effect::Answer {
                from: offer.from.clone(),
                at_ms: offer.shown_ms,
                taken: false,
                reason: Some(t_args("call-move-reason-not-live", &device)),
            });
            effects.push(Effect::Tell {
                title: t("call-move-not-taken-title"),
                body: t_args(
                    "call-move-not-taken-body",
                    &[("device", here.called(&offer.from).into())],
                ),
            });
        } else {
            return;
        }
        self.taking = None;
    }
}

impl crate::Session {
    /// Move this device's call to the person's device `to`, as the call card's Continue asked, and
    /// take every hand-off between their devices a step further: what the record says and what
    /// the person here answered.
    pub(crate) fn follow_call_moves(
        &self,
        media: &NativeMediaService,
        continue_on: Option<String>,
    ) {
        let Some(devices) = self.shared.owner_devices.as_ref() else {
            return;
        };
        let record = devices.devices();
        let (_, local) = devices.who();
        let here = Here {
            record: &record,
            local: &local,
            media,
            asker: &self.shared.asker,
            now_ms: i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX),
        };
        let mut moves = self.shared.call_move.borrow_mut();
        let mut effects = Vec::new();
        if let (Some(to), Some(call)) = (continue_on, media.own_call()) {
            effects.extend(moves.send(&here, &call, &to));
        }
        effects.extend(moves.follow(&here));
        drop(moves);
        for effect in effects {
            self.carry_out(devices, effect);
        }
    }

    /// Do what a hand-off asked of the session.
    fn carry_out(&self, devices: &crate::owner_session::OwnerDevices, effect: Effect) {
        let (record, _) = devices.record();
        match effect {
            Effect::Offer(offer) => record.invoke_unwaited(&Devices::offering_continuity(&offer), |made| {
                if let Err(error) = made {
                    tracing::warn!(%error, "the call was not offered to the device it moves to");
                }
            }),
            Effect::Answer {
                from,
                at_ms,
                taken,
                reason,
            } => {
                let local = devices.cert().device_public_key;
                let answering =
                    Devices::answering_continuity(&from, at_ms, &local, taken, reason.as_deref());
                record.invoke_unwaited(&answering, |made| {
                    if let Err(error) = made {
                        tracing::warn!(%error, "an answer to a handed call was not kept");
                    }
                });
            }
            Effect::Open(open) => {
                let Some(apps) = self.shared.launch.as_ref() else {
                    return;
                };
                let reference = tilo_reference::ResourceRef::whole("", open.resource, "");
                if let Err(error) =
                    apps.open_reference(&open.workspace, &open.app, Some(&open.record), &reference)
                {
                    tracing::warn!(%error, "the call handed here could not be shown");
                }
            }
            Effect::Tell { title, body } => self.shared.notifications.borrow().tell(
                &crate::platform::notice::Notice::new(t("call-move-app"), 1, title, body),
            ),
        }
    }
}

#[cfg(test)]
mod tests;
