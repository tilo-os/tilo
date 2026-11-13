//! Google Vertex AI, or the Gemini API, as an agent model provider, signed in
//! with Google rather than set up with an API key. The person signs in in the
//! system browser (PKCE with a loopback redirect, through account custody),
//! then chooses the Google Cloud project billed for requests and, for Vertex
//! AI, the location they run in and the model.

use std::sync::Arc;

use account_custody::cloud_projects::{self, CloudProject};
use account_custody::{Capability, Custodian, CustodyError, OAuthIssuer, OAuthUse};
use accounts::AccountId;
use agent_provider_config::{
    ModelDescriptor, ProviderConfig, ProviderProfile, SignInCredential, VERTEX_DEFAULT_LOCATION,
    VERTEX_DEFAULT_MODEL, is_cloud_project_id, is_vertex_location, presets, sign_in_grant_id,
    vertex_endpoint, vertex_model_id, vertex_target,
};
use ui::LocalState;
use ui::framework::Actions;
use ui::widget::{Button, Row, TextInput};

use super::agent_models::{AgentModelsEffect, invoking};
use crate::effects::effect;
use crate::i18n::{t, t_args};
use crate::panes::accounts::BrowserOpener;
use crate::panes::agent_sign_in::{SignInWait, Waited};
use crate::resource::Settings;
use provider_settings_ui::widgets::{Item, SettingsSection, field_row, note, row, rows, section};

pub(crate) const GOOGLE_MODELS: OAuthUse = OAuthUse::new(OAuthIssuer::Google, Capability::Models);

/// The scope a Vertex AI grant must carry; Google lets the person untick it
/// on the consent page.
const CLOUD_PLATFORM: &str = "https://www.googleapis.com/auth/cloud-platform";

const SHOWN_PROJECTS: usize = 12;

pub(crate) fn grant_id(profile_id: &str) -> AccountId {
    AccountId::new(sign_in_grant_id(profile_id))
}

struct SignedIn {
    /// The projects the grant can see, or why they could not be listed.
    projects: Result<Vec<CloudProject>, String>,
}

enum Stage {
    Intro,
    Waiting(SignInWait<Result<SignedIn, String>>),
    Choose,
}

pub(crate) enum Polled {
    Nothing,
    Changed,
    SignedInAgain,
}

pub(crate) struct VertexSetup {
    pub(crate) profile_id: String,
    /// Whether the provider already exists.
    pub(crate) existing: bool,
    /// The Gemini API rather than Vertex AI: only the project is chosen.
    pub(crate) gemini: bool,
    stage: Stage,
    project: LocalState<String>,
    location: LocalState<String>,
    model: LocalState<String>,
    projects: Vec<CloudProject>,
    projects_error: Option<String>,
    message: Option<String>,
}

impl VertexSetup {
    pub(crate) fn adding() -> Self {
        Self::new(
            uuid::Uuid::new_v4().to_string(),
            false,
            (String::new(), VERTEX_DEFAULT_LOCATION, VERTEX_DEFAULT_MODEL),
        )
    }

    pub(crate) fn adding_gemini() -> Self {
        Self {
            gemini: true,
            ..Self::new(
                uuid::Uuid::new_v4().to_string(),
                false,
                (String::new(), "", ""),
            )
        }
    }

    pub(crate) fn again(profile: &ProviderProfile) -> Self {
        let fields = VertexFields::of(profile);
        let text = |state: &LocalState<String>| state.with(Clone::clone);
        Self {
            gemini: fields.gemini,
            ..Self::new(
                profile.id.clone(),
                true,
                (
                    text(&fields.project),
                    &text(&fields.location),
                    &text(&fields.model),
                ),
            )
        }
    }

    fn new(
        profile_id: String,
        existing: bool,
        (project, location, model): (String, &str, &str),
    ) -> Self {
        Self {
            profile_id,
            existing,
            gemini: false,
            stage: Stage::Intro,
            project: LocalState::new(project),
            location: LocalState::new(location.to_string()),
            model: LocalState::new(model.to_string()),
            projects: Vec::new(),
            projects_error: None,
            message: None,
        }
    }

    pub(crate) fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub(crate) fn waiting(&self) -> bool {
        matches!(self.stage, Stage::Waiting(_))
    }

    pub(crate) fn choosing(&self) -> bool {
        matches!(self.stage, Stage::Choose)
    }

    pub(crate) fn choose_project(&self, project: &str) {
        self.project.set(project.to_string());
    }

    #[cfg(test)]
    pub(crate) fn signed_in_for_test(&mut self, projects: Vec<CloudProject>) {
        self.stage = Stage::Choose;
        self.projects = projects;
    }

