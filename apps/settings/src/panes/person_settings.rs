//! The person's accounts and model providers on a page, kept by the server
//! their workspace is on.
//!
//! A browser has no connection to a mail server and no vault of its own, so
//! the server does the work: it signs in to the provider, seals the
//! password or grant in the person's own vault there, keeps the provider
//! keys, and answers what it kept. What the page shows is what it answered;
//! every change is a request to it, never a file written in the browser.
//!
//! Local models are absent by nature: they need a computer, so the pane
//! says where they are instead of offering a control that cannot work here.
//! The device build of Settings carries that part of the same category.
//!
//! A hosted window uses this provider pane even when its binary can also
//! run the device panes. Mail on the device has its own accounts pane.

#![cfg_attr(
    feature = "tilo-os",
    allow(dead_code, reason = "the device uses its own mail accounts pane")
)]

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use provider_settings_ui::person_models::{
    ModelBindings, ModelChoice, PersonModels, PersonModelsEffect,
};
use ui::LocalState;
use ui::framework::{ActionControl, Actions, EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_actions::remote::{ActionCallError, InvokeArgument};
use ui::tilo_resource::{self, Binds};
use ui::widget::{Element, TextInput};

use crate::area::AreaContext;
use crate::effects::SettingsEffect;
use crate::i18n::t;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{Item, SettingsSection, field_row, note, page, rows, section};

mod accounts;

pub use accounts::AccountsModel;

crate::effects::effect_family!(PersonSettingsEffect => answer);
// The model providers' requests are the shared piece's own, which every
// window that shows a person's providers offers.
crate::effects::effect_family!(PersonModelsEffect => answer_models);

/// What a page asks of the server about the person's accounts.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum PersonSettingsEffect {
    /// Show the step that adds an account of `provider`.
    #[effect(kind = "outside")]
    ChooseProvider {
        #[param("Who provides the account: google, microsoft, icloud, nextcloud or other")]
        provider: String,
    },
    /// Stop adding an account and show the providers again.
    #[effect(kind = "outside")]
    CancelAdding,
    /// Show the fields for the mail servers of the account being added.
    #[effect(kind = "outside")]
    SetUpManually,
    /// Open the provider's own page to sign in to the account being added.
    #[effect(kind = "outside")]
    #[effect(job)]
    SignIn,
    /// Sign in to an account again on its provider's page, when the sign-in
    /// it was kept with stopped working.
    #[effect(kind = "outside")]
    #[effect(job)]
    SignInAgain {
        #[param("The account's id, as the list names it")]
        account: String,
    },
    /// Look up the mail servers of the address typed.
    #[effect(kind = "outside")]
    #[effect(job)]
    FindServers,
    /// Add the account the step names, once the server's sign-in check
    /// passed.
    #[effect(kind = "outside")]
    #[effect(job)]
    AddMailAccount,
    /// Stop a sign-in on a provider's page that made no account yet.
    #[effect(kind = "outside")]
    #[effect(label = "settings-effect-cancel-account-sign-in")]
    CancelSignIn {
        #[param("The sign-in's id, as the list names it")]
        sign_in: String,
    },
    /// Turn an app on or off for an account.
    #[effect(kind = "outside")]
    SetService {
        #[param("The account's id, as the list names it")]
        account: String,
        #[param("The app: mail or calendar")]
        service: String,
        #[param("Whether the account feeds it")]
        on: bool,
    },
    /// Remove an account and the secret kept with it.
    #[effect(kind = "outside")]
    #[effect(job, destructive)]
    RemoveMailAccount {
        #[param("The account's id, as the list names it")]
        account: String,
    },
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: PersonSettingsEffect,
    _cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let area = app
        .areas()
        .find::<AccountsArea>()
        .ok_or_else(crate::area::missing)?;
    area.model().borrow_mut().apply(effect)?;
    // A step chosen here (a provider, the servers' fields) shows at once,
    // before any answer from the server comes back.
    area.shown.set(area.shown.get().wrapping_add(1));
    Ok(Effected::done())
}

pub(crate) fn answer_models(
    app: &SettingsApp,
    effect: PersonModelsEffect,
    _cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let area = app
        .areas()
        .find::<ProvidersArea>()
        .ok_or_else(crate::area::missing)?;
    area.model().borrow_mut().apply(effect)?;
    // What the request opened (a key's field, a form) shows at once.
    area.shown.set(area.shown.get().wrapping_add(1));
    Ok(Effected::done())
}

/// The shared piece's controls, offered as Settings' own effects.
struct Offered(Actions<Settings>);

