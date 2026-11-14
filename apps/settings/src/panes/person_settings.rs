//! The person's mail accounts and model providers on a page: the host that
//! runs their workspace does the work.
//!
//! A browser has no connection to a mail server and no vault of its own, so
//! the pane a device shows cannot do its own signing-in here. These are the
//! same two categories, kept by the host: it proves a mail account's
//! sign-in, seals the password in the workspace's vault, keeps the provider
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

use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_actions::remote::{
    InvokeArgument, person_accounts,
    person_accounts::{MailAccount, MailAccounts},
    person_models,
    person_models::{ModelProviders, Protocol, ProviderView},
};
use ui::tilo_resource;
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

/// What a page asks of the host about the person's accounts and providers.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum PersonSettingsEffect {
    /// Add the mail account the form names, once the host's sign-in check
    /// passed.
    #[effect(job, row)]
    AddMailAccount,
    /// Remove a mail account and the secret kept with it.
    #[effect(job, row, destructive)]
    RemoveMailAccount {
        #[param("The account's id, as the list names it")]
        account: String,
    },
    /// Add the provider the form names.
    #[effect(job, row)]
    AddProvider,
    /// Show the field that takes one provider's key.
    #[effect(row)]
    AskProviderKey {
        #[param("The provider's id, as the list names it")]
        provider: String,
    },
    /// Keep the key typed under one provider.
    #[effect(job, row)]
    SaveProviderKey {
        #[param("The provider's id, as the list names it")]
        provider: String,
    },
    /// Remove a provider's key.
    #[effect(job, row, destructive)]
    RemoveProviderKey {
        #[param("The provider's id, as the list names it")]
        provider: String,
    },
    /// Remove a provider and its key.
    #[effect(job, row, destructive)]
    RemoveProvider {
        #[param("The provider's id, as the list names it")]
        provider: String,
    },
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: PersonSettingsEffect,
    _cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    if let Some(area) = app.areas().find::<AccountsArea>()
        && matches!(
            effect,
            PersonSettingsEffect::AddMailAccount | PersonSettingsEffect::RemoveMailAccount { .. }
        )
    {
        area.model().borrow_mut().apply(effect)?;
        return Ok(Effected::done());
    }
    let area = app
        .areas()
        .find::<ProvidersArea>()
        .ok_or_else(crate::area::missing)?;
    if let PersonSettingsEffect::AskProviderKey { provider } = &effect {
        area.model().borrow_mut().ask_key(provider);
        return Ok(Effected::done());
    }
    area.model().borrow_mut().apply(effect)?;
    Ok(Effected::done())
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
    model: OnceCell<Rc<RefCell<ProvidersModel>>>,
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

    pub fn model(&self) -> &Rc<RefCell<ProvidersModel>> {
        self.model
            .get_or_init(|| Rc::new(RefCell::new(ProvidersModel::new(self.remote.clone()))))
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
            _ => Err("that is not an accounts request".into()),
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

    fn reread(&mut self) {
        self.listing = Some(self.remote.invoke(
            person_accounts::target(person_accounts::LIST),
            InvokeArgument::None,
            None,
        ));
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
                        ("host", account.incoming_host.clone().into()),
                        ("port", account.incoming_port.to_string().into()),
                    ],
                ),
                translated(
                    "settings-web-accounts-outgoing-address",
                    &[
                        ("host", account.outgoing_host.clone().into()),
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
            field_row(
                &t("accounts-email"),
                Box::new(
                    TextInput::from_state(&self.form_address)
                        .with_kind(TextInputKind::Email)
                        .with_id("settings.web.accounts.address"),
                ),
            ),
            field_row(
                &t("settings-web-accounts-incoming"),
                Box::new(
                    TextInput::from_state(&self.form_incoming)
                        .with_kind(TextInputKind::Url)
                        .with_id("settings.web.accounts.incoming"),
                ),
            ),
            field_row(
                &t("settings-web-accounts-incoming-port"),
                Box::new(
                    TextInput::from_state(&self.form_incoming_port)
                        .with_kind(TextInputKind::Number)
                        .with_id("settings.web.accounts.incoming-port"),
                ),
            ),
            field_row(
                &t("settings-web-accounts-outgoing"),
                Box::new(
                    TextInput::from_state(&self.form_outgoing)
                        .with_kind(TextInputKind::Url)
                        .with_id("settings.web.accounts.outgoing"),
                ),
            ),
            field_row(
                &t("settings-web-accounts-outgoing-port"),
                Box::new(
                    TextInput::from_state(&self.form_outgoing_port)
                        .with_kind(TextInputKind::Number)
                        .with_id("settings.web.accounts.outgoing-port"),
                ),
            ),
            field_row(
                &t("accounts-password"),
                Box::new(
                    TextInput::from_state(&self.form_password)
                        .with_kind(TextInputKind::Password)
                        .with_secret(true)
                        .with_id("settings.web.accounts.password"),
                ),
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

/// The model providers the workspace keeps, and the form that adds one.
pub struct ProvidersModel {
    remote: ui::RemoteActions,
    listing: Option<ui::InvocationCall>,
    seen: Cell<u64>,
    providers: Vec<ProviderView>,
    error: Option<String>,
    notice: Option<String>,
    adding: Option<ui::InvocationCall>,
    form_name: LocalState<String>,
    form_endpoint: LocalState<String>,
    form_protocol: LocalState<String>,
    /// The provider whose key is being typed, and the key itself.
    key_for: Option<String>,
    form_key: LocalState<String>,
    saving_key: Option<(String, ui::InvocationCall)>,
    removing_key: Option<(String, ui::InvocationCall)>,
    removing: Option<(String, ui::InvocationCall)>,
}

impl ProvidersModel {
    fn new(remote: ui::RemoteActions) -> Self {
        let listing = Some(remote.invoke(
            person_models::target(person_models::LIST),
            InvokeArgument::None,
            None,
        ));
        Self {
            remote,
            listing,
            seen: Cell::new(0),
            providers: Vec::new(),
            error: None,
            notice: None,
            adding: None,
            form_name: LocalState::new(String::new()),
            form_endpoint: LocalState::new(String::new()),
            form_protocol: LocalState::new(String::new()),
            key_for: None,
            form_key: LocalState::new(String::new()),
            saving_key: None,
            removing_key: None,
            removing: None,
        }
    }

    fn busy(&self) -> bool {
        self.adding.is_some()
            || self.saving_key.is_some()
            || self.removing_key.is_some()
            || self.removing.is_some()
    }

    fn ask_key(&mut self, provider: &str) {
        self.key_for = Some(provider.to_owned());
        self.form_key.set(String::new());
    }

    fn apply(&mut self, effect: PersonSettingsEffect) -> Result<(), String> {
        match effect {
            PersonSettingsEffect::AddProvider => {
                let protocol = match self.form_protocol.get().as_str() {
                    "anthropic" => Protocol::Anthropic,
                    _ => Protocol::OpenAiCompatible,
                };
                let adding = person_models::AddProvider {
                    name: self.form_name.get(),
                    protocol,
                    endpoint: self.form_endpoint.get(),
                };
                self.adding = Some(self.remote.invoke(
                    person_models::target(person_models::ADD_PROVIDER),
                    json_argument(&adding)?,
                    None,
                ));
                Ok(())
            }
            PersonSettingsEffect::SaveProviderKey { provider } => {
                let setting = person_models::SetKey {
                    provider: provider.clone(),
                    key: self.form_key.get(),
                };
                self.saving_key = Some((
                    provider,
                    self.remote.invoke(
                        person_models::target(person_models::SET_KEY),
                        json_argument(&setting)?,
                        None,
                    ),
                ));
                Ok(())
            }
            PersonSettingsEffect::RemoveProviderKey { provider } => {
                self.removing_key = Some((
                    provider.clone(),
                    self.remote.invoke(
                        person_models::target(person_models::REMOVE_KEY),
                        json_argument(&serde_json::json!({ "provider": provider }))?,
                        None,
                    ),
                ));
                Ok(())
            }
            PersonSettingsEffect::RemoveProvider { provider } => {
                self.removing = Some((
                    provider.clone(),
                    self.remote.invoke(
                        person_models::target(person_models::REMOVE_PROVIDER),
                        json_argument(&serde_json::json!({ "provider": provider }))?,
                        None,
                    ),
                ));
                Ok(())
            }
            _ => Err("that is not a providers request".into()),
        }
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(call) = &self.listing {
            let revision = call.revision();
            if revision != self.seen.get() {
                self.seen.set(revision);
                match settled(call) {
                    Some(Ok(value)) => match serde_json::from_value::<ModelProviders>(value) {
                        Ok(listed) => {
                            self.providers = listed.providers;
                            self.error = None;
                        }
                        Err(_) => self.error = Some(t("settings-web-providers-unreadable")),
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
                Ok(value) => match serde_json::from_value::<ProviderView>(value) {
                    Ok(provider) => {
                        self.notice = Some(translated(
                            "settings-web-providers-added",
                            &[("name", provider.name.into())],
                        ));
                        self.form_name.set(String::new());
                        self.form_endpoint.set(String::new());
                        self.key_for = Some(provider.id);
                        self.reread();
                    }
                    Err(_) => self.error = Some(t("settings-web-providers-unreadable")),
                },
                Err(reason) => self.error = Some(reason),
            }
            changed = true;
        }
        if let Some((provider, saving)) = &self.saving_key
            && let Some(outcome) = settled(saving)
        {
            let provider = provider.clone();
            self.saving_key = None;
            match outcome {
                Ok(_) => {
                    self.notice = Some(t("settings-web-providers-key-saved"));
                    self.key_for = None;
                    self.form_key.set(String::new());
                    let _ = provider;
                    self.reread();
                }
                Err(reason) => self.error = Some(reason),
            }
            changed = true;
        }
        if let Some((provider, removing)) = &self.removing_key
            && let Some(outcome) = settled(removing)
        {
            let provider = provider.clone();
            self.removing_key = None;
            match outcome {
                Ok(_) => {
                    self.notice = Some(translated(
                        "settings-web-providers-key-removed",
                        &[("name", provider.into())],
                    ));
                    self.reread();
                }
                Err(reason) => self.error = Some(reason),
            }
            changed = true;
        }
        if let Some((provider, removing)) = &self.removing
            && let Some(outcome) = settled(removing)
        {
            let provider = provider.clone();
            self.removing = None;
            match outcome {
                Ok(value) => match serde_json::from_value::<ModelProviders>(value) {
                    Ok(listed) => {
                        self.providers = listed.providers;
                        self.notice = Some(translated(
                            "settings-web-providers-removed",
                            &[("name", provider.into())],
                        ));
                    }
                    Err(_) => self.error = Some(t("settings-web-providers-unreadable")),
                },
                Err(reason) => self.error = Some(reason),
            }
            changed = true;
        }
        changed
    }

    fn reread(&mut self) {
        self.listing = Some(self.remote.invoke(
            person_models::target(person_models::LIST),
            InvokeArgument::None,
            None,
        ));
    }

    fn page(&self, cx: &ViewCx<Settings>) -> Element {
        page(vec![
            self.list_section(cx),
            self.add_section(cx),
            section(
                &t("settings-web-local-models"),
                rows(vec![note(&t("settings-web-local-models-elsewhere"))]),
            ),
        ])
    }

    /// The providers the host keeps, each with its key's state.
    fn list_section(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let mut items = Vec::new();
        for provider in &self.providers {
            items.extend(self.provider_rows(cx, provider));
        }
        if items.is_empty() {
            items.push(note(&t("settings-web-providers-none")));
        }
        if let Some(line) = self.error.as_ref().or(self.notice.as_ref()) {
            items.push(note(line));
        }
        section(&t("settings-web-providers-list"), rows(items))
    }

    /// One provider's row, and under it the field taking its key while it is
    /// being typed.
    fn provider_rows(&self, cx: &ViewCx<Settings>, provider: &ProviderView) -> Vec<Item> {
        let status = if provider.configured {
            t("settings-web-providers-key-kept")
        } else {
            t("settings-web-providers-no-key")
        };
        let mut details = vec![status, provider.endpoint.clone()];
        if !provider.models.is_empty() {
            details.push(
                provider
                    .models
                    .iter()
                    .map(|model| model.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        let mut trailing = Row::new();
        if provider.configured {
            trailing = trailing.push(Box::new(cx.actions().bind_effect_invocation(
                Button::new(
                    &format!("settings.web.providers.remove-key.{}", provider.id),
                    &t("settings-web-providers-remove-key"),
                ),
                &SettingsEffect::from(PersonSettingsEffect::RemoveProviderKey {
                    provider: provider.id.clone(),
                }),
            )));
        } else {
            trailing = trailing.push(Box::new(cx.actions().bind_effect_invocation(
                Button::new(
                    &format!("settings.web.providers.add-key.{}", provider.id),
                    &t("settings-web-providers-add-key"),
                ),
                &SettingsEffect::from(PersonSettingsEffect::AskProviderKey {
                    provider: provider.id.clone(),
                }),
            )));
        }
        trailing = trailing.push(Box::new(cx.actions().bind_effect_invocation(
            Button::new(
                &format!("settings.web.providers.remove.{}", provider.id),
                &t("settings-web-providers-remove"),
            ),
            &SettingsEffect::from(PersonSettingsEffect::RemoveProvider {
                provider: provider.id.clone(),
            }),
        )));
        let mut rows_ = vec![row(Box::new(thing_row(
            &format!("settings.web.providers.{}", provider.id),
            &provider.name,
            &details,
            Some(Box::new(trailing)),
        )))];
        if self.key_for.as_deref() == Some(provider.id.as_str()) {
            let save = cx.actions().bind_effect_invocation(
                Button::new(
                    &format!("settings.web.providers.save-key.{}", provider.id),
                    &t("settings-web-providers-save-key"),
                )
                .role(ButtonRole::Primary),
                &SettingsEffect::from(PersonSettingsEffect::SaveProviderKey {
                    provider: provider.id.clone(),
                }),
            );
            rows_.push(field_row(
                &t("settings-web-providers-key"),
                Box::new(
                    TextInput::from_state(&self.form_key)
                        .with_kind(TextInputKind::Password)
                        .with_secret(true)
                        .with_id(&format!("settings.web.providers.key.{}", provider.id)),
                ),
            ));
            rows_.push(row(Box::new(
                Row::new()
                    .push(Box::new(save))
                    .push(Box::new(Spacer::new())),
            )));
        }
        rows_
    }

    /// The form that adds a provider.
    fn add_section(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let add = cx.actions().bind_effect_invocation(
            Button::new(
                "settings.web.providers.add",
                &t("settings-web-providers-add"),
            )
            .role(ButtonRole::Primary),
            &SettingsEffect::from(PersonSettingsEffect::AddProvider),
        );
        section(
            &t("settings-web-providers-add-title"),
            rows(vec![
                field_row(
                    &t("settings-web-providers-name"),
                    Box::new(
                        TextInput::from_state(&self.form_name)
                            .with_kind(TextInputKind::Name)
                            .with_id("settings.web.providers.name"),
                    ),
                ),
                field_row(
                    &t("settings-web-providers-endpoint"),
                    Box::new(
                        TextInput::from_state(&self.form_endpoint)
                            .with_kind(TextInputKind::Url)
                            .with_id("settings.web.providers.endpoint"),
                    ),
                ),
                note(&t("settings-web-providers-endpoint-hint")),
                row(Box::new(
                    Row::new().push(Box::new(add)).push(Box::new(Spacer::new())),
                )),
            ]),
        )
    }
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

pub(crate) fn providers_view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = providers_area(app)?;
    let model = Rc::clone(held.model());
    model.borrow_mut().poll();
    Some(cx.switch(&held.shown, move |cx, _| model.borrow().page(cx)))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::sync::Arc;

    use ui::tilo_actions::remote::{Caller, InvokeRequest, ReceiptStatus, TestHost};

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

    fn provider() -> ProviderView {
        ProviderView {
            id: "openai".into(),
            name: "OpenAI".into(),
            protocol: Protocol::OpenAiCompatible,
            endpoint: "https://api.openai.com/v1".into(),
            configured: true,
            models: Vec::new(),
            default_model: None,
        }
    }

    /// A host that keeps one account and one provider, and answers every
    /// change with the value it names.
    fn host() -> Arc<TestHost> {
        Arc::new(TestHost::new().answering(|_, request| {
            let providers = request.target.provider == person_models::PROVIDER;
            let value = match (providers, request.target.address.action.as_str()) {
                (false, person_accounts::LIST) => serde_json::to_value(MailAccounts {
                    accounts: vec![account()],
                    ..MailAccounts::default()
                }),
                (false, person_accounts::ADD_MAIL) => serde_json::to_value(account()),
                (false, person_accounts::REMOVE) => serde_json::to_value(MailAccounts::default()),
                (true, person_models::LIST) => serde_json::to_value(ModelProviders {
                    providers: vec![provider()],
                    ..ModelProviders::default()
                }),
                (true, person_models::ADD_PROVIDER) => serde_json::to_value(provider()),
                (true, person_models::SET_KEY) => serde_json::to_value(provider()),
                (true, person_models::REMOVE_KEY) => serde_json::to_value(provider()),
                (true, person_models::REMOVE_PROVIDER) => {
                    serde_json::to_value(ModelProviders::default())
                }
                _ => serde_json::to_value(serde_json::Value::Null),
            }
            .expect("the test host's answer");
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

    fn remote(host: &Arc<TestHost>) -> ui::RemoteActions {
        let door: Arc<dyn ui::tilo_actions::remote::ActionHost + Send + Sync> = host.clone();
        ui::RemoteActions::with_host(door, Caller::local_person("owner"))
    }

    /// The page shows the accounts and providers the host answered, and
    /// never a secret: the reading has no field for one.
    #[test]
    fn the_panes_show_what_the_host_keeps() {
        en();
        let host = host();
        let mut accounts = AccountsModel::new(remote(&host));
        assert!(accounts.poll());
        assert_eq!(accounts.accounts, vec![account()]);

        let mut providers = ProvidersModel::new(remote(&host));
        assert!(providers.poll());
        assert_eq!(providers.providers, vec![provider()]);
        assert!(providers.providers[0].configured);
    }

    /// Adding a mail account is a request to the host carrying exactly what
    /// the person typed, and the form clears only once the host kept it.
    #[test]
    fn adding_a_mail_account_asks_the_host_with_what_was_typed() {
        en();
        let host = host();
        let mut accounts = AccountsModel::new(remote(&host));
        accounts.poll();
        accounts.form_address.set("ada@example.test".into());
        accounts.form_incoming.set("imap.example.test".into());
        accounts.form_outgoing.set("smtp.example.test".into());
        accounts.form_password.set("hunter2".into());
        accounts
            .apply(PersonSettingsEffect::AddMailAccount)
            .unwrap();

        let adding = host
            .asked()
            .into_iter()
            .find(|request| request.target.address.action == person_accounts::ADD_MAIL)
            .expect("the page asked the host to add the account");
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
    }

    /// A provider's key goes to the host and is never asked back: the pane
    /// reads only whether one is kept.
    #[test]
    fn a_provider_key_is_sent_and_never_read_back() {
        en();
        let host = host();
        let mut providers = ProvidersModel::new(remote(&host));
        providers.poll();
        providers.ask_key("openai");
        providers.form_key.set("sk-live-secret".into());
        providers
            .apply(PersonSettingsEffect::SaveProviderKey {
                provider: "openai".into(),
            })
            .unwrap();

        let requests: Vec<InvokeRequest> = host.asked();
        let setting = requests
            .iter()
            .find(|request| request.target.address.action == person_models::SET_KEY)
            .expect("the page asked the host to keep the key");
        let InvokeArgument::Json(argument) = &setting.argument else {
            panic!("the key travels as JSON");
        };
        assert_eq!(argument["key"], serde_json::json!("sk-live-secret"));
        // The list asks for nothing: no key can travel back on it.
        for request in requests
            .iter()
            .filter(|request| request.target.address.action == person_models::LIST)
        {
            assert!(matches!(request.argument, InvokeArgument::None));
        }
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