    pub(crate) fn begin(&mut self, custodian: &Custodian, opener: &Arc<dyn BrowserOpener>) {
        if self.waiting() {
            return;
        }
        self.message = None;
        let authorization = match custodian.begin_sign_in_for(GOOGLE_MODELS, None) {
            Ok(authorization) => authorization,
            Err(CustodyError::NotConfigured { .. }) => {
                self.message = Some(t("agent-vertex-not-configured"));
                return;
            }
            Err(error) => {
                self.message = Some(t_args(
                    "agent-vertex-sign-in-failed",
                    &[("error", error.to_string().into())],
                ));
                return;
            }
        };
        let Some(cancel) = authorization.canceller() else {
            self.message = Some(t("agent-vertex-ports-busy"));
            return;
        };
        let url = authorization.url().to_owned();
        let custodian = custodian.clone();
        let grant = grant_id(&self.profile_id);
        let wait = SignInWait::browser(&url, cancel, opener, move || {
            Ok(Custodian::finish_sign_in(authorization)
                .map_err(|error| error.to_string())
                .and_then(|tokens| {
                    if tokens.scope.as_deref().is_some_and(|scopes| {
                        !scopes
                            .split_whitespace()
                            .any(|scope| scope == CLOUD_PLATFORM)
                    }) {
                        return Err(t("agent-vertex-scope-missing"));
                    }
                    custodian
                        .seal_oauth(&grant, &tokens)
                        .map_err(|error| error.to_string())?;
                    let projects = cloud_projects::google_cloud_projects(
                        cloud_projects::GOOGLE_PROJECTS_ENDPOINT,
                        &tokens.access_token,
                    )
                    .map_err(|error| error.to_string());
                    Ok(SignedIn { projects })
                }))
        });
        self.stage = Stage::Waiting(wait);
    }

    pub(crate) fn cancel(&mut self) {
        if let Stage::Waiting(wait) = &self.stage {
            wait.cancel();
        }
        self.stage = Stage::Intro;
    }

    pub(crate) fn poll(&mut self) -> Polled {
        let Stage::Waiting(wait) = &mut self.stage else {
            return Polled::Nothing;
        };
        let outcome = match wait.poll() {
            Ok(Waited::Done(outcome)) => outcome,
            Ok(Waited::Nothing | Waited::Changed) => return Polled::Nothing,
            Err(error) => {
                self.stage = Stage::Intro;
                self.message = Some(error);
                return Polled::Changed;
            }
        };
        match outcome {
            Ok(_) if self.existing => {
                self.stage = Stage::Intro;
                Polled::SignedInAgain
            }
            Ok(SignedIn { projects }) => {
                self.stage = Stage::Choose;
                match projects {
                    Ok(projects) => {
                        if self.project.with(String::is_empty)
                            && let [only] = projects.as_slice()
                        {
                            self.project.set(only.id.clone());
                        }
                        self.projects = projects;
                        self.projects_error = None;
                    }
                    Err(error) => self.projects_error = Some(error),
                }
                Polled::Changed
            }
            Err(error) => {
                self.stage = Stage::Intro;
                self.message = Some(t_args(
                    "agent-vertex-sign-in-failed",
                    &[("error", error.into())],
                ));
                Polled::Changed
            }
        }
    }