impl ModelBindings for Offered {
    fn invoking<C: ActionControl<()>>(&self, control: C, effect: PersonModelsEffect) -> C {
        self.0
            .bind_effect_invocation(control, &SettingsEffect::from(effect))
    }

    fn choosing<C: ActionControl<String>>(&self, control: C, choice: ModelChoice) -> C {
        self.0.bind_effect(control, Chosen(choice))
    }
}

/// A model chooser's choice, as the effect of Settings it is.
struct Chosen(ModelChoice);

impl Binds<SettingsEffect, String> for Chosen {
    fn action_id(&self) -> &'static str {
        Binds::<PersonModelsEffect, String>::action_id(&self.0)
    }

    fn arguments(
        &self,
        input: String,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        Binds::<PersonModelsEffect, String>::arguments(&self.0, input)
    }
}

crate::area::pane_area! {
    AccountsArea;
    fn poll(&self) {
        AccountsArea::poll(self);
    }
    fn busy(&self) -> bool {
        AccountsArea::busy(self)
    }
}

crate::area::pane_area! {
    ProvidersArea;
    fn poll(&self) {
        ProvidersArea::poll(self);
    }
    fn busy(&self) -> bool {
        ProvidersArea::busy(self)
    }
}

pub fn accounts_area(app: &SettingsApp) -> Option<&AccountsArea> {
    app.areas().find()
}

pub fn providers_area(app: &SettingsApp) -> Option<&ProvidersArea> {
    app.areas().find()
}

pub struct AccountsArea {
    remote: ui::RemoteActions,
    model: OnceCell<Rc<RefCell<AccountsModel>>>,
    shown: LocalState<u64>,
}

impl AccountsArea {
    pub fn new(_context: &AreaContext, remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            model: OnceCell::new(),
            shown: LocalState::new(0),
        }
    }

    pub fn model(&self) -> &Rc<RefCell<AccountsModel>> {
        self.model
            .get_or_init(|| Rc::new(RefCell::new(AccountsModel::new(self.remote.clone()))))
    }

    fn poll(&self) {
        if let Some(model) = self.model.get()
            && let Ok(mut model) = model.try_borrow_mut()
            && model.poll()
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    fn busy(&self) -> bool {
        self.model
            .get()
            .is_some_and(|model| model.try_borrow().is_ok_and(|model| model.busy()))
    }
}

pub struct ProvidersArea {
    remote: ui::RemoteActions,
    model: OnceCell<Rc<RefCell<PersonModels>>>,
    shown: LocalState<u64>,
}

impl ProvidersArea {
    pub fn new(_context: &AreaContext, remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            model: OnceCell::new(),
            shown: LocalState::new(0),
        }
    }

    pub fn model(&self) -> &Rc<RefCell<PersonModels>> {
        self.model
            .get_or_init(|| Rc::new(RefCell::new(PersonModels::new(self.remote.clone()))))
    }

    fn poll(&self) {
        if let Some(model) = self.model.get()
            && let Ok(mut model) = model.try_borrow_mut()
            && model.poll()
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    fn busy(&self) -> bool {
        self.model
            .get()
            .is_some_and(|model| model.try_borrow().is_ok_and(|model| model.busy()))
    }
}

/// The mail accounts the workspace keeps, and the form that adds one.
/// `input` under its label, which also names it to assistive technology.
fn labelled_field(label: &str, input: TextInput) -> Item {
    field_row(label, Box::new(input.with_accessible_name(label)))
}

/// The host's answer to a request that has finished, or `None` while it
/// still runs.
pub(crate) fn settled(call: &ui::InvocationCall) -> Option<Result<serde_json::Value, String>> {
    use ui::tilo_actions::InvocationUpdate;
    match call.get() {
        ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
            Some(Ok(output.value.unwrap_or(serde_json::Value::Null)))
        }
        ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Some(Err(error.message)),
        ui::Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => Some(Err(
            message.unwrap_or_else(|| t("settings-web-request-cancelled"))
        )),
        ui::Arrival::Failed(error) => Some(Err(unreached(&error))),
        ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => None,
    }
}

/// A request the call layer answered with an error, in the person's words.
/// The server's own refusal is written for them (no servers found for the
/// address, a password the provider did not take) and is shown as it is; for
/// a request that never reached the server, what the call layer says is for
/// the console, where whoever looks into it reads why, not for the page.
fn unreached(error: &ActionCallError) -> String {
    if let ActionCallError::Refused { reason } = error {
        return reason.clone();
    }
    tracing::warn!(?error, "settings: a request did not reach the server");
    t("settings-web-accounts-unreachable")
}

