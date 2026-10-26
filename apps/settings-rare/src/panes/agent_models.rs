//! Agent Models — device-local model lifecycle and provider profiles.
//!
//! Tilo owns the user-facing lifecycle, while a small adapter speaks to the
//! privileged model manager that pins Hugging Face snapshots and supervises
//! vLLM. Settings never downloads model bytes or starts a server itself.
//! With host administration, profiles and credentials come from the backend.
//! Without it, legacy metadata remains editable offline, never as a claim of
//! backend synchronization. Keys stay in the backend vault. Every change is an
//! [`AgentModelsEffect`].

use agent_provider_config::management::CredentialStatus;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;

use agent_provider_config::local::hub::HubTokenState;
pub use agent_provider_config::local::snapshot::{
    AcceleratorSetup, AcceleratorSetupState, HardwareFit, HardwareRecommendation,
    InProcessLocalModel, InstalledLocalModel, LocalModelPackage, LocalModelPhase,
    LocalModelSnapshot,
};
use agent_provider_config::{EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile};
#[cfg(test)]
use agent_provider_config::{ModelRequestLimits, ProviderProtocol};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::{self, Effect};
use ui::time::{Duration, Instant};
use ui::widget::{Button, ProgressBar, Row, Widget};

use crate::effects::SettingsEffect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::panes::accounts::{BrowserOpener, PortalOpener};
use crate::panes::agent_sign_in::SignInSetup;
use crate::panes::agent_vertex::{self, Polled, VertexFields, VertexSetup};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, SettingsSection, StatusTone, note, row, rows, section, section_with,
    semantic_status_pill, thing_row, value_row,
};
use ui::lock::MutexRecover as _;

#[cfg(test)]
mod admin_tests;
mod hub;
mod sample;
mod sign_in_cards;
use provider_settings_ui::{ProviderEffect, ProviderPanel};
use sample::SampleManager;

crate::effects::effect_family!(AgentModelsEffect => answer);

crate::area::pane_area! {
    AgentModelsArea;
    fn poll(&self) {
        self.poll_admin();
    }
    // The window bar's search searches Hugging Face here.
    fn query(&self, category: crate::categories::Category) -> Option<&dyn crate::area::PaneQuery> {
        (category == crate::categories::Category::AgentModels)
            .then_some(self as &dyn crate::area::PaneQuery)
    }
    fn busy(&self) -> bool {
        self.admin_busy()
    }
    // The provider panel's vocabulary is shared with the portable provider
    // host; this area answers it here.
    fn answer_provider(
        &self,
        effect: &ProviderEffect,
        cx: &mut EffectCx<Settings, SettingsApp>,
    ) -> Option<ui::framework::EffectAnswer> {
        Some(AgentModelsArea::answer_provider(self, effect, cx))
    }
}

/// The window's [`AgentModelsArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&AgentModelsArea> {
    app.areas().find()
}

pub use provider_settings_ui::AgentProviders;

/// Local models on this device and the person's model providers.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AgentModelsEffect {
    /// Stage the accelerator drivers the recommended model needs.
    #[effect(job, label = "Set up accelerator")]
    SetUpAccelerator,
    /// Show or hide what downloading the recommended model involves.
    #[effect(view)]
    ReviewModelDownload { show: bool },
    /// Show or hide what downloading one in-process model involves.
    #[effect(view, row)]
    ReviewLocalModel {
        #[param("The model's id")]
        model: String,
        show: bool,
    },
    /// Show one signed in-process model in Agent Models, first, at its
    /// licence confirmation when it is not installed yet. Installs nothing.
    #[effect(view, label = "Show local model")]
    ShowLocalModel {
        #[param("Signed model id, for example u2net-subject")]
        model: String,
    },
    /// Download and install the recommended model or an in-process one,
    /// accepting its license.
    #[effect(job, label = "Install model")]
    InstallModel {
        #[param("The model's id")]
        model: String,
        #[param("The pinned revision")]
        revision: String,
    },
    /// Start serving an installed model.
    #[effect(job, row, idempotent, label = "Start model")]
    StartModel {
        #[param("The model's id")]
        model: String,
    },
    /// Stop serving a model.
    #[effect(job, row, idempotent, label = "Stop model")]
    StopModel {
        #[param("The model's id")]
        model: String,
    },
    /// Install a model again after it failed.
    #[effect(job, row, label = "Retry")]
    RetryModel {
        #[param("The model's id")]
        model: String,
        #[param("The pinned revision")]
        revision: String,
    },
    /// Remove an installed model and its files.
    #[effect(job, row, destructive, label = "Remove model")]
    RemoveModel {
        #[param("The model's id")]
        model: String,
    },
    /// Search Hugging Face for models to run on this device, most
    /// downloaded first. Answers with the results.
    #[effect(job, returns, kind = "read", label = "Search Hugging Face")]
    SearchHubModels {
        #[param("What to search for, for example qwen or llama")]
        query: String,
        #[param("`gguf` for llama.cpp, the default, or `safetensors` for vLLM")]
        format: Option<String>,
    },
    /// Look for another file family on Hugging Face: what the window bar's
    /// search holds is searched again.
    #[effect(view)]
    SetHubFormat {
        #[param("`gguf` for llama.cpp or `safetensors` for vLLM")]
        format: String,
    },
    /// Show a Hugging Face model's variants with their size, fit for this
    /// device and licence. Answers with them; downloads nothing.
    #[effect(job, returns, kind = "read", label = "Show Hugging Face model")]
    ShowHubModel {
        #[param("A model id such as owner/name, or its huggingface.co link")]
        model: String,
    },
    /// Show or hide what downloading one variant of the shown model involves.
    #[effect(view, row)]
    ReviewHubVariant {
        #[param("The variant, for example Q4_K_M")]
        variant: String,
        show: bool,
    },
    /// Download a variant of a Hugging Face model at the commit it was
    /// shown at, accepting its licence, and start it when a signed runtime
    /// here serves it.
    #[effect(job, row, label = "Download model")]
    InstallHubModel {
        #[param("The model id, owner/name")]
        repo: String,
        #[param("The commit it was shown at")]
        revision: String,
        #[param("The variant, for example Q4_K_M")]
        variant: String,
    },
    /// Ask the person to choose a GGUF model file on this device in the
    /// file chooser, and copy it into the model store.
    #[effect(job, label = "Add model file")]
    AddModelFile,
    /// Open a gated model's page on Hugging Face, where its terms are accepted.
    #[effect(view, label = "Open model page")]
    OpenHubModelPage {
        #[param("The model id, owner/name")]
        repo: String,
    },
    /// Keep a Hugging Face access token in this device's vault, for gated
    /// and private models.
    #[effect(job, label = "Save Hugging Face token")]
    SaveHubToken {
        #[param(secret, "The token; what the token field holds when omitted")]
        token: Option<String>,
    },
    /// Remove the Hugging Face token or sign-in from this device's vault.
    #[effect(job, destructive, label = "Remove Hugging Face token")]
    RemoveHubToken,
    /// Sign in with Hugging Face in the browser, for gated models; the
    /// sign-in is kept in this device's vault in place of a token.
    #[effect(attended, label = "Sign in with Hugging Face")]
    SignInWithHuggingFace,
    /// Sign in with Hugging Face by entering a code on its page, from any
    /// device: for a device without a browser.
    #[effect(attended, label = "Sign in with Hugging Face by code")]
    SignInWithHuggingFaceCode,
    /// Stop a Hugging Face sign-in.
    #[effect(view)]
    CancelHuggingFaceSignIn,
    /// Start adding Google Vertex AI, which signs in with Google instead of
    /// taking an API key. Shows its setup; signs in to nothing yet.
    #[effect(view, label = "Add Google Vertex AI")]
    AddGoogleVertex,
    /// Sign in with Google in the browser for Vertex AI: for the provider
    /// being added, or again for an existing one whose sign-in ended.
    #[effect(row, label = "Sign in with Google")]
    SignInWithGoogle {
        #[param("The provider's id; omit for the one being added")]
        provider: Option<String>,
    },
    /// Stop a sign-in, or the setup of a provider being added.
    #[effect(view)]
    CancelSignIn,
    /// Sign in to `ChatGPT`, GitHub Copilot or `OpenRouter`: for a provider
    /// to add, or again for an existing one. Finishes once the
    /// person has signed in and the provider is saved.
    #[effect(attended, job, row, label = "Sign in")]
    SignInToProvider {
        #[param("`chatgpt`, `github-copilot` or `openrouter`")]
        service: String,
        #[param("The provider's id; omit to add one")]
        provider: Option<String>,
    },
    /// Start adding the Gemini API signed in with Google and billed to one
    /// of the person's Cloud projects. Shows its setup; signs in to nothing
    /// yet.
    #[effect(view, label = "Add Gemini with Google")]
    AddGoogleGemini,
    /// Save the Gemini provider being added, once signed in with Google, or
    /// change the Cloud project an existing one bills.
    #[effect(job, row)]
    SetGeminiProvider {
        #[param("The provider's id; omit for the one being added")]
        provider: Option<String>,
        #[param("The Google Cloud project billed for requests")]
        project: String,
    },
    /// Put one of the person's listed Google Cloud projects in the setup.
    #[effect(view)]
    ChooseVertexProject {
        #[param("The Google Cloud project id")]
        project: String,
    },
    /// Save the Vertex AI provider being added, once signed in, or change
    /// an existing one's project, location and model.
    #[effect(job, row)]
    SetVertexProvider {
        #[param("The provider's id; omit for the one being added")]
        provider: Option<String>,
        #[param("The Google Cloud project billed for requests")]
        project: String,
        #[param("The Vertex AI location; `global` when omitted")]
        location: Option<String>,
        #[param("The model, for example gemini-4-argon; the default when omitted")]
        model: Option<String>,
    },
}

