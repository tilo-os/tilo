//! The person's mail accounts and model providers on a page: the host that
//! runs their workspace does the work.
//!
//! A browser has no connection to a mail server and no vault of its own, so
//! the pane a device shows cannot do its own signing-in here. These are the
//! same two categories, kept by the host: it proves a mail account's
//! sign-in, seals the password in the person's own vault there, keeps the provider
//! keys, and answers what it kept. What the page shows is what it answered;
//! every change is a request to it, never a file written in the browser.
//!
//! Local models are absent by nature: they run on a computer, so the pane
//! says where they are instead of offering a control that cannot work here.
//! The device build of Settings carries that part of the same category.
//!
//! A device's own build compiles this module only for its tests: the panes
//! there are the device's, which reach the hardware and the vault directly.

#![cfg_attr(
    feature = "tilo-os",
    allow(dead_code, reason = "a device's build carries this only for its tests")
)]

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use provider_settings_ui::person_models::{
    ModelBindings, ModelChoice, PersonModels, PersonModelsEffect,
};
use ui::LocalState;
use ui::framework::{ActionControl, Actions, EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_actions::remote::{
    InvokeArgument, person_accounts,
    person_accounts::{MailAccount, MailAccounts},
};
use ui::tilo_resource::{self, Binds};
use ui::widget::{Button, ButtonRole, Element, Row, Spacer, TextInput, TextInputKind};

use crate::area::AreaContext;
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args as translated};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, field_row, note, page, row, rows, section, thing_row,
};

crate::effects::effect_family!(PersonSettingsEffect => answer);
// The model providers' requests are the shared piece's own, which every
// window that shows a person's providers offers.
crate::effects::effect_family!(PersonModelsEffect => answer_models);

/// What a page asks of the host about the person's mail accounts.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum PersonSettingsEffect {
    /// Add the mail account the form names, once the host's sign-in check
    /// passed.
    #[effect(kind = "outside")]
    #[effect(job)]
    AddMailAccount,
    /// Remove a mail account and the secret kept with it.
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
        self.model.get().is_some_and(|model| {
            model
                .try_borrow()
                .is_ok_and(|model| model.adding.is_some() || model.removing.is_some())
        })
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
pub struct AccountsModel {
    remote: ui::RemoteActions,
    listing: Option<ui::InvocationCall>,
    seen: Cell<u64>,
    accounts: Vec<MailAccount>,
    error: Option<String>,
    notice: Option<String>,
    adding: Option<ui::InvocationCall>,
    removing: Option<(String, ui::InvocationCall)>,
    form_address: LocalState<String>,
    form_incoming: LocalState<String>,
    form_incoming_port: LocalState<String>,
    form_outgoing: LocalState<String>,
    form_outgoing_port: LocalState<String>,
    form_password: LocalState<String>,
}

impl AccountsModel {
    fn new(remote: ui::RemoteActions) -> Self {
        let listing = Some(remote.invoke(
            person_accounts::target(person_accounts::LIST),
            InvokeArgument::None,
            None,
        ));
        Self {
            remote,
            listing,
            seen: Cell::new(0),
            accounts: Vec::new(),
            error: None,
            notice: None,
            adding: None,
            removing: None,
            form_address: LocalState::new(String::new()),
            form_incoming: LocalState::new(String::new()),
            form_incoming_port: LocalState::new("993".into()),
            form_outgoing: LocalState::new(String::new()),
            form_outgoing_port: LocalState::new("465".into()),
            form_password: LocalState::new(String::new()),
        }
    }

