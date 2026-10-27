//! The authentication and network-secrets sheets ask with the one
//! password prompt every part of Tilo uses ([`SecretPrompt`]): built from
//! the queued request, answered through it, refused in it.

use ui::icons::Icon;
use ui::widget::{
    EventCtx, FieldRole, FieldSpec, PromptOutcome, SecretAnswer, SecretKind, SecretPrompt, Widget,
};
use vello::kurbo::Rect;
use winit::event::WindowEvent;

use super::{
    AuthenticationRequest, DialogView, NetworkSecretsDecision, NetworkSecretsRequest,
    PortalDialogController, PukRefusal, SecretFieldRule, SimUnlock, SimUnlockStep,
    authentication_actions, forward_to_widget,
};
use crate::shell::i18n::{t, t_args};

impl PortalDialogController {
    /// Record where the on-screen keyboard covers the desktop (`None`
    /// while it is down).
    pub fn set_keyboard_cover(&mut self, cover: Option<Rect>) {
        self.keyboard_cover = cover;
    }

    /// How far up from the bottom of `area` the on-screen keyboard reaches.
    pub fn keyboard_inset(&self, area: Rect) -> f64 {
        self.keyboard_cover
            .filter(|cover| cover.intersect(area).area() > 0.0)
            .map_or(0.0, |cover| (area.y1 - cover.y0).max(0.0))
    }

    /// The password prompt, when the sheet up is one.
    pub fn prompt(&self) -> Option<&SecretPrompt> {
        self.prompt.as_ref()
    }

    /// The password prompt, for the painter to lay out and draw.
    pub fn prompt_mut(&mut self) -> Option<&mut SecretPrompt> {
        self.prompt.as_mut()
    }