fn token(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// The effect `action` is for `model`.
fn model_effect(model: &InstalledLocalModel, action: ModelAction) -> AgentModelsEffect {
    let id = model.package.id.clone();
    match action {
        ModelAction::Start => AgentModelsEffect::StartModel { model: id },
        ModelAction::Stop => AgentModelsEffect::StopModel { model: id },
        ModelAction::Retry => AgentModelsEffect::RetryModel {
            model: id,
            revision: model.package.revision.clone(),
        },
        ModelAction::Remove => AgentModelsEffect::RemoveModel { model: id },
    }
}

/// Answer `effect`: the manager's work runs on a worker, then the models
/// are read again; provider changes write the person's settings.
#[allow(
    clippy::too_many_lines,
    reason = "one match arm per effect of the area, each answered where it is read"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: AgentModelsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let shared = crate::panes::agent_models::area(app)
        .ok_or_else(crate::area::missing)?
        .model();
    let mut model = shared.borrow_mut();
    model.changed = true;
    match effect {
        AgentModelsEffect::ReviewModelDownload { show } => {
            model.reviewing.set(show);
            Ok(Effected::done())
        }
        AgentModelsEffect::ReviewLocalModel { model: id, show } => {
            let installable = model
                .snapshot
                .in_process_model(&id)
                .is_some_and(InProcessLocalModel::installable);
            if show && !installable {
                return Err(t("agent-notice-inprocess-changed"));
            }
            model.reviewing_local.set(show.then_some(id));
            Ok(Effected::done())
        }
        AgentModelsEffect::ShowLocalModel { model: id } => {
            drop(model);
            // The pane switches at the next sync, as a sidebar pick does.
            app.request_pane(crate::categories::Category::AgentModels);
            let shared = crate::panes::agent_models::area(app)
                .ok_or_else(crate::area::missing)?
                .model();
            let mut model = shared.borrow_mut();
            model.reveal(&id)?;
            Ok(Effected::done())
        }
        effect @ (AgentModelsEffect::ReviewHubVariant { .. }
        | AgentModelsEffect::OpenHubModelPage { .. }
        | AgentModelsEffect::SearchHubModels { .. }
        | AgentModelsEffect::SetHubFormat { .. }
        | AgentModelsEffect::ShowHubModel { .. }
        | AgentModelsEffect::SaveHubToken { .. }
        | AgentModelsEffect::RemoveHubToken
        | AgentModelsEffect::SignInWithHuggingFace
        | AgentModelsEffect::SignInWithHuggingFaceCode
        | AgentModelsEffect::CancelHuggingFaceSignIn) => hub::answer(model, effect, cx),
        AgentModelsEffect::SetUpAccelerator
        | AgentModelsEffect::InstallModel { .. }
        | AgentModelsEffect::StartModel { .. }
        | AgentModelsEffect::StopModel { .. }
        | AgentModelsEffect::RetryModel { .. }
        | AgentModelsEffect::RemoveModel { .. }
        | AgentModelsEffect::InstallHubModel { .. }
        | AgentModelsEffect::AddModelFile => {
            let work = model.manager_work(&effect)?;
            let control = Arc::clone(&model.control);
            if matches!(effect, AgentModelsEffect::InstallModel { .. }) {
                model.reviewing.set(false);
                model.reviewing_local.set(None);
            }
            if matches!(effect, AgentModelsEffect::InstallHubModel { .. }) {
                model.hub.reviewing = None;
            }
            drop(model);
            Ok(crate::device_read::job(
                cx,
                // The manager is asked again on the job too: the answer
                // carries what the change left, read off the UI thread.
                move || {
                    let outcome = work.run(control.as_ref());
                    (outcome, control.snapshot())
                },
                |app: &mut SettingsApp,
                 (outcome, snapshot): (Result<String, String>, Snapshot),
                 _| {
                    let model = crate::panes::agent_models::area(app)
                        .ok_or_else(crate::area::missing)?
                        .model();
                    let mut model = model.borrow_mut();
                    model.snapshot_reads.supersede();
                    model.apply_snapshot(snapshot);
                    if let Err(error) = &outcome {
                        model.notice = Some(error.clone());
                    }
                    outcome.map(|_| Effected::done())
                },
            ))
        }
        AgentModelsEffect::AddGoogleVertex | AgentModelsEffect::AddGoogleGemini => {
            let gemini = matches!(effect, AgentModelsEffect::AddGoogleGemini);
            if model
                .vertex
                .as_ref()
                .is_none_or(|setup| setup.existing || setup.gemini != gemini)
            {
                model.cancel_vertex();
                model.vertex = Some(if gemini {
                    VertexSetup::adding_gemini()
                } else {
                    VertexSetup::adding()
                });
            }
            Ok(Effected::done())
        }
        AgentModelsEffect::SignInToProvider { service, provider } => {
            model.sign_in_to(&service, provider.as_deref(), Some(cx.reply()))?;
            Ok(Effected::Pending)
        }
        AgentModelsEffect::SignInWithGoogle { provider } => {
            model.sign_in_with_google(provider.as_deref())?;
            Ok(Effected::done())
        }
        AgentModelsEffect::CancelSignIn => {
            model.cancel_vertex();
            model.cancel_sign_in();
            Ok(Effected::done())
        }
        AgentModelsEffect::ChooseVertexProject { project } => {
            model
                .vertex
                .as_ref()
                .filter(|setup| setup.choosing())
                .ok_or_else(|| t("agent-vertex-sign-in-first"))?
                .choose_project(&project);
            Ok(Effected::done())
        }
        AgentModelsEffect::SetVertexProvider {
            provider,
            project,
            location,
            model: chosen,
        } => model.answer_vertex_change(
            &VertexChange::Vertex {
                provider,
                project,
                location,
                model: chosen,
            },
            cx.reply(),
        ),
        AgentModelsEffect::SetGeminiProvider { provider, project } => {
            model.answer_vertex_change(&VertexChange::Gemini { provider, project }, cx.reply())
        }
    }
}

/// A Google provider this area saves itself, because the shared provider
/// panel cannot sign in to Google: the Vertex AI and Gemini providers it sets
/// up, and the signed-in provider it forgets.
enum VertexChange {
    /// Add the Vertex AI provider being set up (`provider` `None`), or change
    /// an existing one.
    Vertex {
        provider: Option<String>,
        project: String,
        location: Option<String>,
        model: Option<String>,
    },
    /// Add the Gemini provider being set up (`provider` `None`), or bill
    /// another Cloud project with an existing one.
    Gemini {
        provider: Option<String>,
        project: String,
    },
    /// Forget a signed-in provider and the grant kept for it here.
    Remove { provider: String },
}

impl AgentModelsArea {
    /// Answer one of the shared provider panel's effects, in the pane that
    /// shows the panel: through the host's borrowed administration
    /// capability when it lends one, else in this person's offline metadata.
    fn answer_provider(
        &self,
        effect: &ProviderEffect,
        cx: &mut EffectCx<Settings, SettingsApp>,
    ) -> EffectAnswer {
        let mut model = self.model().borrow_mut();
        model.changed = true;
        if let ProviderEffect::InspectModelRequestLimits {
            provider,
            model: id,
        } = effect
        {
            return model.inspect_limits(provider, id).map(Effected::value);
        }
        // Removing a signed-in provider forgets its grant here, which only
        // this area can do; the panel's other removals carry no grant.
        if let ProviderEffect::RemoveProvider { provider } = effect
            && model.cx.provider_admin.is_available()
            && model
                .provider_config()
                .profiles
                .iter()
                .any(|profile| profile.id == *provider && profile.sign_in.is_some())
        {
            let change = VertexChange::Remove {
                provider: provider.clone(),
            };
            model.submit_vertex_change(&change, cx.reply())?;
            return Ok(Effected::Pending);
        }
        if model.cx.provider_admin.is_available() {
            model.submit_provider(effect.clone(), cx.reply())?;
            return Ok(Effected::Pending);
        }
        // Offline metadata remains usable, but API-key custody is never simulated.
        if needs_credential_custody(effect) {
            let error = t("agent-provider-offline");
            model.notice = Some(error.clone());
            return Err(error);
        }
        // Removing a signed-in provider forgets its grant here, which only
        // this area can do; the provider record itself is changed by the
        // same typed action any other provider edit uses.
        if let ProviderEffect::RemoveProvider { provider } = effect
            && model
                .provider_config()
                .profiles
                .iter()
                .any(|profile| profile.id == *provider && profile.sign_in.is_some())
        {
            model.forget_signed_in_grant(provider)?;
        }
        let listed = match effect {
            ProviderEffect::SetProviderModel {
                provider,
                model: id,
            } => model
                .listed_models
                .get(provider)
                .and_then(|listed| listed.as_ref().ok())
                .and_then(|models| models.iter().find(|model| model.id == *id))
                .cloned(),
            _ => None,
        };
        let Some(action) = effect.agent_action(listed.as_ref()) else {
            let result = model.change_providers(effect.clone());
            model.provider_notice(result.as_ref().map(|()| t("agent-provider-offline-saved")));
            return result.map(|()| Effected::done());
        };
        let action = ui::tilo_resource::Stamped {
            operation: action,
            now_ms: i64::try_from(ui::time::unix_now_ms()).unwrap_or(i64::MAX),
        };
        if model
            .cx
            .apply_section::<provider_settings_ui::AgentProviders>(&action)
            .is_err()
        {
            // A pane detached from a window (a screenshot, a pane test) has
            // no record to run the action on; it stores the metadata itself.
            let result = model.change_providers(effect.clone());
            model.provider_notice(result.as_ref().map(|()| t("agent-provider-offline-saved")));
            return result.map(|()| Effected::done());
        }
        model.provider_notice(Ok(t("agent-provider-offline-saved")));
        Ok(Effected::done())
    }
}

