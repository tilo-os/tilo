//! The providers that sign in, as Agent Models shows and saves them: the
//! sign-in under way, the saved providers with their sign-in state, and the
//! offers to add one.

use agent_provider_config::{ProviderProfile, vertex_target};
use ui::framework::LiveRow;
use ui::tilo_resource::Effect as _;
use ui::widget::{Button, Row, Text};

use super::{AgentModelsEffect, AgentModelsModel};
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
use crate::panes::agent_sign_in::{self, SignInSetup, SignedIn};
use crate::panes::agent_vertex::{self, VertexSetup};
use crate::widgets::{
    Item, SettingsSection, StatusTone, note, row, rows, section, semantic_status_pill, value_row,
};

/// Native sign-in actions augment the shared provider row at the same public door.
pub(super) fn sign_in_provider_row(row: LiveRow, profile: &ProviderProfile) -> LiveRow {
    if let Some(service) = agent_sign_in::Service::of(profile) {
        return row.operation(
            SettingsEffect::from(AgentModelsEffect::SignInToProvider {
                service: service.slug().into(),
                provider: Some(profile.id.clone()),
            })
            .bound(),
        );
    }
    if !profile.signs_in_with_google() {
        return row;
    }
    if vertex_target(&profile.endpoint).is_none() {
        return row
            .operation(
                SettingsEffect::from(AgentModelsEffect::SignInWithGoogle {
                    provider: Some(profile.id.clone()),
                })
                .bound(),
            )
            .operation(
                SettingsEffect::from(AgentModelsEffect::SetGeminiProvider {
                    provider: Some(profile.id.clone()),
                    project: String::new(),
                })
                .bound_asking(&["project"]),
            );
    }
    row.operation(
        SettingsEffect::from(AgentModelsEffect::SignInWithGoogle {
            provider: Some(profile.id.clone()),
        })
        .bound(),
    )
    .operation(
        SettingsEffect::from(AgentModelsEffect::SetVertexProvider {
            provider: Some(profile.id.clone()),
            project: String::new(),
            location: None,
            model: None,
        })
        .bound_asking(&["project", "location", "model"]),
    )
}

impl AgentModelsModel {
    /// Sign in to `service` for the provider `provider`, or to add one;
    /// `reply` answers once it is signed in and saved.
    pub(super) fn sign_in_to(
        &mut self,
        service: &str,
        provider: Option<&str>,
        reply: Option<ui::framework::EffectReply>,
    ) -> Result<(), String> {
        let service =
            agent_sign_in::Service::from_slug(service).ok_or_else(|| t("agent-preset-unknown"))?;
        let profile = match provider {
            None => None,
            Some(id) => Some(
                self.panel
                    .provider_config()
                    .profiles
                    .iter()
                    .find(|profile| {
                        profile.id == id
                            && (agent_sign_in::Service::of(profile) == Some(service)
                                || (service == agent_sign_in::Service::OpenRouter
                                    && profile.sign_in.is_none()
                                    && profile.endpoint.trim_end_matches('/')
                                        == agent_provider_config::presets::OPENROUTER_ENDPOINT))
                    })
                    .cloned()
                    .ok_or_else(|| t("agent-provider-gone"))?,
            ),
        };
        // A key is stored through provider administration only.
        if service.issuer().is_none() && !self.cx.provider_admin.is_available() {
            return Err(t("agent-provider-offline"));
        }
        self.cancel_vertex();
        self.cancel_sign_in();
        let mut setup =
            SignInSetup::begin(service, profile.as_ref(), &self.custodian, &self.opener)?;
        setup.reply = reply;
        self.sign_in = Some(setup);
        self.changed = true;
        Ok(())
    }

    /// Stop the sign-in under way. A provider being added leaves no grant.
    pub(super) fn cancel_sign_in(&mut self) {
        if let Some(mut setup) = self.sign_in.take() {
            setup.cancel();
            if !setup.existing {
                // Nothing may have been sealed yet; there is nothing to report.
                let _ = self
                    .custodian
                    .forget_oauth(&agent_vertex::grant_id(&setup.profile_id));
            }
        }
        self.changed = true;
    }

