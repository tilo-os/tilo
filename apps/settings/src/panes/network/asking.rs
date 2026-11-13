//! Every secret Network needs is asked with the window's password prompt
//! (`crate::secrets`): a network's password to join it, a hidden network's name
//! and password, an enterprise network's password, a saved network's new
//! password, the hotspot's password and a `WireGuard` private key.

use ui::icons::Icon;
use ui::widget::{FieldSpec, PromptOutcome, SecretAnswer, SecretKind, SecretPrompt};

use super::{Begun, EnterpriseGiven, NetworkChange, NetworkModel};
use crate::i18n::{t, t_args};

const KEY: &str = "network";

pub(super) enum Asked {
    Join(String),
    Hidden,
    Enterprise(Box<EnterpriseGiven>),
    NewPassword {
        uuid: String,
        name: String,
    },
    /// The hotspot's password, kept until the hotspot starts.
    Hotspot,
    /// A `WireGuard` tunnel's private key, kept until it is created.
    PrivateKey,
}

pub(super) struct Asking {
    pub(super) what: Asked,
    busy: bool,
}

impl NetworkModel {
    fn ask(&mut self, what: Asked, prompt: SecretPrompt) {
        self.reach
            .secrets
            .ask(KEY, prompt.with_id("settings.network.prompt"));
        self.asking = Some(Asking { what, busy: false });
        self.changed = true;
    }

    pub(super) fn asking_to_join(&self, ssid: &str) -> bool {
        self.asking
            .as_ref()
            .is_some_and(|asking| matches!(&asking.what, Asked::Join(asked) if asked == ssid))
    }

    /// Ask for the password of `ssid`; asked already, the one given was
    /// refused.
    pub(super) fn ask_join(&mut self, ssid: &str, security: &str) {
        if self.asking_to_join(ssid) {
            self.refuse_prompt(t("net-wrong-password"));
            return;
        }
        let prompt = SecretPrompt::new(SecretKind::Secret)
            .with_icon(Icon::Wifi)
            .with_title(t_args("net-prompt-join", &[("ssid", ssid.into())]))
            .with_resource(security)
            .with_primary_label(t("net-join"));
        self.ask(Asked::Join(ssid.to_owned()), prompt);
    }

    pub(super) fn ask_hidden(&mut self) {
        let prompt = SecretPrompt::with_fields(vec![
            FieldSpec::text(t("net-ssid")),
            FieldSpec::secret(t("net-password")).optional(),
        ])
        .with_icon(Icon::Wifi)
        .with_title(t("net-hidden-title"))
        .with_resource(t("net-hidden-caption"))
        .with_primary_label(t("net-join"));
        self.ask(Asked::Hidden, prompt);
    }

    pub(super) fn ask_enterprise(&mut self, given: EnterpriseGiven, ssid: &str, identity: &str) {
        let prompt = SecretPrompt::new(SecretKind::Secret)
            .with_icon(Icon::Landmark)
            .with_title(t_args("net-prompt-join", &[("ssid", ssid.into())]))
            .with_resource(identity)
            .with_primary_label(t("net-save-connect"));
        self.ask(Asked::Enterprise(Box::new(given)), prompt);
    }

    pub(super) fn ask_new_password(&mut self, uuid: String, name: String) {
        let prompt = SecretPrompt::new(SecretKind::Secret)
            .with_icon(Icon::Wifi)
            .with_title(t_args(
                "net-prompt-new-password",
                &[("ssid", name.as_str().into())],
            ))
            .with_label(ui::widget::FieldRole::Secret, t("net-new-password"))
            .with_primary_label(t("net-change-password"));
        self.ask(Asked::NewPassword { uuid, name }, prompt);
    }

    pub(super) fn ask_hotspot_password(&mut self) {
        let prompt = SecretPrompt::new(SecretKind::Secret)
            .with_icon(Icon::Wifi)
            .with_title(t("net-hotspot-password"))
            .with_resource(t("net-hotspot-password-caption"))
            .with_primary_label(t("net-prompt-use"));
        self.ask(Asked::Hotspot, prompt);
    }