/// Whether `effect` hands a provider's key to the host's custody, which
/// offline metadata never stands in for.
fn needs_credential_custody(effect: &ProviderEffect) -> bool {
    matches!(
        effect,
        ProviderEffect::SaveProviderKey { .. }
            | ProviderEffect::RemoveProviderKey { .. }
            | ProviderEffect::RemoveLegacyProviderKey { .. }
            | ProviderEffect::MigrateProviderKey { .. }
            | ProviderEffect::ListProviderModels { .. }
            | ProviderEffect::RefreshProviders
    )
}

/// What the model manager reported, or why it could not.
type Snapshot = Result<LocalModelSnapshot, String>;

/// A model's reads of the device while its pane is open, made one at a
/// time off the UI thread and taken in at the sync their landing wakes.
struct Rereads<T> {
    read: crate::device_read::DeviceRead<T>,
    /// Asked for while a read ran: read once more when it lands, so the
    /// reading shows what changed meanwhile.
    again: bool,
    /// The read on its way began before a reading already taken in: it is
    /// dropped when it lands rather than shown over the newer one.
    stale: bool,
}

impl<T> Default for Rereads<T> {
    fn default() -> Self {
        Self {
            read: crate::device_read::DeviceRead::default(),
            again: false,
            stale: false,
        }
    }
}

impl<T: Send + 'static> Rereads<T> {
    /// Whether a read is on its way.
    fn running(&self) -> bool {
        !self.read.is_finished()
    }

    /// The reading that landed since last asked, unless a newer one was
    /// taken in meanwhile.
    fn landed(&mut self) -> Option<T> {
        if self.running() {
            return None;
        }
        let stale = std::mem::take(&mut self.stale);
        let reading = self.read.take()?;
        (!stale).then_some(reading)
    }

    /// Whether a read asked for while one ran is due now.
    fn due_again(&mut self) -> bool {
        let due = self.again && !self.running();
        if due {
            self.again = false;
        }
        due
    }

    /// Read with `read` off the UI thread, or once more after the read on
    /// its way. Returns a reading to take in now: one that had landed, or,
    /// on a host that runs no background work, the one read here.
    fn ask(&mut self, read: impl FnOnce() -> T + Clone + Send + 'static) -> Option<T> {
        if self.running() {
            self.again = true;
            return None;
        }
        // Finished stays finished until the next start: what landed is
        // taken here, so the start below is not refused for it.
        let landed = self.landed();
        // This read answers what was asked for while the last one ran.
        self.again = false;
        if self.read.start(read.clone()) {
            landed
        } else {
            Some(read())
        }
    }

    /// A reading made after the read on its way began was taken in.
    fn supersede(&mut self) {
        if self.running() {
            self.stale = true;
        }
    }
}

/// One call to the model manager, checked against what it reported.
enum ManagerWork {
    Accelerator,
    Install(LocalModelPackage),
    HubInstall {
        repo: String,
        revision: String,
        variant: String,
    },
    /// The file the person chooses, handed over open.
    ImportFile,
    Start(String),
    Stop(String),
    Remove(String),
}

impl ManagerWork {
    /// Carry the call out. Never on the UI thread: the manager may take
    /// seconds.
    fn run(&self, control: &dyn LocalModelControl) -> Result<String, String> {
        match self {
            Self::Accelerator => control
                .stage_accelerator()
                .map(|()| t("agent-notice-accel-ok"))
                .map_err(|error| t_args("agent-notice-accel-err", &[("error", error.into())])),
            Self::Install(package) => control
                .install(package)
                .map(|()| t("agent-notice-install-ok"))
                .map_err(|error| t_args("agent-notice-install-err", &[("error", error.into())])),
            Self::HubInstall {
                repo,
                revision,
                variant,
            } => control
                .hub_install(repo, revision, variant)
                .map(|()| t("agent-notice-install-ok"))
                .map_err(|error| t_args("agent-notice-install-err", &[("error", error.into())])),
            Self::ImportFile => match control.choose_model_file() {
                Ok(Some((name, file))) => control
                    .import_file(&name, file)
                    .map(|()| t("agent-notice-install-ok"))
                    .map_err(|error| {
                        t_args("agent-notice-install-err", &[("error", error.into())])
                    }),
                Ok(None) => Ok(String::new()),
                Err(error) => Err(t_args(
                    "agent-notice-install-err",
                    &[("error", error.into())],
                )),
            },
            Self::Start(id) => Self::named(ModelAction::Start, control.start(id)),
            Self::Stop(id) => Self::named(ModelAction::Stop, control.stop(id)),
            Self::Remove(id) => Self::named(ModelAction::Remove, control.remove(id)),
        }
    }

    fn named(action: ModelAction, result: Result<(), String>) -> Result<String, String> {
        result
            .map(|()| {
                t_args(
                    "agent-notice-action-ok",
                    &[("action", action_label(action).into())],
                )
            })
            .map_err(|error| {
                t_args(
                    "agent-notice-action-failed",
                    &[
                        ("action", action_label(action).into()),
                        ("error", error.into()),
                    ],
                )
            })
    }
}

/// What the agent models area keeps for the window: the model, asked of
/// the manager when first needed.
pub struct AgentModelsArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<AgentModelsModel>>,
    /// The manager's snapshot, asked off the UI thread after the first frame.
    /// Provider administration is separately polled as a portable future.
    read: crate::device_read::DeviceRead<AgentModelsRead>,
}

/// The model manager's device snapshot plus unavailable credential statuses
/// until the host administration capability answers.
pub(crate) struct AgentModelsRead {
    snapshot: Result<LocalModelSnapshot, String>,
    keys: HashMap<String, Result<CredentialStatus, String>>,
    /// What Hugging Face token or sign-in is stored on this device.
    hub_token: Result<HubTokenState, String>,
}

impl AgentModelsRead {
    /// Ask only the device manager here; provider reads require host authority.
    fn of(control: &dyn LocalModelControl, hosted: Vec<String>) -> Self {
        Self {
            snapshot: control.snapshot(),
            hub_token: control.hub_token(),
            keys: hosted
                .into_iter()
                .map(|id| (id, Err(t("agent-provider-offline"))))
                .collect(),
        }
    }
}

/// The ids of the hosted providers in `providers`, whose keys this
/// device's vault holds or not.
fn hosted_profiles(providers: &AgentProviders) -> Vec<String> {
    providers
        .config
        .profiles
        .iter()
        .filter(|profile| profile.location == EndpointLocation::Hosted)
        .map(|profile| profile.id.clone())
        .collect()
}

impl AgentModelsArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
            read: crate::device_read::DeviceRead::default(),
        }
    }

    /// The model manager: this device's, or a sample one for a screenshot.
    fn control(&self) -> Arc<dyn LocalModelControl> {
        if self.cx.reaches_devices() {
            Arc::new(ModelManagerCli)
        } else {
            Arc::new(SampleManager)
        }
    }

    /// The model, using the device manager's read or a screenshot stand-in.
    pub fn model(&self) -> &Shared<AgentModelsModel> {
        self.model.get_or_init(|| {
            let control = self.control();
            let model = match self.read.take() {
                Some(read) => AgentModelsModel::from_read(self.cx.clone(), control, read),
                None => AgentModelsModel::new(self.cx.clone(), control),
            };
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// Ask the device manager off the UI thread; provider reads are futures.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let control = self.control();
        let hosted = hosted_profiles(self.cx.follow::<AgentProviders>().get());
        self.read
            .start(move || AgentModelsRead::of(control.as_ref(), hosted))
    }

    /// Whether the read [`Self::start_read`] started has ended.
    pub(crate) fn read_finished(&self) -> bool {
        self.read.is_finished()
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    pub(crate) fn poll_admin(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_admin();
        }
    }

    pub(crate) fn admin_busy(&self) -> bool {
        self.model
            .get()
            .is_some_and(|model| model.borrow().admin_pending.is_some())
    }

    /// One row per installed model, per provider, and the recommendation.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        self.model().borrow_mut().poll_admin();
        let model = self.model().borrow();
        let offer = |effect: AgentModelsEffect| SettingsEffect::from(effect).bound();
        let mut rows: Vec<LiveRow> = model
            .snapshot
            .models
            .iter()
            .map(|installed| {
                model_actions(installed).into_iter().fold(
                    LiveRow::new(
                        format!("settings.agent-model:{}", installed.package.id),
                        "settings.agent-model",
                        installed.package.name.clone(),
                    ),
                    |row, action| row.operation(offer(model_effect(installed, action))),
                )
            })
            .collect();
        // In-process models run inside the apps that use them on any CPU, so
        // neither the hardware fit nor accelerator setup gates them.
        for local in &model.snapshot.in_process {
            let mut row = LiveRow::new(
                format!("settings.agent-model:{}", local.package.id),
                "settings.agent-model-in-process",
                local.package.name.clone(),
            );
            if local.installable() {
                row = row.operation(offer(AgentModelsEffect::InstallModel {
                    model: local.package.id.clone(),
                    revision: local.package.revision.clone(),
                }));
            }
            if local.removable() {
                row = row.operation(offer(AgentModelsEffect::RemoveModel {
                    model: local.package.id.clone(),
                }));
            }
            rows.push(row);
        }
        if let Some(package) = &model.snapshot.recommendation {
            rows.push(
                LiveRow::new(
                    format!("settings.agent-model-recommendation:{}", package.id),
                    "settings.agent-model-recommendation",
                    package.name.clone(),
                )
                .operation(offer(AgentModelsEffect::InstallModel {
                    model: package.id.clone(),
                    revision: package.revision.clone(),
                })),
            );
        }
        rows.extend(model.hub.rows());
        rows.extend(model.panel.rows().into_iter().map(|row| {
            let profile = model
                .panel
                .provider_config()
                .profiles
                .iter()
                .find(|profile| {
                    profile.sign_in.is_some()
                        && row.id() == format!("settings.agent-provider:{}", profile.id)
                });
            if let Some(profile) = profile {
                sign_in_cards::sign_in_provider_row(row, profile)
            } else {
                row
            }
        }));
        rows
    }
}