/// `value` as an action's argument map.
fn json_argument(value: &impl serde::Serialize) -> Result<InvokeArgument, String> {
    let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
    match value {
        serde_json::Value::Object(map) => Ok(InvokeArgument::Json(map)),
        _ => Err("a request's arguments are a JSON object".into()),
    }
}

pub(crate) fn accounts_view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = accounts_area(app)?;
    let model = Rc::clone(held.model());
    model.borrow_mut().poll();
    Some(cx.switch(&held.shown, move |cx, _| model.borrow().page(cx)))
}

/// The person's model providers: the piece every window shows them with,
/// and where the models that run on a computer are.
pub(crate) fn providers_view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = providers_area(app)?;
    let model = Rc::clone(held.model());
    model.borrow_mut().poll();
    Some(cx.switch(&held.shown, move |cx, _| {
        let mut sections: Vec<SettingsSection> = model.borrow().sections(&Offered(cx.actions()));
        sections.push(section(
            &t("settings-web-local-models"),
            rows(vec![note(&t("settings-web-local-models-elsewhere"))]),
        ));
        page(sections)
    }))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    fn en() {
        ui::i18n::I18nManager::init("en-US");
    }

    /// The panes bind every request to a button on Settings' own resource,
    /// so each is offered there: one offered only on a row is never among
    /// the actions a window permits, and its button stays disabled for
    /// every role.
    #[test]
    fn every_request_is_offered_where_its_button_binds_it() {
        use ui::tilo_resource::Effect as _;
        let offered: Vec<String> = PersonSettingsEffect::specs()
            .into_iter()
            .map(|spec| spec.id.clone())
            .collect();
        for effect in [
            PersonSettingsEffect::ChooseProvider {
                provider: "google".into(),
            },
            PersonSettingsEffect::CancelAdding,
            PersonSettingsEffect::SetUpManually,
            PersonSettingsEffect::SignIn,
            PersonSettingsEffect::FindServers,
            PersonSettingsEffect::AddMailAccount,
            PersonSettingsEffect::CancelSignIn {
                sign_in: "one".into(),
            },
            PersonSettingsEffect::SetService {
                account: "ada@example.test".into(),
                service: "calendar".into(),
                on: false,
            },
            PersonSettingsEffect::RemoveMailAccount {
                account: "ada@example.test".into(),
            },
        ] {
            assert!(
                offered.iter().any(|id| id == effect.id()),
                "{} is not offered on the resource: {offered:?}",
                effect.id()
            );
        }
    }

    /// Every request of the shared providers piece is one Settings takes as
    /// its own effect and reads back unchanged, and a chooser's choice is
    /// offered as the effect that records it.
    #[test]
    fn the_providers_requests_are_settings_own() {
        use ui::tilo_resource::Effect as _;
        for effect in [
            PersonModelsEffect::ReadProviders,
            PersonModelsEffect::LinkProvider {
                preset: "gemini".into(),
            },
            PersonModelsEffect::EnterProviderKey {
                provider: "gemini".into(),
            },
            PersonModelsEffect::UnlinkProvider {
                provider: "gemini".into(),
            },
            PersonModelsEffect::ChooseDefaultModel {
                provider: "gemini".into(),
                model: "large".into(),
            },
            PersonModelsEffect::ChooseWorkModel {
                work: "main-thread".into(),
                provider: None,
                model: None,
            },
        ] {
            let settings = SettingsEffect::from(effect.clone());
            assert_eq!(
                PersonModelsEffect::decode(settings.id(), &settings.arguments()).ok(),
                Some(effect)
            );
        }
        let chosen = Chosen(ModelChoice(None));
        let arguments = chosen
            .arguments("gemini\u{1}large".to_owned())
            .expect("a choice names a provider and its model");
        assert_eq!(
            PersonModelsEffect::decode(chosen.action_id(), &arguments).ok(),
            Some(PersonModelsEffect::ChooseDefaultModel {
                provider: "gemini".into(),
                model: "large".into(),
            })
        );
    }

    /// The link a window shows a person's model with opens this pane, as
    /// every other link to a page of Settings does.
    #[test]
    fn the_models_link_opens_this_pane() {
        let opened =
            crate::resource::parse_settings_uri(provider_settings_ui::person_models::SETTINGS_LINK)
                .expect("the link names a page of Settings");
        let pane = crate::effects::open_pane(crate::categories::Category::AgentModels);
        assert_eq!(opened, pane);
    }

    /// The local-model section is a statement, not a control: a page cannot
    /// download or run a model.
    #[test]
    fn local_models_say_where_they_are() {
        en();
        let said = t("settings-web-local-models-elsewhere");
        assert!(said.contains("computer"), "{said}");
    }
}
