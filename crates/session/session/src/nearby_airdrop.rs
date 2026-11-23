//! Verifying an `AirDrop` receiver on the portal sheet.
//!
//! A file handed to a receiver that is not one of the person's devices leaves only after they
//! confirm the receiver: the sheet names the file, its size and the endpoint, and the person types
//! the receiver's leaf certificate fingerprint as 64 hexadecimal digits, obtained from the
//! receiver's console or another channel that did not carry the file. A fingerprint that is not
//! the receiver's is refused on the sheet and the attempt waits for the right one; declining it,
//! pressing Escape or leaving it for two minutes withdraws the attempt and nothing is sent.
//!
//! The attempt itself runs in the Nearby Send engine, which stages the file and observes the
//! receiver's certificate; this is only the question, and the one answer that reaches it.

use std::sync::Arc;

use tilo_sheets::SheetState as _;
use tilo_sheets::portal::{Body, Dialog, Field, PortalEffect, PortalState};
use ui::i18n::ArgValue;

use crate::i18n::{t, t_args};
use crate::nearby_transfer::{AirDropAnswer, AirDropAsk, NearbyTransfer};

/// The handle the attempt's question is shown and answered under.
fn handle(id: [u8; 16]) -> String {
    format!("airdrop:{}", hex::encode(id))
}

/// What shows of one file handed to a receiver that is not one of the person's devices.
#[derive(Default)]
pub(crate) struct Consent {
    /// The attempt whose question is up, once it is.
    shown: Option<[u8; 16]>,
}

impl Consent {
    /// Follow the attempt waiting for this person, in this pass of the session's loop: put the
    /// verification up while the sheet is free, and take their answer into the attempt.
    pub(crate) fn follow(
        &mut self,
        nearby: Option<&Arc<NearbyTransfer>>,
        portal: Option<&PortalState>,
    ) {
        let (Some(nearby), Some(portal)) = (nearby, portal) else {
            self.shown = None;
            return;
        };
        if let Some(id) = self.shown {
            self.settle(nearby, portal, id);
            return;
        }
        // One question stands at a time: the sheet may already be asking something else.
        if portal.dialog().is_some() {
            return;
        }
        let Some(ask) = nearby.air_drop_ask() else {
            return;
        };
        portal.show(Some(dialog(&ask)));
        portal.presentation().open(false);
        self.shown = Some(ask.id);
    }

    /// Take what the person did on the attempt's question, or notice that it is no longer up.
    fn settle(&mut self, nearby: &Arc<NearbyTransfer>, portal: &PortalState, id: [u8; 16]) {
        let Some(ask) = nearby.air_drop_ask_of(id) else {
            // The attempt ended under the question: nothing is left to answer.
            if portal
                .dialog()
                .is_some_and(|dialog| dialog.handle == handle(id))
            {
                portal.taken_down();
            }
            self.shown = None;
            return;
        };
        if portal
            .dialog()
            .is_none_or(|dialog| dialog.handle != handle(id))
        {
            // The sheet was dismissed or another question took it: the attempt is withdrawn.
            let _ = nearby.answer_air_drop(id, AirDropAnswer::Decline);
            self.shown = None;
            return;
        }
        for effect in portal.take_asked() {
            if effect.handle() != handle(id) {
                // Another question's answer is not this sheet's to take.
                portal.press(effect);
                continue;
            }
            match effect {
                PortalEffect::Confirm { .. } => {
                    let pin = portal.text(Field::Answer).to_string();
                    match nearby.answer_air_drop(id, AirDropAnswer::Confirm { pin }) {
                        Ok(()) => {
                            portal.taken_down();
                            self.shown = None;
                        }
                        Err(why) => {
                            let mut asking = dialog(&ask);
                            asking.error = Some(why);
                            portal.show(Some(asking));
                        }
                    }
                }
                PortalEffect::Cancel { .. } => {
                    let _ = nearby.answer_air_drop(id, AirDropAnswer::Decline);
                    portal.taken_down();
                    self.shown = None;
                }
                _ => {}
            }
        }
    }
}

/// The attempt `ask` as the sheet puts it to the person.
fn dialog(ask: &AirDropAsk) -> Dialog {
    Dialog {
        handle: handle(ask.id),
        title: t("airdrop-verify-title"),
        detail: t_args(
            "airdrop-verify-detail",
            &[
                ("name", ArgValue::Text(ask.name.clone())),
                ("bytes", ArgValue::Text(ask.bytes.to_string())),
                ("endpoint", ArgValue::Text(ask.endpoint.clone())),
            ],
        ),
        body: Body::Entry {
            masked: false,
            required: true,
            choices: Vec::new(),
        },
        confirm_label: t("airdrop-verify-send-once"),
        cancel_label: t("airdrop-verify-cancel"),
        ..Dialog::default()
    }
}