    /// Take the sign-in's news; save the provider once it is signed in.
    pub(super) fn poll_sign_in(&mut self) {
        let Some(setup) = self.sign_in.as_mut() else {
            return;
        };
        match setup.poll() {
            Ok(agent_sign_in::Waited::Nothing) => {}
            Ok(agent_sign_in::Waited::Changed) => self.changed = true,
            Ok(agent_sign_in::Waited::Done(signed)) => {
                if let Some(setup) = self.sign_in.take() {
                    let id = setup.profile_id.clone();
                    let existing = setup.existing;
                    if let Err(error) = self.save_sign_in(setup, signed) {
                        if !existing {
                            let _ = self.custodian.forget_oauth(&agent_vertex::grant_id(&id));
                        }
                        self.notice = Some(error);
                    }
                }
                self.changed = true;
            }
            Err(error) => {
                if let Some(mut setup) = self.sign_in.take() {
                    if !setup.existing {
                        let _ = self
                            .custodian
                            .forget_oauth(&agent_vertex::grant_id(&setup.profile_id));
                    }
                    if let Some(reply) = setup.reply.take() {
                        reply.finish(Err(error.clone()));
                    }
                }
                self.notice = Some(error);
                self.changed = true;
            }
        }
    }

    /// Save what a finished sign-in brought: a new provider with its grant
    /// or key, or the new key of an existing one. Answers the sign-in's
    /// action when saved.
    fn save_sign_in(&mut self, mut setup: SignInSetup, signed: SignedIn) -> Result<(), String> {
        let reply = setup.reply.take();
        let id = setup.profile_id.clone();
        let done = || ui::tilo_actions::ActionOutput {
            value: None,
            resources: Vec::new(),
        };
        let label = setup.service.preset().label();
        match (signed, setup.existing, reply) {
            (SignedIn::Grant, true, reply) => {
                self.read_vertex_keys();
                self.notice = Some(t_args("agent-sign-in-done", &[("service", label.into())]));
                if let Some(reply) = reply {
                    reply.finish(Ok(done()));
                }
                Ok(())
            }
            (signed, existing, Some(reply)) if self.cx.provider_admin.is_available() => {
                let mut config = self.panel.provider_config().clone();
                let profile = (!existing).then(|| agent_sign_in::new_profile(setup.service, &id));
                if let Some(profile) = &profile {
                    config.upsert(profile.clone());
                }
                match signed {
                    SignedIn::Key(key) => self.panel.submit_config_with_key(
                        profile.is_some().then_some(config),
                        &id,
                        key,
                        reply,
                    )?,
                    SignedIn::Grant => {
                        self.panel.submit_config(config.sanitised(), reply)?;
                        self.vertex_commit = Some((id, profile));
                    }
                }
                self.notice = Some(t_args("agent-sign-in-done", &[("service", label.into())]));
                Ok(())
            }
            (SignedIn::Grant, false, reply) => {
                let profile = agent_sign_in::new_profile(setup.service, &id);
                self.providers
                    .settings()
                    .update(|providers| providers.config.upsert(profile))
                    .map_err(|error| error.to_string())?;
                self.providers.refresh();
                self.panel.offline_config = self.providers.get().config.clone();
                self.read_vertex_keys();
                self.notice = Some(t_args("agent-sign-in-done", &[("service", label.into())]));
                if let Some(reply) = reply {
                    reply.finish(Ok(done()));
                }
                Ok(())
            }
            (SignedIn::Key(_), _, reply) => {
                let error = t("agent-provider-offline");
                if let Some(reply) = reply {
                    reply.finish(Err(error.clone()));
                }
                Err(error)
            }
        }
    }

    /// A provider that signs in: for Google its project (and Vertex AI's
    /// location and model), whether its sign-in still holds on this device,
    /// the offer to sign in again, and its models to choose from. It has no
    /// key, endpoint or protocol to edit.
    fn push_sign_in_profile(
        &self,
        body: &mut Vec<Item>,
        profile: &ProviderProfile,
        default_profile: Option<&str>,
    ) {
        let profile_id = profile.id.clone();
        let issuer = profile.sign_in_issuer().map_or_else(
            || t("agent-sign-in-unsupported"),
            |issuer| issuer.label().to_owned(),
        );
        body.push(value_row(profile.display_label(), &issuer));
        let google = profile.signs_in_with_google();
        if google {
            self.vertex_fields(profile)
                .push(&self.cx, body, &profile_id);
        }
        let vertex = self
            .vertex
            .as_ref()
            .filter(|setup| setup.profile_id == profile_id);
        let waiting = vertex.is_some_and(VertexSetup::waiting)
            || self
                .sign_in
                .as_ref()
                .is_some_and(|setup| setup.profile_id == profile_id);
        let state = self.vertex_keys.get(&profile_id);
        if let Some(Err(error)) = state {
            body.push(row(crate::widgets::error_caption(&t_args(
                "agent-sign-in-read-error",
                &[("error", error.as_str().into())],
            ))));
        }
        // why: a sign-in status sentence or badge is a status line, not a label/control pair.
        body.push(row(if waiting {
            Box::new(Text::new(&t("agent-sign-in-waiting")).dimmed())
        } else if matches!(state, Some(Ok(true))) {
            Box::new(
                Text::new(&t_args(
                    "agent-signed-in",
                    &[("service", issuer.as_str().into())],
                ))
                .dimmed(),
            )
        } else if matches!(state, Some(Ok(false))) {
            semantic_status_pill(&t("agent-sign-in-again-needed"), StatusTone::Warning)
        } else {
            Box::new(Text::new(&t("agent-sign-in-unknown")).dimmed())
        }));
        if let Some(message) = vertex.and_then(VertexSetup::message) {
            body.push(row(crate::widgets::error_caption(message)));
        }
        body.push(self.sign_in_controls(profile, waiting));
        // Vertex AI's model is typed above; every other one is chosen from
        // what the provider lists.
        if !google || vertex_target(&profile.endpoint).is_none() {
            self.panel
                .profile_models(body, profile, self.panel.can_admin());
        }
        body.extend(self.default_button(profile, default_profile));
    }