pub struct AgentModelsModel {
    cx: PaneCx,
    control: Arc<dyn LocalModelControl>,
    snapshot: LocalModelSnapshot,
    manager_available: bool,
    /// Whether the local provider profile follows what the manager runs.
    follow_local_profile: bool,
    providers: Followed<AgentProviders>,
    /// Whether the download of the recommended model is being reviewed.
    reviewing: LocalState<bool>,
    /// The in-process model whose download is being reviewed.
    reviewing_local: LocalState<Option<String>>,
    /// The in-process model a link asked to show; its card comes first.
    revealed: Option<String>,
    last_poll: Instant,
    panel: ProviderPanel<PaneCx>,
    /// Manager reads remain native and off the UI thread; providers use the shared panel.
    snapshot_reads: Rereads<Snapshot>,
    vertex: Option<VertexSetup>,
    vertex_fields: Mutex<HashMap<String, VertexFields>>,
    vertex_keys: HashMap<String, Result<bool, String>>,
    vertex_reads: Rereads<HashMap<String, Result<bool, String>>>,
    custodian: account_custody::Custodian,
    opener: Arc<dyn BrowserOpener>,
    /// Who a Hugging Face sign-in belongs to is asked here.
    hub_userinfo: String,
    /// A native sign-in edit waiting for the authoritative provider acknowledgement.
    vertex_commit: Option<(String, Option<ProviderProfile>)>,
    /// Hugging Face: what was found, the model shown, the token.
    hub: hub::HubBrowser,
    /// A sign-in to `ChatGPT`, Copilot or `OpenRouter` under way.
    sign_in: Option<SignInSetup>,
}

impl std::ops::Deref for AgentModelsModel {
    type Target = ProviderPanel<PaneCx>;
    fn deref(&self) -> &Self::Target {
        &self.panel
    }
}
impl std::ops::DerefMut for AgentModelsModel {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.panel
    }
}
impl AgentModelsModel {
    /// The model over `control`, starting the host's asynchronous provider read.
    fn new(cx: PaneCx, control: Arc<dyn LocalModelControl>) -> Self {
        let hosted = hosted_profiles(cx.follow::<AgentProviders>().get());
        let read = AgentModelsRead::of(control.as_ref(), hosted);
        Self::from_read(cx, control, read)
    }

    /// Build from the device snapshot. No legacy local settings are treated
    /// as authoritative when this mount has a provider administration grant.
    fn from_read(cx: PaneCx, control: Arc<dyn LocalModelControl>, read: AgentModelsRead) -> Self {
        let (snapshot, manager_available, notice) = match read.snapshot {
            Ok(snapshot) => (snapshot, true, None),
            Err(error) => (
                fallback_snapshot(),
                false,
                Some(t_args(
                    "agent-notice-service-down",
                    &[("error", error.into())],
                )),
            ),
        };
        let panel = ProviderPanel::new(
            cx.clone(),
            cx.provider_admin.clone(),
            cx.follow::<AgentProviders>().get().config.clone(),
        );
        let mut model = Self {
            providers: cx.follow::<AgentProviders>(),
            cx,
            control,
            snapshot,
            manager_available,
            follow_local_profile: true,
            reviewing: LocalState::new(false),
            reviewing_local: LocalState::new(None),
            revealed: None,
            last_poll: ui::time::now(),
            panel,
            snapshot_reads: Rereads::default(),
            vertex: None,
            vertex_fields: Mutex::new(HashMap::new()),
            vertex_keys: HashMap::new(),
            vertex_reads: Rereads::default(),
            custodian: account_custody::Custodian::live(),
            opener: Arc::new(PortalOpener),
            hub_userinfo: account_custody::hugging_face::USERINFO_ENDPOINT.into(),
            vertex_commit: None,
            hub: hub::HubBrowser::new(read.hub_token),
            sign_in: None,
        };
        model.notice = notice;
        model.sync_local_profile();
        model.keys = read.keys;
        model.read_keys();
        model.read_vertex_keys();
        model
    }

    /// Read native sign-in status off the UI thread, separately from API-key custody.
    fn read_vertex_keys(&mut self) {
        let ids: Vec<_> = self
            .panel
            .provider_config()
            .profiles
            .iter()
            .filter(|profile| profile.sign_in.is_some())
            .map(|profile| profile.id.clone())
            .collect();
        let custodian = self.custodian.clone();
        if let Some(keys) = self.vertex_reads.ask(move || {
            ids.into_iter()
                .map(|id| {
                    let result = custodian
                        .oauth_grant_state(&agent_vertex::grant_id(&id))
                        .map(|state| state == account_custody::GrantState::Usable)
                        .map_err(|error| error.to_string());
                    (id, result)
                })
                .collect()
        }) {
            self.vertex_keys = keys;
            self.changed = true;
        }
    }

    fn take_reads(&mut self) {
        if let Some(snapshot) = self.snapshot_reads.landed() {
            self.apply_snapshot(snapshot);
        }
        if self.snapshot_reads.due_again() {
            self.refresh();
        }
        if let Some(keys) = self.vertex_reads.landed() {
            self.vertex_keys = keys;
            self.changed = true;
        }
        if self.vertex_reads.due_again() {
            self.read_vertex_keys();
        }
    }

    /// Open Google's consent page for Vertex AI: for the provider being
    /// added when `provider` is `None`, else again for that provider.
    fn sign_in_with_google(&mut self, provider: Option<&str>) -> Result<(), String> {
        let current = self.vertex.take();
        let reuse = |setup: &VertexSetup| match provider {
            None => !setup.existing,
            Some(id) => setup.profile_id == id,
        };
        let mut setup = match current {
            Some(setup) if reuse(&setup) => setup,
            other => {
                self.vertex = other;
                self.cancel_vertex();
                match provider {
                    None => VertexSetup::adding(),
                    Some(id) => VertexSetup::again(
                        self.panel
                            .provider_config()
                            .profiles
                            .iter()
                            .find(|profile| profile.id == id && profile.signs_in_with_google())
                            .ok_or_else(|| t("agent-provider-gone"))?,
                    ),
                }
            }
        };
        setup.begin(&self.custodian, &self.opener);
        self.vertex = Some(setup);
        self.changed = true;
        Ok(())
    }