    pub(crate) fn section(&self, actions: &Actions<Settings>) -> SettingsSection {
        let mut body: Vec<Item> = Vec::new();
        let cancel = || {
            Box::new(invoking(
                actions,
                Button::new("agent-models-vertex-cancel", &t("agent-cancel")),
                AgentModelsEffect::CancelSignIn,
            ))
        };
        match self.stage {
            Stage::Intro => {
                body.push(note(&t(if self.gemini {
                    "agent-gemini-intro"
                } else {
                    "agent-vertex-intro"
                })));
                body.push(row(Box::new(
                    Row::new()
                        .push(Box::new(invoking(
                            actions,
                            Button::new("agent-models-vertex-sign-in", &t("agent-vertex-sign-in")),
                            AgentModelsEffect::SignInWithGoogle { provider: None },
                        )))
                        .push(provider_settings_ui::widgets::gap(
                            ui::tokens::SpacingStep::S2,
                        ))
                        .push(cancel()),
                )));
            }
            Stage::Waiting(_) => {
                body.push(note(&t("agent-vertex-waiting")));
                body.push(row(cancel()));
            }
            Stage::Choose => {
                body.push(note(&t(if self.gemini {
                    "agent-gemini-choose"
                } else {
                    "agent-vertex-choose"
                })));
                self.push_projects(actions, &mut body);
                self.push_fields(&mut body, "new");
                body.push(note(&t(if self.gemini {
                    "agent-gemini-requirements"
                } else {
                    "agent-vertex-requirements"
                })));
                let (project, location, model) = (
                    self.project.clone(),
                    self.location.clone(),
                    self.model.clone(),
                );
                let add = Button::new("agent-models-vertex-add", &t("agent-vertex-add-provider"));
                let add = if self.gemini {
                    actions.bind_effect(
                        add,
                        effect(AgentModelsEffect::SET_GEMINI_PROVIDER)
                            .map(move |()| (None, project.with(|value| value.trim().to_string()))),
                    )
                } else {
                    actions.bind_effect(
                        add,
                        effect(AgentModelsEffect::SET_VERTEX_PROVIDER)
                            .map(move |()| fields_value(None, &project, &location, &model)),
                    )
                };
                body.push(row(Box::new(
                    Row::new()
                        .push(Box::new(add))
                        .push(provider_settings_ui::widgets::gap(
                            ui::tokens::SpacingStep::S2,
                        ))
                        .push(cancel()),
                )));
            }
        }
        if let Some(message) = &self.message {
            body.push(row(provider_settings_ui::widgets::error_caption(message)));
        }
        section(
            &t(if self.gemini {
                "agent-gemini-title"
            } else {
                "agent-vertex-title"
            }),
            rows(body),
        )
    }

    fn push_projects(&self, actions: &Actions<Settings>, body: &mut Vec<Item>) {
        if let Some(error) = &self.projects_error {
            body.push(note(&t_args(
                "agent-vertex-projects-unlisted",
                &[("error", error.as_str().into())],
            )));
            return;
        }
        if self.projects.is_empty() {
            body.push(note(&t("agent-vertex-projects-none")));
            return;
        }
        let mut buttons = Row::new();
        for (index, project) in self.projects.iter().take(SHOWN_PROJECTS).enumerate() {
            if index > 0 {
                buttons = buttons.push(provider_settings_ui::widgets::gap(
                    ui::tokens::SpacingStep::S2,
                ));
            }
            let label = if project.name.is_empty() || project.name == project.id {
                project.id.clone()
            } else {
                t_args(
                    "agent-vertex-project-named",
                    &[
                        ("name", project.name.as_str().into()),
                        ("id", project.id.as_str().into()),
                    ],
                )
            };
            buttons = buttons.push(Box::new(invoking(
                actions,
                Button::new(
                    &format!("agent-models-vertex-project-{}", project.id),
                    &label,
                ),
                AgentModelsEffect::ChooseVertexProject {
                    project: project.id.clone(),
                },
            )));
        }
        body.push(row(Box::new(buttons)));
    }

    fn push_fields(&self, body: &mut Vec<Item>, id: &str) {
        push_vertex_fields(
            body,
            id,
            self.gemini,
            (&self.project, &self.location, &self.model),
        );
    }
}

#[derive(Clone)]
pub(crate) struct VertexFields {
    pub(crate) project: LocalState<String>,
    pub(crate) location: LocalState<String>,
    pub(crate) model: LocalState<String>,
    pub(crate) gemini: bool,
}

impl VertexFields {
    pub(crate) fn of(profile: &ProviderProfile) -> Self {
        let target = vertex_target(&profile.endpoint);
        let gemini = target.is_none();
        let (project, location) = target.unwrap_or_else(|| {
            let project = profile
                .sign_in
                .as_ref()
                .and_then(|sign_in| sign_in.billing_project.clone())
                .unwrap_or_default();
            (project, String::new())
        });
        Self {
            gemini,
            project: LocalState::new(project),
            location: LocalState::new(location),
            model: LocalState::new(
                profile
                    .default_model
                    .as_deref()
                    .map(display_model)
                    .unwrap_or_default()
                    .to_string(),
            ),
        }
    }

    pub(crate) fn push(&self, actions: &Actions<Settings>, body: &mut Vec<Item>, profile_id: &str) {
        push_vertex_fields(
            body,
            profile_id,
            self.gemini,
            (&self.project, &self.location, &self.model),
        );
        let fields = self.clone();
        let provider = profile_id.to_string();
        let save = Button::new(
            &format!("agent-models-vertex-save-{profile_id}"),
            &t("agent-save-details"),
        );
        let save = if self.gemini {
            actions.bind_effect(
                save,
                effect(AgentModelsEffect::SET_GEMINI_PROVIDER).map(move |()| {
                    (
                        Some(provider.clone()),
                        fields.project.with(|value| value.trim().to_string()),
                    )
                }),
            )
        } else {
            actions.bind_effect(
                save,
                effect(AgentModelsEffect::SET_VERTEX_PROVIDER).map(move |()| {
                    fields_value(
                        Some(provider.clone()),
                        &fields.project,
                        &fields.location,
                        &fields.model,
                    )
                }),
            )
        };
        body.push(row(Box::new(save)));
    }
}

