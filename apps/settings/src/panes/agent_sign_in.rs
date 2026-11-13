//! Agent model providers set up by signing in rather than typing a key: a.
//! `ChatGPT` subscription (the person's `OpenAI` account, in the browser), a.
//! GitHub Copilot subscription (a code entered on GitHub's page), and.

use std::sync::Arc;

use account_custody::openrouter::{self, KeyAuthorization};
use account_custody::{
    Capability, Custodian, CustodyError, DeviceAuthorization, DeviceCancel, LoopbackCancel,
    OAuthIssuer, OAuthUse,
};
use agent_provider_config::presets::Preset;
use agent_provider_config::{ProviderProfile, SecretReference, SignInCredential, SignInIssuer};
use ui::framework::Actions;
use ui::widget::{Button, Row};
use zeroize::Zeroizing;

use super::agent_models::{AgentModelsEffect, invoking};
use super::agent_vertex;
use crate::channel::{self, Receiver, TryRecvError};
use crate::i18n::{t, t_args};
use crate::panes::accounts::BrowserOpener;
use crate::resource::Settings;
use provider_settings_ui::widgets::{Item, SettingsSection, note, row, rows, section};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Service {
    ChatGpt,
    Copilot,
    OpenRouter,
}

impl Service {
    pub(crate) const ALL: [Self; 3] = [Self::ChatGpt, Self::Copilot, Self::OpenRouter];

    pub(crate) fn slug(self) -> &'static str {
        self.preset().slug()
    }

    pub(crate) fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|service| service.slug() == slug)
    }

    pub(crate) fn preset(self) -> Preset {
        match self {
            Self::ChatGpt => Preset::CHATGPT,
            Self::Copilot => Preset::COPILOT,
            Self::OpenRouter => Preset::OPENROUTER,
        }
    }

    /// Who its grant is from; `OpenRouter` hands back a key instead.
    pub(crate) fn issuer(self) -> Option<SignInIssuer> {
        match self {
            Self::ChatGpt => Some(SignInIssuer::ChatGpt),
            Self::Copilot => Some(SignInIssuer::GitHubCopilot),
            Self::OpenRouter => None,
        }
    }

    pub(crate) fn of(profile: &ProviderProfile) -> Option<Self> {
        match profile.sign_in_issuer()? {
            SignInIssuer::ChatGpt => Some(Self::ChatGpt),
            SignInIssuer::GitHubCopilot => Some(Self::Copilot),
            SignInIssuer::HuggingFace | SignInIssuer::Google => None,
        }
    }

    pub(crate) fn oauth_use(self) -> Option<OAuthUse> {
        let issuer = match self {
            Self::ChatGpt => OAuthIssuer::OpenAi,
            Self::Copilot => OAuthIssuer::GitHub,
            Self::OpenRouter => return None,
        };
        Some(OAuthUse::new(issuer, Capability::Models))
    }

    pub(crate) fn sign_in_label(self) -> String {
        t(match self {
            Self::ChatGpt => "agent-sign-in-chatgpt",
            Self::Copilot => "agent-sign-in-copilot",
            Self::OpenRouter => "agent-sign-in-openrouter",
        })
    }

    pub(crate) fn available(self, custodian: &Custodian) -> bool {
        self.oauth_use()
            .is_none_or(|oauth_use| custodian.is_configured(oauth_use))
    }
}

pub(crate) enum SignedIn {
    /// A grant, already sealed under the provider's id.
    Grant,
    /// `OpenRouter`'s new API key, still to be stored.
    Key(Zeroizing<String>),
}

enum Progress<T> {
    Device {
        code: String,
        page: String,
        cancel: DeviceCancel,
    },
    Finished(Result<T, String>),
}

enum Cancel {
    None,
    Loopback(LoopbackCancel),
    Device(DeviceCancel),
}

impl Cancel {
    fn cancel(&self) {
        match self {
            Self::None => {}
            Self::Loopback(cancel) => cancel.cancel(),
            Self::Device(cancel) => cancel.cancel(),
        }
    }
}

pub(crate) enum Waited<T> {
    Nothing,
    Changed,
    Done(T),
}

pub(crate) struct SignInWait<T> {
    cancel: Cancel,
    launch: Option<ui::portal::LaunchChannel>,
    progress: Receiver<Progress<T>>,
    device: Option<(String, String)>,
    opener: Arc<dyn BrowserOpener>,
}