    /// Stop the Vertex AI sign-in or setup. A provider that was being added
    /// leaves no grant behind.
    fn cancel_vertex(&mut self) {
        if let Some(mut setup) = self.vertex.take() {
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

    fn vertex_fields(&self, profile: &ProviderProfile) -> VertexFields {
        self.vertex_fields
            .lock_recover()
            .entry(profile.id.clone())
            .or_insert_with(|| VertexFields::of(profile))
            .clone()
    }

    /// Ask the manager again off the UI thread; its answer is taken in at
    /// the sync its landing wakes ([`Self::take_reads`]).
    fn refresh(&mut self) {
        let control = Arc::clone(&self.control);
        if let Some(snapshot) = self.snapshot_reads.ask(move || control.snapshot()) {
            self.apply_snapshot(snapshot);
        }
    }

    /// Take in what the manager reported, or why it could not.
    fn apply_snapshot(&mut self, snapshot: Snapshot) {
        match snapshot {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.manager_available = true;
                self.notice = None;
                self.sync_local_profile();
            }
            Err(error) => {
                self.manager_available = false;
                self.notice = Some(t_args(
                    "agent-notice-refresh-err",
                    &[("error", error.into())],
                ));
            }
        }
        self.last_poll = ui::time::now();
        self.changed = true;
    }

    /// Keep the local provider profile on what the manager serves.
    fn sync_local_profile(&mut self) {
        if self.cx.provider_admin.is_available() || !self.follow_local_profile {
            return;
        }
        let config = &self.providers.get().config;
        let existing = config
            .profiles
            .iter()
            .find(|profile| profile.is_local_vllm_candidate())
            .cloned();
        let Some(profile) = synced_local_profile(existing.clone(), &self.snapshot.models) else {
            return;
        };
        if existing.as_ref() != Some(&profile) {
            let _ = self.providers.settings().update(|providers| {
                providers.config.upsert(profile);
            });
            self.providers.refresh();
            self.panel.offline_config = self.providers.get().config.clone();
        }
    }

    /// Show one in-process model's card first, opened at its licence
    /// confirmation when it is not installed yet. Nothing is downloaded
    /// until the person accepts there.
    fn reveal(&mut self, id: &str) -> Result<(), String> {
        self.changed = true;
        let Some(model) = self.snapshot.in_process_model(id) else {
            let unknown = t_args(
                "agent-notice-unknown-model",
                &[("id", id.to_owned().into())],
            );
            self.notice = Some(unknown.clone());
            return Err(unknown);
        };
        if model.installable() {
            self.reviewing_local.set(Some(model.package.id.clone()));
        }
        self.revealed = Some(model.package.id.clone());
        Ok(())
    }

    fn is_busy(&self) -> bool {
        self.snapshot
            .models
            .iter()
            .any(|model| model.phase.is_transient())
            || self.snapshot.in_process.iter().any(|model| {
                model
                    .phase
                    .as_ref()
                    .is_some_and(LocalModelPhase::is_transient)
            })
            || self
                .snapshot
                .accelerator_setup
                .as_ref()
                .is_some_and(|setup| setup.status.is_transient())
    }

    /// The manager call `effect` asks for, revalidated against the models
    /// the manager last reported.
    fn manager_work(&self, effect: &AgentModelsEffect) -> Result<ManagerWork, String> {
        let installed = |id: &str| {
            self.snapshot
                .models
                .iter()
                .find(|model| model.package.id == id)
                .ok_or_else(|| t("agent-model-gone"))
        };
        let allowed = |model: &InstalledLocalModel, action: ModelAction| {
            if model_actions(model).contains(&action) {
                Ok(())
            } else {
                Err(t("agent-model-busy"))
            }
        };
        match effect {
            AgentModelsEffect::SetUpAccelerator => Ok(ManagerWork::Accelerator),
            AgentModelsEffect::InstallModel { model, revision } => {
                if let Some(local) = self.snapshot.in_process_model(model) {
                    // The action's label is the licence acceptance; it holds
                    // only for the revision it was offered with.
                    return if &local.package.revision == revision && local.installable() {
                        Ok(ManagerWork::Install(local.package.clone()))
                    } else {
                        Err(t("agent-notice-inprocess-changed"))
                    };
                }
                self.snapshot
                    .recommendation
                    .clone()
                    .filter(|package| &package.id == model && &package.revision == revision)
                    .map(ManagerWork::Install)
                    .ok_or_else(|| t("agent-model-gone"))
            }
            AgentModelsEffect::InstallHubModel {
                repo,
                revision,
                variant,
            } => {
                // The download is of what was shown and reviewed: the same
                // commit, a variant that fits.
                self.hub
                    .offered(repo, revision, variant)
                    .filter(|offered| offered.fit.fit != HardwareFit::Unsupported)
                    .ok_or_else(|| t("agent-hub-changed"))?;
                Ok(ManagerWork::HubInstall {
                    repo: repo.clone(),
                    revision: revision.clone(),
                    variant: variant.clone(),
                })
            }
            AgentModelsEffect::AddModelFile => Ok(ManagerWork::ImportFile),
            AgentModelsEffect::StartModel { model } => {
                let model = installed(model)?;
                allowed(model, ModelAction::Start)?;
                Ok(ManagerWork::Start(model.package.id.clone()))
            }
            AgentModelsEffect::StopModel { model } => {
                let model = installed(model)?;
                allowed(model, ModelAction::Stop)?;
                Ok(ManagerWork::Stop(model.package.id.clone()))
            }
            AgentModelsEffect::RetryModel { model, revision } => {
                let model = installed(model)?;
                allowed(model, ModelAction::Retry)?;
                if &model.package.revision != revision {
                    return Err(t("agent-model-gone"));
                }
                Ok(ManagerWork::Install(model.package.clone()))
            }
            AgentModelsEffect::RemoveModel { model } => {
                if let Some(local) = self.snapshot.in_process_model(model) {
                    return if local.removable() {
                        Ok(ManagerWork::Remove(local.package.id.clone()))
                    } else {
                        Err(t("agent-notice-inprocess-changed"))
                    };
                }
                let model = installed(model)?;
                allowed(model, ModelAction::Remove)?;
                Ok(ManagerWork::Remove(model.package.id.clone()))
            }
            _ => Err("not a model manager effect".into()),
        }
    }

    /// Forget a signed-in provider locally. API-key providers are removed through
    /// the shared administration capability, never through a UI-thread socket.
    fn remove_vertex_provider(
        &mut self,
        config: &mut ProviderConfig,
        provider: &str,
    ) -> Result<(), String> {
        let profile = config
            .profiles
            .iter()
            .find(|profile| profile.id == provider)
            .ok_or_else(|| t("agent-provider-gone"))?;
        if profile.sign_in.is_none() {
            return Err(t("agent-provider-offline"));
        }
        let _ = config.remove(provider);
        self.forget_signed_in_grant(provider)
    }

    /// Forget the grant kept on this device for signed-in `provider`.
    fn forget_signed_in_grant(&mut self, provider: &str) -> Result<(), String> {
        // Forgotten here only: revoking at Google would end every grant of
        // Tilo's client for this person, Mail's included.
        if self
            .vertex
            .as_ref()
            .is_some_and(|setup| setup.profile_id == provider)
        {
            self.cancel_vertex();
        }
        self.custodian
            .forget_oauth(&agent_vertex::grant_id(provider))
            .map_err(|error| {
                t_args(
                    "agent-sign-in-forget-err",
                    &[("error", error.to_string().into())],
                )
            })?;
        self.vertex_fields.lock_recover().remove(provider);
        Ok(())
    }

    /// Add the Vertex AI provider being set up (`provider` `None`), once
    /// signed in, or change an existing one; the provider's id.
    fn set_vertex_provider(
        &self,
        config: &mut ProviderConfig,
        provider: Option<String>,
        fields: (&str, Option<&str>, Option<&str>),
    ) -> Result<String, String> {
        let (id, existing) = self.google_provider(provider, false)?;
        agent_vertex::apply(config, &id, existing, fields)?;
        Ok(id)
    }

    /// Add the Gemini API provider being set up (`provider` `None`), once
    /// signed in, or bill another project with an existing one.
    fn set_gemini_provider(
        &self,
        config: &mut ProviderConfig,
        provider: Option<String>,
        project: &str,
    ) -> Result<String, String> {
        let (id, existing) = self.google_provider(provider, true)?;
        agent_vertex::apply_gemini(config, &id, existing, project)?;
        Ok(id)
    }

    /// The Google provider an edit is for and whether it exists: the named
    /// one, or the one being added, which must be signed in already.
    fn google_provider(
        &self,
        provider: Option<String>,
        gemini: bool,
    ) -> Result<(String, bool), String> {
        let (id, existing) = match provider {
            Some(id) => (id, true),
            None => (
                self.vertex
                    .as_ref()
                    .filter(|setup| !setup.existing && setup.gemini == gemini)
                    .map(|setup| setup.profile_id.clone())
                    .ok_or_else(|| t("agent-vertex-sign-in-first"))?,
                false,
            ),
        };
        if !existing && !agent_vertex::signed_in(&self.custodian, &id) {
            return Err(t("agent-vertex-sign-in-first"));
        }
        Ok((id, existing))
    }

    /// Close the setup of the Vertex AI provider `id` once it is saved, and
    /// read its fields and sign-in again.
    fn vertex_saved(&mut self, id: &str) {
        if self
            .vertex
            .as_ref()
            .is_some_and(|setup| setup.profile_id == id)
        {
            self.vertex = None;
        }
        if self
            .sign_in
            .as_ref()
            .is_some_and(|setup| setup.profile_id == id)
        {
            self.sign_in = None;
        }
        self.vertex_fields.lock_recover().remove(id);
        self.read_keys();
        self.read_vertex_keys();
    }

    /// Keep native OAuth setup on the same borrowed administration capability as the panel.
    fn submit_vertex_change(
        &mut self,
        change: &VertexChange,
        reply: ui::framework::EffectReply,
    ) -> Result<(), String> {
        if self.panel.admin_pending.is_some() {
            return Err(t("agent-provider-busy"));
        }
        let snapshot = self
            .panel
            .admin_snapshot
            .as_ref()
            .ok_or_else(|| t("agent-provider-refresh-required"))?;
        let mut config = snapshot.config.clone();
        let id = match change {
            VertexChange::Vertex {
                provider,
                project,
                location,
                model,
            } => self.set_vertex_provider(
                &mut config,
                provider.clone(),
                (project, location.as_deref(), model.as_deref()),
            )?,
            VertexChange::Gemini { provider, project } => {
                self.set_gemini_provider(&mut config, provider.clone(), project)?
            }
            // The host keeps the grant of a signed-in provider it removes.
            VertexChange::Remove { provider } => {
                config
                    .remove(provider)
                    .ok_or_else(|| t("agent-provider-gone"))?;
                provider.clone()
            }
        };
        let config = config.sanitised();
        let expected = config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned();
        self.panel.submit_config(config, reply)?;
        self.vertex_commit = Some((id, expected));
        self.changed = true;
        Ok(())
    }

    /// Save a Google provider: through the host's borrowed administration
    /// capability when it lends one, else in this person's offline metadata.
    fn answer_vertex_change(
        &mut self,
        change: &VertexChange,
        reply: ui::framework::EffectReply,
    ) -> EffectAnswer {
        if self.cx.provider_admin.is_available() {
            self.submit_vertex_change(change, reply)?;
            return Ok(Effected::Pending);
        }
        let result = self.change_vertex(change);
        self.provider_notice(result.as_ref().map(|()| t("agent-provider-offline-saved")));
        result.map(|()| Effected::done())
    }

    /// Save a Google provider in this person's offline metadata, where the
    /// grant kept for it here is this device's to forget.
    fn change_vertex(&mut self, change: &VertexChange) -> Result<(), String> {
        self.panel.offline_config = self.providers.get().config.clone();
        let mut config = self.providers.get().config.clone();
        let saved = match change {
            VertexChange::Vertex {
                provider,
                project,
                location,
                model,
            } => self.set_vertex_provider(
                &mut config,
                provider.clone(),
                (project, location.as_deref(), model.as_deref()),
            )?,
            VertexChange::Gemini { provider, project } => {
                self.set_gemini_provider(&mut config, provider.clone(), project)?
            }
            VertexChange::Remove { provider } => {
                self.remove_vertex_provider(&mut config, provider)?;
                provider.clone()
            }
        };
        self.store_providers(config.sanitised())?;
        self.vertex_saved(&saved);
        Ok(())
    }

    fn change_providers(&mut self, effect: ProviderEffect) -> Result<(), String> {
        // Offline metadata remains usable, but API-key custody is never simulated.
        self.panel.offline_config = self.providers.get().config.clone();
        let limits_ack = self.remember_limits(&effect);
        let config = match effect {
            ProviderEffect::RemoveProvider { provider }
                if self
                    .providers
                    .get()
                    .config
                    .profiles
                    .iter()
                    .any(|profile| profile.id == provider && profile.sign_in.is_some()) =>
            {
                let mut config = self.providers.get().config.clone();
                self.remove_vertex_provider(&mut config, &provider)?;
                config.sanitised()
            }
            effect => self.config_after(effect)?,
        };
        self.store_providers(config)?;
        if let Some(acknowledgement) = limits_ack {
            self.acknowledge_limits(&acknowledgement);
        }
        Ok(())
    }

    /// Keep `config` as this person's providers, and show what was stored.
    fn store_providers(&mut self, config: ProviderConfig) -> Result<(), String> {
        self.providers
            .settings()
            .update(|providers| providers.config = config)
            .map_err(|error| error.to_string())?;
        self.providers.refresh();
        self.panel.offline_config = self.providers.get().config.clone();
        self.changed = true;
        Ok(())
    }

    fn accelerator_card(&self) -> Option<SettingsSection> {
        let setup = self.snapshot.accelerator_setup.as_ref()?;
        let match_label = t("agent-accel-match");
        let mut body = vec![value_row(&match_label, &setup.boot_pack_id)];
        let (status, tone) = match &setup.status {
            AcceleratorSetupState::Available => {
                body.push(note(&t("agent-accel-available")));
                body.push(row(Box::new(Row::new().push(Box::new(
                    self.cx.bind_invocation(
                        Button::new("agent-models-accelerator-setup", &t("agent-accel-setup")),
                        AgentModelsEffect::SetUpAccelerator,
                    ),
                )))));
                (t("agent-accel-ready"), StatusTone::Neutral)
            }
            AcceleratorSetupState::Queued => {
                body.push(row(Box::new(
                    ProgressBar::indeterminate().with_label(t("agent-accel-preparing")),
                )));
                body.push(note(&t("agent-accel-queued-caption")));
                (t("agent-accel-queued"), StatusTone::Neutral)
            }
            AcceleratorSetupState::Staging => {
                body.push(row(Box::new(
                    ProgressBar::indeterminate().with_label(t("agent-accel-staging-label")),
                )));
                body.push(note(&t("agent-accel-staging-caption")));
                (t("agent-accel-staging"), StatusTone::Warning)
            }
            AcceleratorSetupState::RestartRequired => {
                body.push(note(&t("agent-accel-restart-caption")));
                (t("agent-accel-restart"), StatusTone::Warning)
            }
            AcceleratorSetupState::Active => {
                body.push(note(&t("agent-accel-active-caption")));
                (t("agent-accel-active"), StatusTone::Success)
            }
            AcceleratorSetupState::Error { message } => {
                body.push(note(message));
                body.push(note(&t("agent-accel-error-retry")));
                body.push(row(Box::new(Row::new().push(Box::new(
                    self.cx.bind_invocation(
                        Button::new("agent-models-accelerator-retry", &t("agent-accel-retry")),
                        AgentModelsEffect::SetUpAccelerator,
                    ),
                )))));
                (t("agent-status-attention"), StatusTone::Warning)
            }
        };
        Some(section_with(
            &t("agent-accel-title"),
            semantic_status_pill(&status, tone),
            rows(body),
        ))
    }

    fn push_recommendation_confirm(&self, body: &mut Vec<Item>, package: &LocalModelPackage) {
        self.push_confirm(
            body,
            package,
            (
                "agent-models-confirm-install",
                "agent-models-cancel-install",
            ),
            AgentModelsEffect::ReviewModelDownload { show: false },
        );
    }

    /// The download, storage, source, revision and licence a person accepts,
    /// with the accept and cancel controls (widget ids `controls`); cancel
    /// invokes `cancel`.
    fn push_confirm(
        &self,
        body: &mut Vec<Item>,
        package: &LocalModelPackage,
        controls: (&str, &str),
        cancel_effect: AgentModelsEffect,
    ) {
        let download = t("agent-download");
        let download_size = byte_size(package.download_bytes);
        let after = t("agent-storage-after");
        let after_size = byte_size(package.installed_bytes);
        let source = t("agent-source");
        let revision = t("agent-revision");
        let license = t("agent-license");
        let accept = t_args(
            "agent-license-accept",
            &[("url", package.license_url.as_str().into())],
        );
        let accept_btn = t("agent-accept-download");
        let cancel = t("agent-cancel");
        body.extend([
            value_row(&download, &download_size),
            value_row(&after, &after_size),
            value_row(&source, &package.source),
            value_row(&revision, &package.revision),
            value_row(&license, &package.license),
            note(&accept),
            row(Box::new(
                Row::new()
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(controls.0, &accept_btn),
                        AgentModelsEffect::InstallModel {
                            model: package.id.clone(),
                            revision: package.revision.clone(),
                        },
                    )))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(controls.1, &cancel),
                        cancel_effect,
                    ))),
            )),
        ]);
    }

    fn recommendation_card(&self) -> SettingsSection {
        let hardware = &self.snapshot.hardware;
        let fit_label = fit_label(hardware.fit);
        let mem = gib(i64::from(hardware.memory_gib));
        let hw_mem = t("agent-hw-memory");
        let hw_accel = t("agent-hw-accel");
        // How well this device fits is the group's badge, not a row too.
        let mut body = vec![
            value_row(&hw_mem, &mem),
            value_row(&hw_accel, &hardware.accelerator),
        ];
        if let Some(vram) = hardware.accelerator_memory_gib {
            let label = t("agent-hw-accel-mem");
            let value = gib(i64::from(vram));
            body.push(value_row(&label, &value));
        }
        if let Some(storage) = hardware.free_storage_gib {
            let label = t("agent-hw-storage");
            let value = gib(i64::from(storage));
            body.push(value_row(&label, &value));
        }
        body.push(note(&hardware.reason));
        let Some(package) = &self.snapshot.recommendation else {
            let rec_model = t("agent-rec-model");
            let waiting = t("agent-rec-waiting");
            body.push(value_row(&rec_model, &waiting));
            body.push(note(&t("agent-rec-waiting-caption")));
            let unavailable = t("agent-setup-unavailable");
            body.push(row(Box::new(Row::new().push(Box::new(
                Button::new("agent-models-setup-unavailable", &unavailable).with_enabled(false),
            )))));
            return section_with(
                &t("agent-rec-title"),
                semantic_status_pill(&t("agent-service-unavailable"), StatusTone::Warning),
                rows(body),
            );
        };
        if self.reviewing.with(|reviewing| *reviewing) {
            body.extend([
                value_row(&t("agent-rec-model"), &package.name),
                note(&package.description),
            ]);
            self.push_recommendation_confirm(&mut body, package);
        } else {
            // The model this device is suited for, and setting it up, on one
            // row.
            let setup = t("agent-setup-device");
            body.push(row(Box::new(thing_row(
                "agent-models.recommended",
                &package.name,
                std::slice::from_ref(&package.description),
                Some(Box::new(
                    self.cx.bind_invocation(
                        Button::new("agent-models-setup-local", &setup)
                            .role(ui::widget::ButtonRole::Primary)
                            .with_enabled(
                                self.manager_available
                                    && hardware.fit != HardwareFit::Unsupported
                                    && self
                                        .snapshot
                                        .accelerator_setup
                                        .as_ref()
                                        .is_none_or(|setup| !setup.status.blocks_model_setup()),
                            ),
                        AgentModelsEffect::ReviewModelDownload { show: true },
                    ),
                )),
            ))));
        }
        section_with(
            &t("agent-rec-title"),
            semantic_status_pill(&fit_label, fit_tone(hardware.fit)),
            rows(body),
        )
    }

    /// One in-process model: what it does, then setup with the same licence
    /// confirmation as the recommendation, progress, or ready with Remove.
    /// Never Start or Stop: it runs inside the apps that use it.
    #[allow(
        clippy::too_many_lines,
        reason = "every install phase of the card stays beside its controls"
    )]
    fn in_process_card(&self, model: &InProcessLocalModel) -> SettingsSection {
        let package = &model.package;
        let widget_token = token(&package.id);
        let mut body = vec![
            value_row(&t("agent-inprocess-model"), &package.name),
            note(&package.description),
            note(&t("agent-inprocess-caption")),
        ];
        let confirming = self
            .reviewing_local
            .with(|reviewing| reviewing.as_deref() == Some(package.id.as_str()));
        if confirming && model.installable() {
            let confirm_id = format!("agent-models-inprocess-confirm-{widget_token}");
            let cancel_id = format!("agent-models-inprocess-cancel-{widget_token}");
            self.push_confirm(
                &mut body,
                package,
                (&confirm_id, &cancel_id),
                AgentModelsEffect::ReviewLocalModel {
                    model: package.id.clone(),
                    show: false,
                },
            );
        } else {
            match &model.phase {
                None => {
                    body.push(value_row(
                        &t("agent-download"),
                        &byte_size(package.download_bytes),
                    ));
                }
                Some(
                    phase @ LocalModelPhase::Downloading {
                        downloaded_bytes,
                        total_bytes,
                    },
                ) => {
                    let progress = t_args(
                        "agent-progress-downloading",
                        &[("name", package.name.as_str().into())],
                    );
                    body.push(row(Box::new(
                        ProgressBar::new(phase_progress(phase).unwrap_or(0.0)).with_label(progress),
                    )));
                    let done = byte_size(*downloaded_bytes);
                    let total = byte_size(*total_bytes);
                    body.push(note(&t_args(
                        "agent-download-progress",
                        &[("done", done.as_str().into()), ("total", total.into())],
                    )));
                }
                Some(LocalModelPhase::Installing { message, .. }) => {
                    let progress = t_args(
                        "agent-progress-installing",
                        &[("name", package.name.as_str().into())],
                    );
                    body.push(row(Box::new(
                        ProgressBar::indeterminate().with_label(progress),
                    )));
                    body.push(note(message));
                }
                Some(LocalModelPhase::Ready { .. }) => {
                    body.push(value_row(
                        &t("agent-model-storage"),
                        &byte_size(package.installed_bytes),
                    ));
                    body.push(value_row(&t("agent-license"), &package.license));
                    body.push(note(&t("agent-inprocess-ready-caption")));
                }
                Some(LocalModelPhase::Error { message }) => {
                    body.push(note(message));
                    body.push(note(&t("agent-inprocess-error-caption")));
                }
            }
            let mut buttons = Row::new();
            let installable = model.installable();
            let removable = model.removable();
            if installable {
                buttons = buttons.push(Box::new(
                    self.cx.bind_invocation(
                        Button::new(
                            &format!("agent-models-inprocess-setup-{widget_token}"),
                            &t("agent-setup-device"),
                        )
                        .with_enabled(self.manager_available),
                        AgentModelsEffect::ReviewLocalModel {
                            model: package.id.clone(),
                            show: true,
                        },
                    ),
                ));
            }
            if removable {
                buttons = buttons.push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("agent-models-inprocess-remove-{widget_token}"),
                        &action_label(ModelAction::Remove),
                    ),
                    AgentModelsEffect::RemoveModel {
                        model: package.id.clone(),
                    },
                )));
            }
            if installable || removable {
                body.push(row(Box::new(buttons)));
            }
        }
        let (status, tone) = match &model.phase {
            None => (t("agent-status-not-installed"), StatusTone::Neutral),
            Some(LocalModelPhase::Ready { .. }) => (t("agent-status-ready"), StatusTone::Success),
            Some(phase) => (phase_status(phase), phase_tone(phase)),
        };
        section_with(
            &capability_title(package),
            semantic_status_pill(&status, tone),
            rows(body),
        )
    }

    fn model_card(&self, model: &InstalledLocalModel) -> SettingsSection {
        let status = phase_status(&model.phase);
        let storage = byte_size(model.package.installed_bytes);
        let status_label = t("agent-model-status");
        let storage_label = t("agent-model-storage");
        let license_label = t("agent-license");
        let mut body = vec![
            value_row(&status_label, &status),
            value_row(&storage_label, &storage),
            value_row(&license_label, &model.package.license),
        ];
        match &model.phase {
            LocalModelPhase::Downloading {
                downloaded_bytes,
                total_bytes,
            } => {
                let progress = t_args(
                    "agent-progress-downloading",
                    &[("name", model.package.name.as_str().into())],
                );
                body.push(row(Box::new(
                    ProgressBar::new(phase_progress(&model.phase).unwrap_or(0.0))
                        .with_label(progress),
                )));
                let done = byte_size(*downloaded_bytes);
                let total = byte_size(*total_bytes);
                body.push(note(&t_args(
                    "agent-download-progress",
                    &[("done", done.as_str().into()), ("total", total.into())],
                )));
            }
            LocalModelPhase::Installing { percent, message } => {
                let progress = t_args(
                    "agent-progress-installing",
                    &[("name", model.package.name.as_str().into())],
                );
                body.push(row(Box::new(match percent {
                    Some(_) => ProgressBar::new(phase_progress(&model.phase).unwrap_or(0.0))
                        .with_label(progress),
                    None => ProgressBar::indeterminate().with_label(progress),
                })));
                body.push(note(message));
            }
            LocalModelPhase::Error { message } => {
                body.push(note(message));
                body.push(note(&t("agent-error-retry-caption")));
            }
            LocalModelPhase::Ready { running: true, .. } => {
                body.push(note(&t("agent-ready-running-caption")));
            }
            LocalModelPhase::Ready { running: false, .. } => {
                body.push(note(&t("agent-ready-stopped-caption")));
            }
        }
        if model.package.no_runtime {
            body.push(note(&t("agent-hub-no-runtime")));
        }
        let actions = model_actions(model);
        if !actions.is_empty() {
            let mut buttons = Row::new();
            for action in actions {
                buttons = buttons.push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!(
                            "agent-models-{}-{}",
                            action_slug(action),
                            token(&model.package.id)
                        ),
                        &action_label(action),
                    ),
                    model_effect(model, action),
                )));
            }
            body.push(row(Box::new(buttons)));
        }
        section_with(
            &model.package.name,
            semantic_status_pill(&status, phase_tone(&model.phase)),
            rows(body),
        )
    }

    /// The offer to make `profile` the default, when it has a model.
    fn default_button(
        &self,
        profile: &ProviderProfile,
        default_profile: Option<&str>,
    ) -> Option<Item> {
        profile.default_model.as_ref()?;
        Some(row(Box::new(self.cx.bind_invocation(
            Button::new(
                &format!("agent-models-default-{}", profile.id),
                &if default_profile == Some(profile.id.as_str()) {
                    t("agent-default-current")
                } else {
                    t("agent-use-default")
                },
            ),
            ProviderEffect::SetDefaultProvider {
                provider: profile.id.clone(),
            },
        ))))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let private_title = t("agent-private-title");
        let local_inf = t("agent-local-inference");
        let local_gw = t("agent-local-gateway");
        let downloads = t("agent-downloads");
        let downloads_v = t("agent-downloads-value");
        let mut cards = vec![section(
            &private_title,
            rows(vec![
                note(&t("agent-private-caption")),
                value_row(&local_inf, &local_gw),
                value_row(&downloads, &downloads_v),
            ]),
        )];
        if !self.manager_available {
            cards.push(section(
                &t("agent-service-down-title"),
                rows(vec![note(&t("agent-service-down-body"))]),
            ));
            self.push_provider_cards(&mut cards);
            return crate::widgets::page(cards);
        }
        if let Some(accelerator) = self.accelerator_card() {
            cards.push(accelerator);
        }
        // A link to one in-process model puts its card first, where the
        // person who followed it sees it without hunting.
        let revealed = self
            .revealed
            .as_deref()
            .and_then(|id| self.snapshot.in_process_model(id));
        if let Some(model) = revealed {
            cards.push(self.in_process_card(model));
        }
        cards.push(self.recommendation_card());
        for model in &self.snapshot.in_process {
            if revealed.is_none_or(|shown| shown.package.id != model.package.id) {
                cards.push(self.in_process_card(model));
            }
        }
        cards.extend(self.hub.sections(&self.cx, self.manager_available));
        if self.snapshot.models.is_empty() {
            cards.push(section(
                &t("agent-models-empty-title"),
                rows(vec![note(&t("agent-models-empty"))]),
            ));
        } else {
            for model in &self.snapshot.models {
                cards.push(self.model_card(model));
            }
        }
        self.push_provider_cards(&mut cards);
        crate::widgets::page(cards)
    }
}