fn push_vertex_fields(
    body: &mut Vec<Item>,
    id: &str,
    gemini: bool,
    (project, location, model): (
        &LocalState<String>,
        &LocalState<String>,
        &LocalState<String>,
    ),
) {
    let fields = [
        (
            "project",
            "agent-vertex-project",
            "agent-vertex-project-hint",
            project,
        ),
        (
            "location",
            "agent-vertex-location",
            "agent-vertex-location-hint",
            location,
        ),
        (
            "model",
            "agent-placeholder-model",
            "agent-vertex-model-hint",
            model,
        ),
    ];
    for (name, label, placeholder, state) in fields.into_iter().take(if gemini { 1 } else { 3 }) {
        body.push(field_row(
            &t(label),
            Box::new(
                TextInput::from_state(state)
                    .with_id(&format!("agent-models-vertex-{name}-{id}"))
                    .with_accessible_name(t(label))
                    .with_placeholder(t(placeholder)),
            ),
        ));
    }
}

fn fields_value(
    provider: Option<String>,
    project: &LocalState<String>,
    location: &LocalState<String>,
    model: &LocalState<String>,
) -> (Option<String>, String, Option<String>, Option<String>) {
    let text = |state: &LocalState<String>| state.with(|value| value.trim().to_string());
    let optional = |value: String| (!value.is_empty()).then_some(value);
    (
        provider,
        text(project),
        optional(text(location)),
        optional(text(model)),
    )
}

fn display_model(id: &str) -> &str {
    id.strip_prefix("google/").unwrap_or(id)
}

pub(crate) fn apply(
    config: &mut ProviderConfig,
    profile_id: &str,
    existing: bool,
    (project, location, model): (&str, Option<&str>, Option<&str>),
) -> Result<(), String> {
    let project = project.trim();
    if !is_cloud_project_id(project) {
        return Err(t("agent-vertex-project-invalid"));
    }
    let location = location
        .map(str::trim)
        .filter(|location| !location.is_empty())
        .unwrap_or(VERTEX_DEFAULT_LOCATION);
    if !is_vertex_location(location) {
        return Err(t("agent-vertex-location-invalid"));
    }
    let model = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(VERTEX_DEFAULT_MODEL);
    if model.contains(char::is_whitespace) {
        return Err(t("agent-vertex-model-invalid"));
    }
    if !existing {
        let mut profile = ProviderProfile::google_vertex(project, location, model);
        profile.id = profile_id.to_string();
        config.upsert(profile);
        return Ok(());
    }
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| t("agent-provider-gone"))?;
    if !profile.signs_in_with_google() || vertex_target(&profile.endpoint).is_none() {
        return Err(t("agent-provider-gone"));
    }
    profile.endpoint = vertex_endpoint(project, location);
    profile.sign_in = Some(SignInCredential::google(project));
    let id = vertex_model_id(model);
    if !profile.models.iter().any(|candidate| candidate.id == id) {
        profile.models.push(ModelDescriptor {
            label: model.to_string(),
            ..ModelDescriptor::new(id.clone())
        });
    }
    profile.default_model = Some(id);
    Ok(())
}

pub(crate) fn apply_gemini(
    config: &mut ProviderConfig,
    profile_id: &str,
    existing: bool,
    project: &str,
) -> Result<(), String> {
    let project = project.trim();
    if !is_cloud_project_id(project) {
        return Err(t("agent-vertex-project-invalid"));
    }
    if !existing {
        let mut profile = ProviderProfile::google_gemini(project);
        profile.id = profile_id.to_string();
        config.upsert(profile);
        return Ok(());
    }
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| {
            profile.id == profile_id
                && profile.signs_in_with_google()
                && profile.endpoint.trim_end_matches('/') == presets::GEMINI_ENDPOINT
        })
        .ok_or_else(|| t("agent-provider-gone"))?;
    profile.sign_in = Some(SignInCredential::google(project));
    Ok(())
}

#[cfg(test)]
pub(crate) fn signed_in(custodian: &Custodian, profile_id: &str) -> bool {
    matches!(
        custodian.oauth_grant_state(&grant_id(profile_id)),
        Ok(account_custody::GrantState::Usable)
    )
}