    fn apply(&mut self, effect: PersonSettingsEffect) -> Result<(), String> {
        match effect {
            PersonSettingsEffect::AddMailAccount => {
                if self.adding.is_some() {
                    return Err(t("settings-web-accounts-adding"));
                }
                let adding = person_accounts::AddMailAccount {
                    address: self.form_address.get(),
                    display_name: String::new(),
                    username: None,
                    incoming_host: self.form_incoming.get(),
                    incoming_port: port(&self.form_incoming_port.get(), 993),
                    outgoing_host: self.form_outgoing.get(),
                    outgoing_port: port(&self.form_outgoing_port.get(), 465),
                    password: self.form_password.get(),
                    identities: Vec::new(),
                };
                self.adding = Some(self.remote.invoke(
                    person_accounts::target(person_accounts::ADD_MAIL),
                    json_argument(&adding)?,
                    None,
                ));
                Ok(())
            }
            PersonSettingsEffect::RemoveMailAccount { account } => {
                self.removing = Some((
                    account.clone(),
                    self.remote.invoke(
                        person_accounts::target(person_accounts::REMOVE),
                        json_argument(&serde_json::json!({ "account": account }))?,
                        None,
                    ),
                ));
                Ok(())
            }
        }
    }

    /// Take what the host answered since the last pass. Whether the pane
    /// changed.
    fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(call) = &self.listing {
            let revision = call.revision();
            if revision != self.seen.get() {
                self.seen.set(revision);
                match settled(call) {
                    Some(Ok(value)) => match serde_json::from_value::<MailAccounts>(value) {
                        Ok(listed) => {
                            self.accounts = listed.accounts;
                            self.error = None;
                        }
                        Err(_) => self.error = Some(t("settings-web-accounts-unreadable")),
                    },
                    Some(Err(reason)) => self.error = Some(reason),
                    None => {}
                }
                changed = true;
            }
        }
        if let Some(adding) = &self.adding
            && let Some(outcome) = settled(adding)
        {
            self.adding = None;
            match outcome {
                Ok(value) => match serde_json::from_value::<MailAccount>(value) {
                    Ok(account) => {
                        self.notice = Some(translated(
                            "settings-web-accounts-added",
                            &[("name", account.address.into())],
                        ));
                        self.form_address.set(String::new());
                        self.form_incoming.set(String::new());
                        self.form_incoming_port.set("993".into());
                        self.form_outgoing.set(String::new());
                        self.form_outgoing_port.set("465".into());
                        self.form_password.set(String::new());
                        self.reread();
                    }
                    Err(_) => self.error = Some(t("settings-web-accounts-unreadable")),
                },
                Err(reason) => self.error = Some(reason),
            }
            changed = true;
        }
        if let Some((account, removing)) = &self.removing
            && let Some(outcome) = settled(removing)
        {
            let account = account.clone();
            self.removing = None;
            match outcome {
                Ok(value) => match serde_json::from_value::<MailAccounts>(value) {
                    Ok(listed) => {
                        self.accounts = listed.accounts;
                        self.error = None;
                        self.notice = Some(translated(
                            "settings-web-accounts-removed",
                            &[("name", account.into())],
                        ));
                    }
                    Err(_) => self.error = Some(t("settings-web-accounts-unreadable")),
                },
                Err(reason) => self.error = Some(reason),
            }
            changed = true;
        }
        changed
    }

    /// Ask the host for the list again. The new request counts its
    /// revisions from the start, so what was seen of the last one is
    /// forgotten: else its answer reads as already taken.
    fn reread(&mut self) {
        self.listing = Some(self.remote.invoke(
            person_accounts::target(person_accounts::LIST),
            InvokeArgument::None,
            None,
        ));
        self.seen.set(u64::MAX);
    }

    fn account_list(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let mut items = Vec::new();
        for account in &self.accounts {
            let status = if account.has_secret {
                t("settings-web-accounts-secret-kept")
            } else {
                t("settings-web-accounts-no-secret")
            };
            let details = vec![
                status,
                translated(
                    "settings-web-accounts-incoming-address",
                    &[
                        ("provider", account.incoming_host.clone().into()),
                        ("port", account.incoming_port.to_string().into()),
                    ],
                ),
                translated(
                    "settings-web-accounts-outgoing-address",
                    &[
                        ("provider", account.outgoing_host.clone().into()),
                        ("port", account.outgoing_port.to_string().into()),
                    ],
                ),
            ];
            let remove = cx.actions().bind_effect_invocation(
                Button::new(
                    &format!("settings.web.accounts.remove.{}", account.id),
                    &t("settings-web-accounts-remove"),
                ),
                &SettingsEffect::from(PersonSettingsEffect::RemoveMailAccount {
                    account: account.id.clone(),
                }),
            );
            items.push(row(Box::new(thing_row(
                &format!("settings.web.accounts.{}", account.id),
                &account.address,
                &details,
                Some(Box::new(remove)),
            ))));
        }
        if items.is_empty() {
            items.push(note(&t("settings-web-accounts-none")));
        }
        if let Some(line) = self.error.as_ref().or(self.notice.as_ref()) {
            items.push(note(line));
        }
        section(&t("settings-web-accounts-list"), rows(items))
    }

    fn page(&self, cx: &ViewCx<Settings>) -> Element {
        let list = self.account_list(cx);

        let add = cx.actions().bind_effect_invocation(
            Button::new("settings.web.accounts.add", &t("settings-web-accounts-add"))
                .role(ButtonRole::Primary),
            &SettingsEffect::from(PersonSettingsEffect::AddMailAccount),
        );
        let mut form = vec![
            labelled_field(
                &t("accounts-email"),
                TextInput::from_state(&self.form_address)
                    .with_kind(TextInputKind::Email)
                    .with_id("settings.web.accounts.address"),
            ),
            labelled_field(
                &t("settings-web-accounts-incoming"),
                TextInput::from_state(&self.form_incoming)
                    .with_kind(TextInputKind::Url)
                    .with_id("settings.web.accounts.incoming"),
            ),
            labelled_field(
                &t("settings-web-accounts-incoming-port"),
                TextInput::from_state(&self.form_incoming_port)
                    .with_kind(TextInputKind::Number)
                    .with_id("settings.web.accounts.incoming-port"),
            ),
            labelled_field(
                &t("settings-web-accounts-outgoing"),
                TextInput::from_state(&self.form_outgoing)
                    .with_kind(TextInputKind::Url)
                    .with_id("settings.web.accounts.outgoing"),
            ),
            labelled_field(
                &t("settings-web-accounts-outgoing-port"),
                TextInput::from_state(&self.form_outgoing_port)
                    .with_kind(TextInputKind::Number)
                    .with_id("settings.web.accounts.outgoing-port"),
            ),
            labelled_field(
                &t("accounts-password"),
                TextInput::from_state(&self.form_password)
                    .with_kind(TextInputKind::Password)
                    .with_secret(true)
                    .with_id("settings.web.accounts.password"),
            ),
            row(Box::new(
                Row::new().push(Box::new(add)).push(Box::new(Spacer::new())),
            )),
        ];
        if self.adding.is_some() {
            form.push(note(&t("settings-web-accounts-checking")));
        }
        page(vec![
            list,
            section(&t("settings-web-accounts-add-title"), rows(form)),
        ])
    }
}