fn gib(n: impl Into<i64>) -> String {
    t_args("agent-gib", &[("n", n.into().into())])
}

fn fit_tone(fit: HardwareFit) -> StatusTone {
    match fit {
        HardwareFit::Excellent | HardwareFit::Good => StatusTone::Success,
        HardwareFit::Tight | HardwareFit::Unsupported => StatusTone::Warning,
    }
}

fn phase_tone(phase: &LocalModelPhase) -> StatusTone {
    match phase {
        LocalModelPhase::Ready { running: true, .. } => StatusTone::Success,
        LocalModelPhase::Ready { running: false, .. } => StatusTone::Neutral,
        LocalModelPhase::Downloading { .. } | LocalModelPhase::Installing { .. } => {
            StatusTone::Info
        }
        LocalModelPhase::Error { .. } => StatusTone::Warning,
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(750);
const MANAGER_OUTPUT_LIMIT: usize = 1024 * 1024;

fn manager_run(args: &[&str], timeout: Duration) -> Result<bounded_command::Output, String> {
    bounded_command::run(
        "tilo-model-manager",
        args,
        None,
        MANAGER_OUTPUT_LIMIT,
        timeout,
    )
    .map_err(|error| {
        t_args(
            "agent-manager-unavailable",
            &[("error", error.to_string().into())],
        )
    })
}

/// The fit's label, as the hardware card shows it.
fn fit_label(fit: HardwareFit) -> String {
    t(match fit {
        HardwareFit::Excellent => "agent-fit-excellent",
        HardwareFit::Good => "agent-fit-good",
        HardwareFit::Tight => "agent-fit-tight",
        HardwareFit::Unsupported => "agent-fit-unsupported",
    })
}

/// The card title naming what an in-process model does for the person.
fn capability_title(package: &LocalModelPackage) -> String {
    if package.segmentation {
        t("agent-inprocess-segmentation-title")
    } else {
        package.name.clone()
    }
}

/// The phase's status line.
fn phase_status(phase: &LocalModelPhase) -> String {
    t(match phase {
        LocalModelPhase::Downloading { .. } => "agent-status-downloading",
        LocalModelPhase::Installing { .. } => "agent-status-installing",
        LocalModelPhase::Ready { running: true, .. } => "agent-status-running",
        LocalModelPhase::Ready { running: false, .. } => "agent-status-ready",
        LocalModelPhase::Error { .. } => "agent-status-attention",
    })
}

/// Boundary implemented by Tilo' model service. The production adapter is a
/// deliberately tiny JSON CLI seam so Settings does not acquire downloader,
/// container-runtime, GPU-driver, or privilege-management dependencies.
#[allow(
    clippy::missing_errors_doc,
    reason = "the six adapter operations share one error contract documented on the trait"
)]
pub trait LocalModelControl: hub::HubControl + Send + Sync {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String>;
    fn install(&self, model: &LocalModelPackage) -> Result<(), String>;
    fn start(&self, id: &str) -> Result<(), String>;
    fn stop(&self, id: &str) -> Result<(), String>;
    fn remove(&self, id: &str) -> Result<(), String>;
    fn stage_accelerator(&self) -> Result<(), String>;
}