    pub(super) fn ask_private_key(&mut self) {
        let prompt = SecretPrompt::new(SecretKind::Secret)
            .with_icon(Icon::Shield)
            .with_title(t("net-vpn-private-key"))
            .with_resource(t("net-vpn-private-key-caption"))
            .with_label(ui::widget::FieldRole::Secret, t("net-vpn-private-key"))
            .with_primary_label(t("net-prompt-use"));
        self.ask(Asked::PrivateKey, prompt);
    }

    pub(super) fn close_prompt(&mut self) {
        if self.asking.take().is_some() {
            self.reach.secrets.close(KEY);
            self.changed = true;
        }
    }

    /// The answer given was refused: say why and ask again.
    fn refuse_prompt(&mut self, why: String) {
        if let Some(asking) = &mut self.asking {
            asking.busy = false;
            self.reach.secrets.reject(KEY, why);
        }
    }

    /// A change the prompt started landed: done, it closes; refused, it
    /// says why and asks again. A prompt that is not waiting is left alone.
    pub(super) fn settle_prompt(&mut self, landed: &Result<String, String>) {
        if !self.asking.as_ref().is_some_and(|asking| asking.busy) {
            return;
        }
        match landed {
            Ok(_) => self.close_prompt(),
            Err(why) => self.refuse_prompt(why.clone()),
        }
    }

    pub(super) fn poll_prompt(&mut self) {
        let Some(outcome) = self.reach.secrets.take_answer(KEY) else {
            return;
        };
        match outcome {
            PromptOutcome::Cancel => {
                if self.asking.take().is_some() {
                    self.changed = true;
                }
            }
            PromptOutcome::Submit(answer) => match self.answered(answer) {
                Ok(Some(Begun::Work(work))) => self.start_job("settings-network-prompt", work),
                Ok(_) => {}
                Err(why) => self.refuse_prompt(why),
            },
        }
    }

    fn answered(&mut self, answer: SecretAnswer) -> Result<Option<Begun>, String> {
        let Some(asking) = self.asking.as_mut() else {
            return Ok(None);
        };
        let wait = |model: &mut Self, words: String| {
            if let Some(asking) = &mut model.asking {
                asking.busy = true;
            }
            model.reach.secrets.busy(KEY, Some(words));
        };
        match &asking.what {
            Asked::Join(ssid) => {
                let ssid = ssid.clone();
                let begun = self.connect(ssid.clone(), Some(answer.secret))?;
                wait(
                    self,
                    t_args("net-notice-connecting", &[("ssid", ssid.into())]),
                );
                Ok(Some(begun))
            }
            Asked::Hidden => {
                let mut values = answer.values.into_iter();
                let ssid = values
                    .next()
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| t("net-hidden-needs-name"))?;
                let password = values.next().filter(|password| !password.is_empty());
                let begun = self.join_hidden(ssid.clone(), password);
                wait(
                    self,
                    t_args("net-notice-connecting", &[("ssid", ssid.into())]),
                );
                Ok(Some(begun))
            }
            Asked::Enterprise(given) => {
                let given = (**given).clone();
                let begun = self.connect_enterprise(given, answer.secret);
                wait(self, t("net-notice-enterprise-connecting"));
                Ok(Some(begun))
            }
            Asked::NewPassword { uuid, name } => {
                let change = NetworkChange::Password {
                    uuid: uuid.clone(),
                    name: name.clone(),
                    password: answer.secret,
                };
                self.revealed = None;
                let begun = self.change(change, None);
                wait(self, t("net-prompt-changing-password"));
                Ok(Some(begun))
            }
            Asked::Hotspot => {
                keep(&self.hotspot_password, &answer.secret);
                self.close_prompt();
                Ok(None)
            }
            Asked::PrivateKey => {
                let public = os_actions::network_settings::wireguard_public_key(&answer.secret)
                    .map_err(|_| t("net-vpn-private-key-wrong"))?;
                keep(&self.vpn.wireguard_private_key(), &answer.secret);
                self.vpn.set_wireguard_public_key(public);
                self.close_prompt();
                Ok(None)
            }
        }
    }
}

fn keep(buffer: &std::sync::Mutex<String>, secret: &str) {
    if let Ok(mut held) = buffer.lock() {
        zeroize::Zeroize::zeroize(&mut *held);
        held.push_str(secret);
    }
}