impl<T: Send + 'static> SignInWait<T> {
    pub(crate) fn browser(
        url: &str,
        cancel: LoopbackCancel,
        opener: &Arc<dyn BrowserOpener>,
        finish: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Self {
        let (sender, progress) = channel::sync_channel(2);
        std::thread::spawn(move || {
            let _ = sender.send(Progress::Finished(finish()));
        });
        Self {
            cancel: Cancel::Loopback(cancel),
            launch: Some(opener.open(url)),
            progress,
            device: None,
            opener: Arc::clone(opener),
        }
    }

    /// Ask the issuer for a device code on a worker (a request), report it,
    /// then wait there in `finish` until the person entered it.
    pub(crate) fn device(
        opener: &Arc<dyn BrowserOpener>,
        begin: impl FnOnce() -> Result<DeviceAuthorization, String> + Send + 'static,
        finish: impl FnOnce(DeviceAuthorization) -> Result<T, String> + Send + 'static,
    ) -> Self {
        let (sender, progress) = channel::sync_channel(2);
        std::thread::spawn(move || {
            let outcome = begin().and_then(|device| {
                let _ = sender.send(Progress::Device {
                    code: device.user_code().to_string(),
                    page: device.verification_uri().to_string(),
                    cancel: device.canceller(),
                });
                finish(device)
            });
            let _ = sender.send(Progress::Finished(outcome));
        });
        Self {
            cancel: Cancel::None,
            launch: None,
            progress,
            device: None,
            opener: Arc::clone(opener),
        }
    }

    /// Block until the worker finished, taking any device code on the way.
    #[cfg(test)]
    pub(crate) fn finish_for_test(&mut self, timeout: std::time::Duration) -> Result<T, String> {
        loop {
            match self.progress.recv_timeout(timeout) {
                Ok(Progress::Device { code, page, cancel }) => {
                    self.device = Some((code, page));
                    self.cancel = Cancel::Device(cancel);
                }
                Ok(Progress::Finished(outcome)) => return outcome,
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    pub(crate) fn device_code(&self) -> Option<(&str, &str)> {
        self.device
            .as_ref()
            .map(|(code, page)| (code.as_str(), page.as_str()))
    }

    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }

    pub(crate) fn poll(&mut self) -> Result<Waited<T>, String> {
        let mut changed = false;
        if let Some(launch) = &self.launch {
            match launch.try_recv() {
                Ok(ui::portal::LaunchOutcome::Opened) | Err(TryRecvError::Disconnected) => {
                    self.launch = None;
                }
                Ok(ui::portal::LaunchOutcome::Failed(error)) => {
                    self.launch = None;
                    // A device code can still be typed on its page by hand.
                    if self.device.is_none() {
                        self.cancel.cancel();
                        return Err(t_args(
                            "agent-sign-in-browser-failed",
                            &[("error", error.into())],
                        ));
                    }
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        loop {
            match self.progress.try_recv() {
                Ok(Progress::Device { code, page, cancel }) => {
                    self.launch = Some(self.opener.open(&page));
                    self.device = Some((code, page));
                    self.cancel = Cancel::Device(cancel);
                    changed = true;
                }
                Ok(Progress::Finished(outcome)) => return outcome.map(Waited::Done),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(t("agent-sign-in-stopped")),
            }
        }
        Ok(if changed {
            Waited::Changed
        } else {
            Waited::Nothing
        })
    }
}

pub(crate) struct SignInSetup {
    pub(crate) service: Service,
    pub(crate) profile_id: String,
    pub(crate) existing: bool,
    pub(crate) reply: Option<ui::framework::EffectReply>,
    wait: SignInWait<SignedIn>,
}

impl SignInSetup {
    pub(crate) fn begin(
        service: Service,
        profile: Option<&ProviderProfile>,
        custodian: &Custodian,
        opener: &Arc<dyn BrowserOpener>,
    ) -> Result<Self, String> {
        let profile_id = profile.map_or_else(
            || uuid::Uuid::new_v4().to_string(),
            |profile| profile.id.clone(),
        );
        let grant = agent_vertex::grant_id(&profile_id);
        let failed = |error: CustodyError| sign_in_error(service, &error);
        let wait = match service {
            Service::OpenRouter => {
                let authorization =
                    KeyAuthorization::begin(openrouter::AUTH_ENDPOINT, openrouter::KEYS_ENDPOINT)
                        .map_err(failed)?;
                let url = authorization.url().to_owned();
                SignInWait::browser(&url, authorization.canceller(), opener, move || {
                    authorization
                        .finish()
                        .map(SignedIn::Key)
                        .map_err(|error| sign_in_error(service, &error))
                })
            }
            Service::Copilot => {
                let oauth_use = service
                    .oauth_use()
                    .ok_or_else(|| t("agent-sign-in-stopped"))?;
                let custodian = custodian.clone();
                let sealing = custodian.clone();
                SignInWait::device(
                    opener,
                    move || {
                        custodian
                            .begin_device_sign_in(oauth_use)
                            .map_err(|error| sign_in_error(service, &error))
                    },
                    move |device| {
                        Custodian::finish_device_sign_in(device)
                            .and_then(|tokens| sealing.seal_oauth(&grant, &tokens))
                            .map(|()| SignedIn::Grant)
                            .map_err(|error| sign_in_error(service, &error))
                    },
                )
            }
            Service::ChatGpt => {
                let oauth_use = service
                    .oauth_use()
                    .ok_or_else(|| t("agent-sign-in-stopped"))?;
                let authorization = custodian
                    .begin_sign_in_for(oauth_use, None)
                    .map_err(failed)?;
                let cancel = authorization
                    .canceller()
                    .ok_or_else(|| t("agent-sign-in-ports-busy"))?;
                let url = authorization.url().to_owned();
                let custodian = custodian.clone();
                SignInWait::browser(&url, cancel, opener, move || {
                    Custodian::finish_sign_in(authorization)
                        .and_then(|tokens| custodian.seal_oauth(&grant, &tokens))
                        .map(|()| SignedIn::Grant)
                        .map_err(|error| sign_in_error(service, &error))
                })
            }
        };
        Ok(Self {
            service,
            profile_id,
            existing: profile.is_some(),
            reply: None,
            wait,
        })
    }

    pub(crate) fn cancel(&mut self) {
        self.wait.cancel();
        if let Some(reply) = self.reply.take() {
            reply.finish(Err(t("agent-sign-in-cancelled")));
        }
    }

    pub(crate) fn poll(&mut self) -> Result<Waited<SignedIn>, String> {
        self.wait.poll()
    }

    pub(crate) fn section(&self, actions: &Actions<Settings>) -> SettingsSection {
        let mut body: Vec<Item> = Vec::new();
        match self.wait.device_code() {
            Some((code, page)) => body.push(device_code_note(code, page)),
            None if self.service == Service::Copilot => {
                body.push(note(&t("agent-sign-in-device-asking")));
            }
            None => body.push(note(&t("agent-sign-in-waiting"))),
        }
        if self.service == Service::ChatGpt {
            body.push(note(&t("agent-sign-in-chatgpt-unofficial")));
        }
        body.push(row(Box::new(Row::new().push(Box::new(invoking(
            actions,
            Button::new("agent-models-sign-in-cancel", &t("agent-cancel")),
            AgentModelsEffect::CancelSignIn,
        ))))));
        section(&self.service.sign_in_label(), rows(body))
    }
}

pub(crate) fn device_code_note(code: &str, page: &str) -> Item {
    note(&t_args(
        "agent-sign-in-device-code",
        &[("code", code.into()), ("page", page.into())],
    ))
}

pub(crate) fn new_profile(service: Service, profile_id: &str) -> ProviderProfile {
    let mut profile = service.preset().profile();
    profile.id = profile_id.to_string();
    match service.issuer() {
        Some(issuer) => {
            profile.credential = None;
            profile.sign_in = Some(SignInCredential::new(issuer));
        }
        None => profile.credential = Some(SecretReference::provider_api_key(profile_id)),
    }
    profile
}

fn sign_in_error(service: Service, error: &CustodyError) -> String {
    let name = service.preset().label();
    match error {
        CustodyError::NotConfigured { .. } => {
            t_args("agent-sign-in-not-configured", &[("service", name.into())])
        }
        CustodyError::Offline => t_args("agent-sign-in-offline", &[("service", name.into())]),
        other => t_args(
            "agent-sign-in-failed",
            &[
                ("service", name.into()),
                ("error", other.to_string().into()),
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_is_its_preset_and_signs_in_again_as_itself() {
        for service in Service::ALL {
            assert_eq!(Service::from_slug(service.slug()), Some(service));
            let profile = new_profile(service, "p-1");
            assert_eq!(profile.id, "p-1");
            if service == Service::OpenRouter {
                assert_eq!(
                    profile.credential,
                    Some(SecretReference::provider_api_key("p-1"))
                );
                assert_eq!(Service::of(&profile), None);
                assert_eq!(service.oauth_use(), None);
            } else {
                assert!(service.oauth_use().is_some());
                assert!(profile.credential.is_none());
                assert_eq!(Service::of(&profile), Some(service));
            }
        }
    }
}