pub struct ModelManagerCli;

impl ModelManagerCli {
    fn command(verb: &str, id: &str) -> Result<(), String> {
        let output = manager_run(&[verb, "--model", id], Duration::from_secs(10 * 60))?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| format!("model service refused {verb}"))
    }
}

impl LocalModelControl for ModelManagerCli {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        crate::device_read::off_ui_thread();
        let output = manager_run(&["snapshot", "--format", "json"], Duration::from_secs(10))?;
        if !output.status.success() {
            return Err(output.stderr_diagnostic());
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("model service returned invalid status: {error}"))
    }

    fn install(&self, model: &LocalModelPackage) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        let output = manager_run(
            &[
                "install",
                "--model",
                &model.id,
                "--revision",
                &model.revision,
                "--accept-license",
            ],
            Duration::from_secs(30 * 60),
        )?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| "model service refused the installation".to_string())
    }

    fn start(&self, id: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Self::command("start", id)
    }
    fn stop(&self, id: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Self::command("stop", id)
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Self::command("remove", id)
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        let output = manager_run(&["stage-accelerator"], Duration::from_secs(30 * 60))?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| "accelerator setup service refused the request".to_string())
    }
}

fn fallback_snapshot() -> LocalModelSnapshot {
    let memory_gib = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("MemTotal:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
        })
        .and_then(|kib| u16::try_from(kib / 1024 / 1024).ok())
        .unwrap_or(8);
    let (fit, reason) = if memory_gib >= 32 {
        (HardwareFit::Excellent, t("agent-reason-excellent"))
    } else if memory_gib >= 16 {
        (HardwareFit::Good, t("agent-reason-good"))
    } else if memory_gib >= 8 {
        (HardwareFit::Tight, t("agent-reason-tight"))
    } else {
        (HardwareFit::Unsupported, t("agent-reason-unsupported"))
    };
    LocalModelSnapshot {
        hardware: HardwareRecommendation {
            fit,
            memory_gib,
            accelerator: t("agent-accel-checked"),
            accelerator_memory_gib: None,
            free_storage_gib: None,
            reason,
        },
        recommendation: None,
        models: Vec::new(),
        in_process: Vec::new(),
        accelerator_setup: None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelAction {
    Start,
    Stop,
    Remove,
    Retry,
}

/// What can be done to `model` now: its phase's actions, without Start
/// while no signed runtime here serves it.
fn model_actions(model: &InstalledLocalModel) -> Vec<ModelAction> {
    let mut actions = available_actions(&model.phase);
    if model.package.no_runtime {
        actions.retain(|action| *action != ModelAction::Start);
    }
    actions
}

fn available_actions(phase: &LocalModelPhase) -> Vec<ModelAction> {
    match phase {
        LocalModelPhase::Ready { running: true, .. } => {
            vec![ModelAction::Stop, ModelAction::Remove]
        }
        LocalModelPhase::Ready { running: false, .. } => {
            vec![ModelAction::Start, ModelAction::Remove]
        }
        LocalModelPhase::Error { .. } => vec![ModelAction::Retry, ModelAction::Remove],
        LocalModelPhase::Downloading { .. } | LocalModelPhase::Installing { .. } => Vec::new(),
    }
}

fn action_label(action: ModelAction) -> String {
    t(match action {
        ModelAction::Start => "agent-action-start",
        ModelAction::Stop => "agent-action-stop",
        ModelAction::Remove => "agent-action-remove",
        ModelAction::Retry => "agent-action-retry",
    })
}

fn action_slug(action: ModelAction) -> &'static str {
    match action {
        ModelAction::Start => "start",
        ModelAction::Stop => "stop",
        ModelAction::Remove => "remove",
        ModelAction::Retry => "retry",
    }
}