/// `input` under its label, which also names it to assistive technology.
fn labelled_field(label: &str, input: TextInput) -> Item {
    field_row(label, Box::new(input.with_accessible_name(label)))
}

/// The host's answer to a request that has finished, or `None` while it
/// still runs.
fn settled(call: &ui::InvocationCall) -> Option<Result<serde_json::Value, String>> {
    use ui::tilo_actions::InvocationUpdate;
    match call.get() {
        ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
            Some(Ok(output.value.unwrap_or(serde_json::Value::Null)))
        }
        ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Some(Err(error.message)),
        ui::Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => Some(Err(
            message.unwrap_or_else(|| t("settings-web-request-cancelled"))
        )),
        ui::Arrival::Failed(error) => Some(Err(error.to_string())),
        ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => None,
    }
}

/// The port typed in a field, or `default` when it names none.
fn port(value: &str, default: u16) -> u16 {
    value
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .unwrap_or(default)
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

    use std::sync::Arc;

    use ui::tilo_actions::remote::{Caller, ReceiptStatus, TestProvider};

    use super::*;

    fn en() {
        ui::i18n::I18nManager::init("en-US");
    }

    fn account() -> MailAccount {
        MailAccount {
            id: "ada@example.test".into(),
            address: "ada@example.test".into(),
            display_name: "Ada".into(),
            incoming_host: "imap.example.test".into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.test".into(),
            outgoing_port: 465,
            has_secret: true,
        }
    }

    /// A host that keeps one account and answers every change with the
    /// value it names.
    fn provider() -> Arc<TestProvider> {
        Arc::new(TestProvider::new().answering(|_, request| {
            let value = match request.target.address.action.as_str() {
                person_accounts::LIST => serde_json::to_value(MailAccounts {
                    accounts: vec![account()],
                    ..MailAccounts::default()
                }),
                person_accounts::ADD_MAIL => serde_json::to_value(account()),
                person_accounts::REMOVE => serde_json::to_value(MailAccounts::default()),
                _ => serde_json::to_value(serde_json::Value::Null),
            }
            .expect("the test provider's answer");
            ui::tilo_actions::remote::Pending::ready(Ok(
                ui::tilo_actions::remote::InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Completed {
                        output: ui::tilo_actions::ActionOutput {
                            value: Some(value),
                            ..Default::default()
                        },
                    },
                },
            ))
        }))
    }

    fn remote(provider: &Arc<TestProvider>) -> ui::RemoteActions {
        let api: Arc<dyn ui::tilo_actions::remote::ActionProvider + Send + Sync> = provider.clone();
        ui::RemoteActions::with_provider(api, Caller::local_person("owner"))
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
            PersonSettingsEffect::AddMailAccount,
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

    /// The page shows the accounts the host answered, and never a secret:
    /// the reading has no field for one.
    #[test]
    fn the_pane_shows_what_the_host_keeps() {
        en();
        let provider = provider();
        let mut accounts = AccountsModel::new(remote(&provider));
        assert!(accounts.poll());
        assert_eq!(accounts.accounts, vec![account()]);
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

    /// Adding a mail account is a request to the host carrying exactly what
    /// the person typed, and the form clears only once the host kept it.
    #[test]
    fn adding_a_mail_account_asks_the_host_with_what_was_typed() {
        en();
        let provider = provider();
        let mut accounts = AccountsModel::new(remote(&provider));
        accounts.poll();
        accounts.form_address.set("ada@example.test".into());
        accounts.form_incoming.set("imap.example.test".into());
        accounts.form_outgoing.set("smtp.example.test".into());
        accounts.form_password.set("hunter2".into());
        accounts
            .apply(PersonSettingsEffect::AddMailAccount)
            .unwrap();

        let adding = provider
            .asked()
            .into_iter()
            .find(|request| request.target.address.action == person_accounts::ADD_MAIL)
            .expect("the page asked the provider to add the account");
        let InvokeArgument::Json(argument) = adding.argument else {
            panic!("the account travels as JSON");
        };
        let sent: person_accounts::AddMailAccount =
            serde_json::from_value(serde_json::Value::Object(argument)).unwrap();
        assert_eq!(sent.address, "ada@example.test");
        assert_eq!(sent.incoming_host, "imap.example.test");
        assert_eq!(sent.outgoing_host, "smtp.example.test");
        assert_eq!(sent.password, "hunter2");
        assert_eq!((sent.incoming_port, sent.outgoing_port), (993, 465));

        assert!(accounts.poll());
        assert_eq!(accounts.form_password.get(), "");
        assert!(accounts.notice.is_some());
        // The list is read again, and what the host keeps now shows.
        accounts.accounts.clear();
        accounts.poll();
        assert_eq!(
            accounts.accounts,
            vec![account()],
            "the list is read again after an account is added"
        );
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