    /// Hand any input to the password prompt — it is modal, so every key,
    /// pointer and touch event is its — and carry out its answer once it
    /// has one. `false` when no prompt is up.
    pub fn prompt_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        let Some(prompt) = &mut self.prompt else {
            return false;
        };
        forward_to_widget(prompt, event, ctx);
        self.settle_prompt();
        true
    }

    /// The authentication request this sheet shows, while it is still the
    /// attempt that is waiting.
    pub(super) fn live_ticket(&self) -> Option<authentication_actions::AuthenticationTicket> {
        self.queue.current_authentication().filter(|ticket| {
            Some(ticket.generation) == self.active_authentication_generation
                && self.active.as_deref() == Some(ticket.request().handle.as_str())
        })
    }

    /// Carry out what the person did in the prompt, if they answered or
    /// backed out. `true` when the sheet closed.
    pub(super) fn settle_prompt(&mut self) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        let Some(outcome) = self.prompt.as_mut().and_then(SecretPrompt::take_outcome) else {
            return false;
        };
        let done = match (outcome, &view) {
            (PromptOutcome::Cancel, DialogView::Authentication { .. }) => {
                let Some(ticket) = self.live_ticket() else {
                    return false;
                };
                self.forget_secret();
                self.queue.cancel_authentication(&ticket)
            }
            (PromptOutcome::Cancel, _) => {
                self.forget_secret();
                self.queue
                    .finish_network_secrets(view.handle(), NetworkSecretsDecision::Cancelled)
            }
            (
                PromptOutcome::Submit(answer),
                DialogView::Authentication {
                    identities,
                    identity,
                    ..
                },
            ) => {
                let identity = if identities.is_empty() {
                    *identity
                } else {
                    answer.choice.unwrap_or(self.selected)
                };
                self.answer_authentication(answer, identity)
            }
            (
                PromptOutcome::Submit(answer),
                DialogView::NetworkSecrets {
                    sim:
                        Some(SimUnlock {
                            step: SimUnlockStep::Puk,
                            sim_path,
                            ..
                        }),
                    ..
                },
            ) => {
                self.send_puk(sim_path.as_deref(), answer);
                false
            }
            (PromptOutcome::Submit(answer), _) => {
                self.answer_network_secrets(view.handle(), answer)
            }
        };
        if done {
            self.active = None;
        }
        done
    }

    fn answer_authentication(&mut self, mut answer: SecretAnswer, identity: usize) -> bool {
        let Some(ticket) = self.live_ticket() else {
            return false;
        };
        let Some(offered) = ticket.request().identities.get(identity).cloned() else {
            return false;
        };
        // The answer's buffer itself moves on to PAM's helper; no copy of
        // it is left behind.
        let secret = answer
            .values
            .first_mut()
            .map(|value| std::mem::take(&mut **value))
            .unwrap_or_default();
        self.forget_secret();
        self.queue.answer_authentication(&ticket, &offered, secret)
    }

    /// Hand the PUK and the new PIN to the modem off the compositor thread;
    /// the sheet waits, busy, for its answer ([`Self::settle_puk_attempt`]).
    fn send_puk(&mut self, sim_path: Option<&str>, mut answer: SecretAnswer) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let (Some(sim_path), Some(new_pin)) = (sim_path, answer.new_pin.take()) else {
            prompt.reject(t("portal-sim-unlock-failed"));
            return;
        };
        let puk = std::mem::take(&mut answer.secret);
        drop(answer);
        prompt.set_busy(Some(t("portal-sim-unlocking")));
        let (sender, receiver) = std::sync::mpsc::channel();
        self.puk_attempt = Some(receiver);
        let unlocker = std::sync::Arc::clone(&self.sim_unlocker);
        let sim_path = sim_path.to_owned();
        let spawned = std::thread::Builder::new()
            .name("sim-puk".into())
            .spawn(move || {
                let _ = sender.send(unlocker.send_puk(&sim_path, puk, new_pin));
                crate::shell::wake_compositor();
            });
        if spawned.is_err() {
            self.puk_attempt = None;
            if let Some(prompt) = &mut self.prompt {
                prompt.reject(t("portal-sim-unlock-failed"));
            }
        }
    }

    /// Carry out the modem's answer to the PUK the sheet sent, once it has
    /// one: the sheet closes on success and says why in place otherwise,
    /// with the attempts the SIM has left. `true` when anything changed.
    pub(super) fn settle_puk_attempt(&mut self) -> bool {
        let Some(receiver) = &self.puk_attempt else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(PukRefusal::Failed(t("portal-sim-unlock-failed")))
            }
        };
        self.apply_puk_result(result)
    }

    /// Wait for the modem's answer to the PUK in flight, then carry it out.
    /// `false` when none is in flight.
    #[cfg(test)]
    pub(crate) fn wait_for_puk_attempt(&mut self) -> bool {
        let Some(receiver) = &self.puk_attempt else {
            return false;
        };
        let result = receiver
            .recv()
            .unwrap_or_else(|_| Err(PukRefusal::Failed(t("portal-sim-unlock-failed"))));
        self.apply_puk_result(result)
    }

    fn apply_puk_result(&mut self, result: Result<(), PukRefusal>) -> bool {
        self.puk_attempt = None;
        match result {
            Ok(()) => {
                let Some(view) = self.view() else {
                    return false;
                };
                self.forget_secret();
                if self
                    .queue
                    .finish_network_secrets(view.handle(), NetworkSecretsDecision::UnlockedWithPuk)
                {
                    self.active = None;
                }
                true
            }
            Err(refusal) => {
                let Some(prompt) = &mut self.prompt else {
                    return false;
                };
                match refusal {
                    PukRefusal::Wrong { attempts_left } => {
                        prompt.set_attempts_left(attempts_left);
                        prompt.reject_at(0, t("portal-sim-puk-wrong"));
                    }
                    PukRefusal::Failed(reason) => prompt.reject_at(0, reason),
                }
                true
            }
        }
    }

    /// Check the answer against each field's rule; a refusal stays in the
    /// prompt, on the field it is about.
    fn answer_network_secrets(&mut self, handle: &str, answer: SecretAnswer) -> bool {
        let (fields, _) = self.queue.network_fields(handle);
        let refusal =
            fields
                .iter()
                .zip(&answer.values)
                .enumerate()
                .find_map(|(index, (field, value))| {
                    if value.is_empty() {
                        field.required.then(|| (index, t("portal-secret-required")))
                    } else if !field.rule.accepts(value.as_str()) {
                        Some((index, t("portal-secret-wifi-length")))
                    } else {
                        None
                    }
                });
        if let Some((index, reason)) = refusal {
            if let Some(prompt) = &mut self.prompt {
                prompt.refuse_at(index, reason);
            }
            return false;
        }
        let remember = answer.option.unwrap_or(false);
        let values = answer.values;
        self.forget_secret();
        self.queue.finish_network_secrets(
            handle,
            NetworkSecretsDecision::Answered { values, remember },
        )
    }
}