    /// Sign in again (or stop the sign-in under way), offer to agents or
    /// not, and remove: the controls of a provider that signs in.
    fn sign_in_controls(&self, profile: &ProviderProfile, waiting: bool) -> Item {
        let profile_id = profile.id.clone();
        let google = profile.signs_in_with_google();
        let again = if google {
            Some(AgentModelsEffect::SignInWithGoogle {
                provider: Some(profile_id.clone()),
            })
        } else {
            agent_sign_in::Service::of(profile).map(|service| AgentModelsEffect::SignInToProvider {
                service: service.slug().into(),
                provider: Some(profile_id.clone()),
            })
        };
        let mut controls = Row::new();
        if waiting {
            controls = controls.push(Box::new(self.cx.bind_invocation(
                Button::new(
                    &format!("agent-models-vertex-cancel-{profile_id}"),
                    &t("agent-cancel"),
                ),
                AgentModelsEffect::CancelSignIn,
            )));
        } else if let Some(again) = again {
            controls = controls.push(Box::new(self.cx.bind_invocation(
                Button::new(
                    &format!("agent-models-vertex-sign-in-{profile_id}"),
                    &t("agent-sign-in-again"),
                ),
                again,
            )));
        }
        row(Box::new(
            controls
                .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                .push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("agent-models-enabled-{profile_id}"),
                        &if profile.enabled {
                            t("agent-disable")
                        } else {
                            t("agent-enable")
                        },
                    ),
                    AgentModelsEffect::SetProviderEnabled {
                        provider: profile_id.clone(),
                        on: !profile.enabled,
                    },
                )))
                .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                .push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("agent-models-remove-provider-{profile_id}"),
                        &t("agent-remove-provider"),
                    ),
                    AgentModelsEffect::RemoveProvider {
                        provider: profile_id,
                    },
                ))),
        ))
    }

    /// The setups under way, the hosted providers and their request limits,
    /// then the providers that sign in and the offers to add one.
    pub(super) fn push_provider_cards(&self, cards: &mut Vec<SettingsSection>) {
        if let Some(setup) = self.vertex.as_ref().filter(|setup| !setup.existing) {
            cards.push(setup.section(&self.cx));
        }
        // Also for signing in again: GitHub's code is shown here.
        if let Some(setup) = self.sign_in.as_ref() {
            cards.push(setup.section(&self.cx));
        }
        cards.extend(self.panel.sections_without_sign_in());
        let config = self.panel.provider_config();
        let mut body = vec![note(&t("agent-sign-in-intro"))];
        let mut services = Row::new();
        for (index, service) in agent_sign_in::Service::ALL
            .into_iter()
            .filter(|service| service.available(&self.custodian))
            .enumerate()
        {
            if index > 0 {
                services = services.push(crate::widgets::gap(ui::tokens::SpacingStep::S2));
            }
            services = services.push(Box::new(self.cx.bind_invocation(
                Button::new(
                    &format!("agent-models-sign-in-{}", service.slug()),
                    &service.sign_in_label(),
                ),
                AgentModelsEffect::SignInToProvider {
                    service: service.slug().into(),
                    provider: None,
                },
            )));
        }
        body.push(row(Box::new(services)));
        body.push(row(Box::new(
            Row::new()
                .push(Box::new(self.cx.bind_invocation(
                    Button::new("agent-models-add-vertex", &t("agent-add-vertex")),
                    AgentModelsEffect::AddGoogleVertex,
                )))
                .push(crate::widgets::gap(ui::tokens::SpacingStep::S2))
                .push(Box::new(self.cx.bind_invocation(
                    Button::new("agent-models-add-gemini", &t("agent-add-gemini")),
                    AgentModelsEffect::AddGoogleGemini,
                ))),
        )));
        if self
            .sign_in
            .as_ref()
            .is_some_and(|setup| setup.service == agent_sign_in::Service::ChatGpt)
            || config.profiles.iter().any(|profile| {
                agent_sign_in::Service::of(profile) == Some(agent_sign_in::Service::ChatGpt)
            })
        {
            body.push(note(&t("agent-sign-in-chatgpt-unofficial")));
        }
        for profile in config
            .profiles
            .iter()
            .filter(|profile| profile.sign_in.is_some())
        {
            self.push_sign_in_profile(&mut body, profile, config.default_profile.as_deref());
        }
        cards.push(section(&t("agent-sign-in-title"), rows(body)));
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{mock, model_over, snapshot};
    use super::*;
    use crate::panes::model::PaneModel as _;
    use account_custody::{Capability, Custodian, GrantState, OAuthIssuer, OAuthUse};
    use std::sync::Arc;

    /// A browser that follows the sign-in page to its redirect.
    struct Browser;
    impl crate::panes::accounts::BrowserOpener for Browser {
        fn open(&self, url: &str) -> ui::portal::LaunchChannel {
            let url = url.to_owned();
            std::thread::spawn(move || tilo_oauth::fake::browse(&url).unwrap());
            let (sender, launch) = std::sync::mpsc::channel();
            sender.send(ui::portal::LaunchOutcome::Opened).unwrap();
            launch
        }
    }

    /// Signing in with a `ChatGPT` account adds a provider holding its own
    /// grant, which only this device's vault keeps; cancelling one leaves
    /// none behind.
    #[test]
    fn a_chatgpt_sign_in_adds_a_provider_with_its_own_grant() {
        let server = tilo_oauth::fake::FakeAuthorizationServer::start();
        let vault = tempfile::tempdir().unwrap();
        let mut pane = model_over(mock(snapshot(None)));
        pane.custodian = Custodian::for_root(Some(vault.path().to_path_buf())).with_use(
            OAuthUse::new(OAuthIssuer::OpenAi, Capability::Models),
            server.provider(),
            "fake-client",
        );
        pane.opener = Arc::new(Browser);
        pane.sign_in_to("chatgpt", None, None).unwrap();
        let id = pane.sign_in.as_ref().unwrap().profile_id.clone();
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        while pane.sign_in.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "the sign-in never finished"
            );
            pane.poll_sign_in();
            std::thread::yield_now();
        }
        assert_eq!(
            pane.notice.as_deref(),
            Some(t_args("agent-sign-in-done", &[("service", "ChatGPT".into())]).as_str())
        );
        let stored = pane.providers.get().config.clone();
        let profile = stored
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .unwrap();
        assert_eq!(
            profile.sign_in_issuer(),
            Some(agent_provider_config::SignInIssuer::ChatGpt)
        );
        assert!(profile.credential.is_none());
        assert_eq!(
            profile.endpoint,
            agent_provider_config::presets::CHATGPT_ENDPOINT
        );
        assert!(stored.validation_errors().is_empty());
        assert_eq!(
            pane.custodian
                .oauth_grant_state(&agent_vertex::grant_id(&id))
                .unwrap(),
            GrantState::Usable
        );
        assert_eq!(server.verifiers().len(), 1, "one PKCE exchange");
        let text = crate::widgets::announced_text(pane.compose().as_ref());
        assert!(
            text.iter()
                .any(|line| line.contains("OpenAI doesn't offer ChatGPT sign-in")),
            "{text:#?}"
        );

        // A sign-in cancelled before it finished saves nothing.
        pane.sign_in_to("chatgpt", None, None).unwrap();
        let cancelled = pane.sign_in.as_ref().unwrap().profile_id.clone();
        pane.cancel_sign_in();
        assert!(pane.sign_in.is_none());
        assert_eq!(
            pane.custodian
                .oauth_grant_state(&agent_vertex::grant_id(&cancelled))
                .unwrap(),
            GrantState::Missing
        );
        assert_eq!(pane.providers.get().config.profiles.len(), 1);
    }

    /// `OpenRouter` hands back a key, and a key is stored only through
    /// provider administration: without it nothing starts.
    #[test]
    fn an_openrouter_sign_in_needs_provider_administration() {
        let mut pane = model_over(mock(snapshot(None)));
        assert_eq!(
            pane.sign_in_to("openrouter", None, None),
            Err(t("agent-provider-offline"))
        );
        assert!(pane.sign_in.is_none());
        assert_eq!(
            pane.sign_in_to("claude", None, None),
            Err(t("agent-preset-unknown"))
        );
    }
}