fn byte_size(bytes: u64) -> String {
    // A model is a download stored on the drive: a size.
    ui::format::size(bytes)
}

fn phase_progress(phase: &LocalModelPhase) -> Option<f32> {
    match phase {
        LocalModelPhase::Downloading {
            downloaded_bytes,
            total_bytes,
        } if *total_bytes > 0 => Some(*downloaded_bytes as f32 / *total_bytes as f32),
        LocalModelPhase::Installing {
            percent: Some(percent),
            ..
        } => Some(f32::from(*percent) / 100.0),
        _ => None,
    }
}

fn synced_local_profile(
    existing: Option<ProviderProfile>,
    models: &[InstalledLocalModel],
) -> Option<ProviderProfile> {
    let ready = models
        .iter()
        .filter(|model| {
            !model.package.in_process && matches!(model.phase, LocalModelPhase::Ready { .. })
        })
        .collect::<Vec<_>>();
    if ready.is_empty() {
        return None;
    }
    let running_endpoint = ready.iter().find_map(|model| match &model.phase {
        LocalModelPhase::Ready {
            running: true,
            endpoint: Some(endpoint),
        } if endpoint.model_id == model.package.id
            && (endpoint.url.starts_with("http://127.0.0.1:")
                || endpoint.url.starts_with("http://[::1]:")) =>
        {
            Some(endpoint)
        }
        _ => None,
    });
    // Do not manufacture a disabled profile holding `:8000` merely because
    // bytes are installed. The dynamic endpoint exists only when the manager
    // has actually started and validated the loopback server.
    if existing.is_none() && running_endpoint.is_none() {
        return None;
    }
    let mut profile = existing.unwrap_or_else(ProviderProfile::local_vllm);
    profile.models = ready
        .iter()
        .map(|model| ModelDescriptor {
            id: model.package.id.clone(),
            label: model.package.name.clone(),
            context_window: profile
                .models
                .iter()
                .find(|old| old.id == model.package.id)
                .and_then(|old| old.context_window),
            request_limits: profile
                .models
                .iter()
                .find(|old| old.id == model.package.id)
                .and_then(|old| old.request_limits.clone()),
            revision: Some(model.package.revision.clone()),
            vision: model.package.vision,
            face_recognition: model.package.face_recognition,
            tool_calling: true,
        })
        .collect();
    profile.default_model = profile
        .default_model
        .filter(|id| profile.models.iter().any(|model| model.id == *id))
        .or_else(|| profile.models.first().map(|model| model.id.clone()));
    // The manager owns port allocation. Settings persists only the endpoint
    // returned by its ready state, never the legacy `:8000` placeholder.
    if let Some(endpoint) = running_endpoint {
        profile.endpoint.clone_from(&endpoint.url);
        profile.default_model = Some(endpoint.model_id.clone());
    }
    profile.enabled = running_endpoint.is_some();
    Some(profile)
}

impl PaneModel for AgentModelsModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.take_reads();
        let revision = self
            .panel
            .admin_snapshot
            .as_ref()
            .map(|snapshot| snapshot.revision.clone());
        self.poll_admin();
        if revision
            != self
                .panel
                .admin_snapshot
                .as_ref()
                .map(|snapshot| snapshot.revision.clone())
        {
            self.read_vertex_keys();
        }
        if self.panel.admin_pending.is_none()
            && let Some((id, expected)) = self.vertex_commit.take()
            && self.panel.admin_snapshot.as_ref().is_some_and(|snapshot| {
                snapshot
                    .config
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
                    == expected.as_ref()
            })
        {
            if expected.is_some() {
                self.vertex_saved(&id);
            } else {
                if let Err(error) = self.custodian.forget_oauth(&agent_vertex::grant_id(&id)) {
                    self.notice = Some(t_args(
                        "agent-sign-in-forget-err",
                        &[("error", error.to_string().into())],
                    ));
                }
                if self
                    .vertex
                    .as_ref()
                    .is_some_and(|setup| setup.profile_id == id)
                {
                    self.cancel_vertex();
                }
                self.vertex_fields.lock_recover().remove(&id);
                self.read_vertex_keys();
            }
        }
        if !self.cx.provider_admin.is_available() && self.providers.refresh() {
            self.panel.offline_config = self.providers.get().config.clone();
            self.read_keys();
            self.read_vertex_keys();
        }
        self.poll_sign_in();
        let (hub_changed, hub_notice) = self.hub.poll();
        self.changed |= hub_changed;
        if hub_notice.is_some() {
            self.notice = hub_notice;
        }
        let polled = self.vertex.as_mut().map(VertexSetup::poll);
        match polled {
            None | Some(Polled::Nothing) => {}
            Some(Polled::Changed) => self.changed = true,
            Some(Polled::SignedInAgain) => {
                self.vertex = None;
                self.read_keys();
                self.read_vertex_keys();
                self.notice = Some(t("agent-vertex-signed-in"));
            }
        }
        // While a model is busy the manager is asked again, one read at a
        // time, each a while after the last landed.
        if self.is_busy()
            && !self.snapshot_reads.running()
            && self.last_poll.elapsed() >= POLL_INTERVAL
        {
            self.refresh();
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "AgentModelsPane"
    }

    fn busy(&self) -> bool {
        self.admin_pending.is_some()
            || self.is_busy()
            || self.snapshot_reads.running()
            || self.vertex_reads.running()
            || self.vertex.as_ref().is_some_and(VertexSetup::waiting)
            || self.hub.signing_in()
    }
}

#[cfg(test)]
mod tests;