/// The polkit sheet as the shared password prompt: the request's message
/// as its title, who answers and for which action under it, PAM's prompt
/// naming the field and the last attempt's failure as its error.
pub(super) fn authentication_prompt(request: &AuthenticationRequest) -> SecretPrompt {
    let label = request.prompt.trim().trim_end_matches(':').trim();
    let label = if label.is_empty() {
        t("portal-network-password")
    } else {
        label.to_owned()
    };
    let field = if request.echo {
        FieldSpec::text(label)
    } else {
        FieldSpec::secret(label)
    };
    let resource = match request.identities.as_slice() {
        [user] => format!(
            "{}\n{}",
            t_args("portal-auth-as", &[("user", user.as_str().into())]),
            request.action_id
        ),
        _ => request.action_id.clone(),
    };
    // PAM decides what it accepts, an empty answer included.
    let mut prompt = SecretPrompt::with_fields(vec![field.optional()])
        .with_id("portal-authentication")
        .with_icon(Icon::Shield)
        .with_title(request.message.clone())
        .with_resource(resource)
        .with_primary_label(t("portal-authenticate"))
        .with_cancel_label(t("portal-cancel"));
    if request.identities.len() > 1 {
        prompt = prompt.with_choices(
            t("portal-auth-identity-param"),
            request.identities.clone(),
            request.identity,
        );
    }
    if let Some(notice) = request
        .notice
        .as_deref()
        .filter(|notice| !notice.trim().is_empty())
    {
        prompt.reject(notice);
    }
    Widget::set_focus(&mut prompt, true);
    prompt
}

/// A network-secrets sheet as the shared password prompt. The agent's
/// "the last attempt did not work" line becomes the prompt's error.
pub(super) fn network_prompt(request: &NetworkSecretsRequest) -> SecretPrompt {
    let retry = t("portal-network-retry");
    let mut retried = false;
    let resource = request
        .detail
        .lines()
        .filter(|line| {
            let is_retry = *line == retry;
            retried |= is_retry;
            !is_retry
        })
        .collect::<Vec<_>>()
        .join("\n");
    let fields = request
        .fields
        .iter()
        .map(|field| {
            let spec = if field.masked {
                FieldSpec::secret(field.label.clone())
            } else {
                FieldSpec::text(field.label.clone()).with_initial(field.initial.clone())
            };
            if field.required {
                spec
            } else {
                spec.optional()
            }
        })
        .collect();
    let wifi = request.fields.iter().any(|field| {
        field.rule == SecretFieldRule::WpaPassphrase
            || field.key == "psk"
            || field.key.starts_with("wep-key")
    });
    if let Some(sim) = &request.sim {
        return sim_prompt(request, sim, resource, retried);
    }
    let mut prompt = SecretPrompt::with_fields(fields)
        .with_id("portal-network-secret")
        .with_icon(if wifi { Icon::Wifi } else { Icon::Lock })
        .with_title(request.title.clone())
        .with_resource(resource)
        .with_primary_label(t("portal-connect"))
        .with_cancel_label(t("portal-cancel"));
    if let Some(remember) = request.remember {
        prompt = prompt.with_option(t("portal-remember-device"), remember);
    }
    if retried {
        let index = request
            .fields
            .iter()
            .position(|field| field.masked)
            .unwrap_or(0);
        prompt.reject_at(index, retry);
    }
    Widget::set_focus(&mut prompt, true);
    prompt
}

/// A SIM's PIN, or its PUK and a new PIN once wrong PINs locked it: digits
/// only, a keypad while touch is in use, the attempts the SIM has left, and
/// Unlock. The SIM keeps its PIN, so nothing offers to remember it.
fn sim_prompt(
    request: &NetworkSecretsRequest,
    sim: &SimUnlock,
    resource: String,
    retried: bool,
) -> SecretPrompt {
    let prompt = match sim.step {
        SimUnlockStep::Pin => {
            let mut prompt = SecretPrompt::new(SecretKind::SimPin {
                attempts_left: sim.attempts_left,
            });
            if let Some(field) = request.fields.first() {
                prompt = prompt.with_label(FieldRole::Pin, field.label.clone());
            }
            prompt
        }
        SimUnlockStep::Puk => SecretPrompt::new(SecretKind::SimPuk {
            attempts_left: sim.attempts_left,
        }),
    };
    let mut prompt = prompt
        .with_id("portal-network-secret")
        .with_icon(Icon::Lock)
        .with_title(request.title.clone())
        .with_resource(resource)
        .with_primary_label(t("portal-sim-unlock"))
        .with_cancel_label(t("portal-cancel"));
    // NetworkManager asks again after a wrong PIN; a PUK is refused in place.
    if retried && sim.step == SimUnlockStep::Pin {
        prompt.reject(t("portal-sim-pin-wrong"));
    }
    Widget::set_focus(&mut prompt, true);
    prompt
}
