//! The person's accounts on a page: one place for every provider.
//!
//! The person picks who provides the account. Google, Microsoft and
//! Nextcloud sign in on their own page, which the server begins and
//! finishes itself; the account arrives in the list through the server's
//! live reading of the accounts, whichever tab the sign-in ended in.
//! iCloud takes an app-specific password. Any other provider takes an
//! address: the server finds its mail servers, shows them before a password
//! is typed for them, and only when it finds none does the page ask for
//! them.
//!
//! One account feeds every app: its Mail and Calendar switches say which.

use std::cell::Cell;

use ui::LocalState;
use ui::framework::ViewCx;
use ui::tilo_actions::InvocationUpdate;
use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::person_accounts::{
    self, BeginSignIn, CancelSignIn, FindServers, FoundServers, MailAccount, MailAccounts,
    Provider, Services, SetServices, SignInStep, SigningIn,
};
use ui::tilo_resource::Binds;
use ui::widget::{
    Button, ButtonRole, Checkbox, Element, ListRow, Row, Spacer, TextInput, TextInputKind, Toggle,
};
use ui::{Arrival, icons::Icon};

use super::{PersonSettingsEffect, json_argument, labelled_field, settled};
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args as translated};
use crate::resource::Settings;
use provider_settings_ui::widgets::{
    El, Item, SettingsSection, block, error_caption, error_row, note, page, row, rows, section,
    value_row,
};

/// Where Apple's page for app-specific passwords is.
const APPLE_ACCOUNT_PAGE: &str = "https://appleid.apple.com/account/manage";

/// The providers a person picks from, in the order they are shown.
const PROVIDERS: [Provider; 5] = [
    Provider::Google,
    Provider::Microsoft,
    Provider::Icloud,
    Provider::Nextcloud,
    Provider::Other,
];

/// The step of adding an account the page shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Adding {
    /// The providers to pick from.
    Choosing,
    /// One provider's own step.
    With(Provider),
    /// Another provider's servers, found from the address.
    Found(FoundServers),
    /// Another provider's servers, typed by the person.
    Manual,
}

/// The person's accounts, as their server keeps them, and the steps that
/// add one.
pub struct AccountsModel {
    remote: ui::RemoteActions,
    watching: Option<ui::Followed>,
    seen: Cell<u64>,
    listed: MailAccounts,
    /// Why the list could not be read or changed.
    error: Option<String>,
    /// What the last change did.
    notice: Option<String>,
    adding: Adding,
    /// Why the step shown failed.
    step_error: Option<String>,
    /// A request of the step shown, while it runs.
    asking: Option<(Asked, ui::InvocationCall)>,
    /// A change to the list (remove, services), while it runs.
    changing: Option<ui::InvocationCall>,
    mail: LocalState<bool>,
    calendar: LocalState<bool>,
    address: LocalState<String>,
    password: LocalState<String>,
    server: LocalState<String>,
    incoming: LocalState<String>,
    incoming_port: LocalState<String>,
    outgoing: LocalState<String>,
    outgoing_port: LocalState<String>,
    username: LocalState<String>,
}

/// What a step asked of the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Find,
    Add,
    /// A sign-in on this provider's page.
    SignIn(Provider),
}

impl AccountsModel {
    pub(super) fn new(remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            watching: None,
            seen: Cell::new(u64::MAX),
            listed: MailAccounts::default(),
            error: None,
            notice: None,
            adding: Adding::Choosing,
            step_error: None,
            asking: None,
            changing: None,
            mail: LocalState::new(true),
            calendar: LocalState::new(true),
            address: LocalState::new(String::new()),
            password: LocalState::new(String::new()),
            server: LocalState::new(String::new()),
            incoming: LocalState::new(String::new()),
            incoming_port: LocalState::new("993".into()),
            outgoing: LocalState::new(String::new()),
            outgoing_port: LocalState::new("465".into()),
            username: LocalState::new(String::new()),
        }
    }

    /// Whether a request is under way.
    pub(super) fn busy(&self) -> bool {
        self.asking.is_some() || self.changing.is_some()
    }

    /// What the person chose to use the account for, within what its
    /// provider offers.
    fn services(&self, provider: Provider) -> Services {
        Services {
            mail: self.mail.get(),
            calendar: self.calendar.get(),
            files: false,
        }
        .within(provider)
    }

    fn ask(&mut self, asked: Asked, action: &str, argument: InvokeArgument) {
        self.step_error = None;
        self.notice = None;
        let call = self
            .remote
            .invoke(person_accounts::target(action), argument, None);
        self.asking = Some((asked, call));
    }

    fn change(&mut self, action: &str, argument: InvokeArgument) {
        self.error = None;
        self.notice = None;
        self.changing = Some(
            self.remote
                .invoke(person_accounts::target(action), argument, None),
        );
    }

    fn back_to_choosing(&mut self) {
        self.adding = Adding::Choosing;
        self.step_error = None;
        self.password.set(String::new());
        self.server.set(String::new());
        self.incoming.set(String::new());
        self.outgoing.set(String::new());
        self.incoming_port.set("993".into());
        self.outgoing_port.set("465".into());
        self.username.set(String::new());
    }

    pub(super) fn apply(&mut self, effect: PersonSettingsEffect) -> Result<(), String> {
        // A reading that could not reach the server is asked for again
        // whenever the person does something here.
        if self
            .watching
            .as_ref()
            .is_some_and(|watching| watching.call().with(ui::Arrival::is_finished))
        {
            self.watching = None;
            self.seen.set(u64::MAX);
        }
        match effect {
            PersonSettingsEffect::ChooseProvider { provider } => {
                let provider = Provider::from_key(&provider)
                    .ok_or_else(|| t("settings-web-accounts-unknown-provider"))?;
                self.back_to_choosing();
                self.mail.set(true);
                self.calendar.set(true);
                self.adding = Adding::With(provider);
            }
            PersonSettingsEffect::CancelAdding => self.back_to_choosing(),
            PersonSettingsEffect::SetUpManually => {
                self.step_error = None;
                if let Adding::Found(found) = &self.adding {
                    self.incoming.set(found.incoming.name.clone());
                    self.incoming_port.set(found.incoming.port.to_string());
                    self.outgoing.set(found.outgoing.name.clone());
                    self.outgoing_port.set(found.outgoing.port.to_string());
                    self.username
                        .set(found.username.clone().unwrap_or_default());
                }
                self.adding = Adding::Manual;
            }
            PersonSettingsEffect::SignIn => {
                let provider = match &self.adding {
                    Adding::With(provider) => *provider,
                    Adding::Found(found) => found.provider,
                    Adding::Choosing | Adding::Manual => {
                        return Err(t("settings-web-accounts-unknown-provider"));
                    }
                };
                let server = if provider == Provider::Nextcloud {
                    self.server.get().trim().to_owned()
                } else {
                    String::new()
                };
                let begin = BeginSignIn {
                    provider,
                    server,
                    services: self.services(provider),
                    account: String::new(),
                };
                self.ask(
                    Asked::SignIn(provider),
                    person_accounts::BEGIN_SIGN_IN,
                    json_argument(&begin)?,
                );
            }
            PersonSettingsEffect::SignInAgain { account } => self.sign_in_again(&account)?,
            PersonSettingsEffect::FindServers => {
                let find = FindServers {
                    address: self.address.get().trim().to_owned(),
                };
                self.ask(
                    Asked::Find,
                    person_accounts::FIND_SERVERS,
                    json_argument(&find)?,
                );
            }
            PersonSettingsEffect::AddMailAccount => {
                let adding = self.account_to_add()?;
                self.ask(
                    Asked::Add,
                    person_accounts::ADD_MAIL,
                    json_argument(&adding)?,
                );
            }
            PersonSettingsEffect::CancelSignIn { sign_in } => {
                self.change(
                    person_accounts::CANCEL_SIGN_IN,
                    json_argument(&CancelSignIn { sign_in })?,
                );
            }
            PersonSettingsEffect::SetService {
                account,
                service,
                on,
            } => self.set_service(account, &service, on)?,
            PersonSettingsEffect::RemoveMailAccount { account } => {
                self.change(
                    person_accounts::REMOVE,
                    json_argument(&serde_json::json!({ "account": account }))?,
                );
            }
        }
        Ok(())
    }

    /// Sign in again to a kept account whose sign-in stopped working, on its
    /// provider's page, for what it already feeds.
    fn sign_in_again(&mut self, account: &str) -> Result<(), String> {
        let kept = self
            .listed
            .accounts
            .iter()
            .find(|kept| kept.id == account)
            .ok_or_else(|| t("settings-web-accounts-unknown-account"))?;
        if !signs_in_again(kept.provider) {
            return Err(t("settings-web-accounts-unknown-provider"));
        }
        let begin = BeginSignIn {
            provider: kept.provider,
            server: String::new(),
            services: kept.services,
            account: kept.id.clone(),
        };
        let provider = kept.provider;
        self.ask(
            Asked::SignIn(provider),
            person_accounts::BEGIN_SIGN_IN,
            json_argument(&begin)?,
        );
        Ok(())
    }

    /// Turn `service` on or off for `account`, keeping what else it feeds.
    fn set_service(&mut self, account: String, service: &str, on: bool) -> Result<(), String> {
        let Some(kept) = self.listed.accounts.iter().find(|kept| kept.id == account) else {
            return Err(t("settings-web-accounts-unknown-account"));
        };
        let mut services = kept.services;
        match service {
            "mail" => services.mail = on,
            "calendar" => services.calendar = on,
            _ => return Err(t("settings-web-accounts-unknown-service")),
        }
        self.change(
            person_accounts::SET_SERVICES,
            json_argument(&SetServices { account, services })?,
        );
        Ok(())
    }

    /// The account the step shown adds: iCloud's with its app password,
    /// another provider's with the servers found or typed.
    fn account_to_add(&self) -> Result<person_accounts::AddMailAccount, String> {
        let address = self.address.get().trim().to_owned();
        let (provider, incoming, outgoing, username) = match &self.adding {
            Adding::With(Provider::Icloud) => (
                Provider::Icloud,
                (String::new(), 993),
                (String::new(), 587),
                None,
            ),
            Adding::Found(found) => (
                found.provider,
                (found.incoming.name.clone(), found.incoming.port),
                (found.outgoing.name.clone(), found.outgoing.port),
                found.username.clone(),
            ),
            Adding::Manual => (
                Provider::Other,
                (
                    self.incoming.get().trim().to_owned(),
                    port(&self.incoming_port.get(), 993),
                ),
                (
                    self.outgoing.get().trim().to_owned(),
                    port(&self.outgoing_port.get(), 465),
                ),
                Some(self.username.get().trim().to_owned()).filter(|name| !name.is_empty()),
            ),
            Adding::Choosing | Adding::With(_) => {
                return Err(t("settings-web-accounts-unknown-provider"));
            }
        };
        Ok(person_accounts::AddMailAccount {
            address,
            display_name: String::new(),
            username,
            incoming_host: incoming.0,
            incoming_port: incoming.1,
            outgoing_host: outgoing.0,
            outgoing_port: outgoing.1,
            password: self.password.get(),
            identities: Vec::new(),
            services: self.services(provider),
        })
    }

    /// Take what the server answered since the last pass. Whether the page
    /// changed.
    pub(super) fn poll(&mut self) -> bool {
        // A server that answers a held ask at once is looked at again on the
        // client's interval, which only its owner's pass drives.
        self.remote.poll();
        let mut changed = self.read_watched();
        if let Some((asked, call)) = &self.asking
            && let Some(outcome) = settled(call)
        {
            let asked = *asked;
            self.asking = None;
            changed = true;
            match outcome {
                Ok(value) => self.asked(asked, value),
                Err(reason) => {
                    self.step_error = Some(reason);
                    // A lookup that found nothing asks for the servers.
                    if asked == Asked::Find {
                        self.incoming.set(String::new());
                        self.outgoing.set(String::new());
                        self.adding = Adding::Manual;
                    }
                }
            }
        }
        if let Some(call) = &self.changing
            && let Some(outcome) = settled(call)
        {
            self.changing = None;
            changed = true;
            match outcome.and_then(|value| {
                serde_json::from_value::<MailAccounts>(value)
                    .map_err(|_| t("settings-web-accounts-unreadable"))
            }) {
                Ok(listed) => {
                    self.listed = listed;
                    self.error = None;
                }
                Err(reason) => self.error = Some(reason),
            }
        }
        changed
    }

    /// What a step's request answered.
    fn asked(&mut self, asked: Asked, value: serde_json::Value) {
        match asked {
            Asked::Find => match serde_json::from_value::<FoundServers>(value) {
                Ok(found) => self.adding = Adding::Found(found),
                Err(_) => self.step_error = Some(t("settings-web-accounts-unreadable")),
            },
            Asked::Add => match serde_json::from_value::<MailAccount>(value) {
                Ok(account) => {
                    self.notice = Some(translated(
                        "settings-web-accounts-added",
                        &[("name", account.address.into())],
                    ));
                    self.address.set(String::new());
                    self.back_to_choosing();
                }
                Err(_) => self.step_error = Some(t("settings-web-accounts-unreadable")),
            },
            Asked::SignIn(_) => match serde_json::from_value::<SignInStep>(value) {
                Ok(step) => {
                    ui::widget::open_external_url(&step.open);
                    self.back_to_choosing();
                }
                Err(_) => self.step_error = Some(t("settings-web-accounts-unreadable")),
            },
        }
    }

    /// The server's live reading of the accounts: an account a sign-in in
    /// another tab finished arrives here when it is kept.
    fn read_watched(&mut self) -> bool {
        if self.watching.is_none() {
            self.watching = Some(self.remote.follow(
                person_accounts::target(person_accounts::WATCH),
                InvokeArgument::None,
            ));
        }
        let Some(watching) = &self.watching else {
            return false;
        };
        let call = watching.call();
        let revision = call.revision();
        if revision == self.seen.get() {
            return false;
        }
        self.seen.set(revision);
        match call.get() {
            Arrival::Ready(InvocationUpdate::Item { value, .. }) => {
                match serde_json::from_value::<MailAccounts>(value) {
                    Ok(listed) => {
                        self.listed = listed;
                        self.error = None;
                    }
                    Err(_) => self.error = Some(t("settings-web-accounts-unreadable")),
                }
            }
            // A provider that answers the reading once, rather than
            // following it.
            Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
                match output.value.map(serde_json::from_value::<MailAccounts>) {
                    Some(Ok(listed)) => {
                        self.listed = listed;
                        self.error = None;
                    }
                    _ => self.error = Some(t("settings-web-accounts-unreadable")),
                }
            }
            Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                self.error = Some(error.message);
            }
            Arrival::Failed(error) => self.error = Some(super::unreached(&error)),
            _ => {}
        }
        true
    }

    pub(super) fn page(&self, cx: &ViewCx<Settings>) -> Element {
        let mut sections = Vec::new();
        if let Some(list) = self.account_list(cx) {
            sections.push(list);
        }
        sections.push(self.add_section(cx));
        page(sections)
    }

    fn account_list(&self, cx: &ViewCx<Settings>) -> Option<SettingsSection> {
        let mut items = Vec::new();
        for account in &self.listed.accounts {
            items.extend(account_rows(cx, account));
        }
        for signing in &self.listed.signing_in {
            items.push(signing_row(cx, signing));
        }
        if let Some(line) = &self.error {
            items.push(error_row(&t("settings-web-accounts-list"), line));
        } else if let Some(line) = &self.notice {
            items.push(note(line));
        }
        (!items.is_empty()).then(|| section(&t("settings-web-accounts-list"), rows(items)))
    }

    fn add_section(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let mut items = match &self.adding {
            Adding::Choosing => self.provider_rows(cx),
            Adding::With(provider @ (Provider::Google | Provider::Microsoft)) => {
                let mut items = self.service_choice(*provider);
                items.push(Self::buttons(cx, Some(sign_in_button(cx, *provider))));
                items
            }
            Adding::With(Provider::Nextcloud) => vec![
                labelled_field(
                    &t("settings-web-accounts-server"),
                    TextInput::from_state(&self.server)
                        .with_kind(TextInputKind::Url)
                        .with_id("settings.web.accounts.server"),
                ),
                Self::buttons(cx, Some(sign_in_button(cx, Provider::Nextcloud))),
            ],
            Adding::With(Provider::Icloud) => self.icloud_step(cx),
            Adding::With(Provider::Other) => vec![
                self.address_field(),
                Self::buttons(
                    cx,
                    Some(bind(
                        cx,
                        Button::new(
                            "settings.web.accounts.find",
                            &t("settings-web-accounts-set-up"),
                        )
                        .role(ButtonRole::Primary),
                        PersonSettingsEffect::FindServers,
                    )),
                ),
            ],
            Adding::Found(found) => self.found_step(cx, found),
            Adding::Manual => self.manual_step(cx),
        };
        if let Some((asked, _)) = &self.asking {
            items.push(note(&Self::asking_note(*asked)));
        } else if let Some(line) = &self.step_error {
            // Above the step's buttons, where the person who pressed one is
            // looking: a long step's last rows are below what the window shows.
            let at = if matches!(self.adding, Adding::Choosing) {
                items.len()
            } else {
                items.len().saturating_sub(1)
            };
            items.insert(at, block(error_caption(line)));
        }
        section(&t("settings-web-accounts-add-title"), rows(items))
    }

    /// What the step shows while its request is under way.
    fn asking_note(asked: Asked) -> String {
        match asked {
            Asked::Find => t("settings-web-accounts-finding"),
            Asked::Add => t("settings-web-accounts-checking"),
            Asked::SignIn(provider) => translated(
                "settings-web-accounts-opening",
                &[("provider", provider_name(provider).into())],
            ),
        }
    }

    fn provider_rows(&self, cx: &ViewCx<Settings>) -> Vec<Item> {
        PROVIDERS
            .into_iter()
            .filter(|provider| {
                !matches!(provider, Provider::Google | Provider::Microsoft)
                    || self.listed.signs_in_with.contains(provider)
            })
            .map(|provider| {
                let choose = ListRow::new(
                    format!("settings.web.accounts.provider.{}", provider.key()),
                    provider_name(provider),
                )
                .avatar(provider_name(provider), provider.key())
                .avatar_glyph(provider_icon(provider))
                .square()
                .secondary(t(&format!(
                    "settings-web-accounts-provider-{}-offers",
                    provider.key()
                )));
                row(Box::new(bind(
                    cx,
                    choose,
                    PersonSettingsEffect::ChooseProvider {
                        provider: provider.key().to_owned(),
                    },
                )))
            })
            .collect()
    }

    /// The services an account of `provider` can feed, to choose from, when
    /// it offers more than one.
    fn service_choice(&self, provider: Provider) -> Vec<Item> {
        let offered = provider.offers();
        if [offered.mail, offered.calendar]
            .iter()
            .filter(|on| **on)
            .count()
            < 2
        {
            return Vec::new();
        }
        vec![
            row(Box::new(
                Checkbox::from_state(&self.mail).with_label(t("settings-web-accounts-mail")),
            )),
            row(Box::new(
                Checkbox::from_state(&self.calendar)
                    .with_label(t("settings-web-accounts-calendar")),
            )),
        ]
    }

    fn address_field(&self) -> Item {
        labelled_field(
            &t("accounts-email"),
            TextInput::from_state(&self.address)
                .with_kind(TextInputKind::Email)
                .with_id("settings.web.accounts.address"),
        )
    }

    fn password_field(&self, label: &str) -> Item {
        labelled_field(
            label,
            TextInput::from_state(&self.password)
                .with_kind(TextInputKind::Password)
                .with_secret(true)
                .with_id("settings.web.accounts.password"),
        )
    }

    fn icloud_step(&self, cx: &ViewCx<Settings>) -> Vec<Item> {
        let open_apple = Button::new(
            "settings.web.accounts.apple-page",
            &t("settings-web-accounts-icloud-open"),
        )
        .role(ButtonRole::Secondary)
        .on_open(|| ui::widget::open_external_url(APPLE_ACCOUNT_PAGE));
        let mut items = vec![
            note(&t("settings-web-accounts-icloud-how")),
            row(Box::new(
                Row::new()
                    .push(Box::new(open_apple))
                    .push(Box::new(Spacer::new())),
            )),
            labelled_field(
                &t("settings-web-accounts-icloud-address"),
                TextInput::from_state(&self.address)
                    .with_kind(TextInputKind::Email)
                    .with_id("settings.web.accounts.address"),
            ),
            self.password_field(&t("settings-web-accounts-icloud-password")),
        ];
        items.extend(self.service_choice(Provider::Icloud));
        items.push(Self::buttons(cx, Some(add_button(cx))));
        items
    }

    fn found_step(&self, cx: &ViewCx<Settings>, found: &FoundServers) -> Vec<Item> {
        let address = self.address.get();
        if found.provider.signs_in_on_its_page() {
            let mut items = vec![note(&translated(
                "settings-web-accounts-found-provider",
                &[
                    ("address", address.into()),
                    ("provider", provider_name(found.provider).into()),
                ],
            ))];
            items.extend(self.service_choice(found.provider));
            items.push(Self::buttons(cx, Some(sign_in_button(cx, found.provider))));
            return items;
        }
        let mut items = vec![
            value_row(&t("accounts-email"), &address),
            value_row(
                &t("settings-web-accounts-incoming"),
                &server_at(&found.incoming.name, found.incoming.port),
            ),
            value_row(
                &t("settings-web-accounts-outgoing"),
                &server_at(&found.outgoing.name, found.outgoing.port),
            ),
            self.password_field(&t("accounts-password")),
        ];
        items.extend(self.service_choice(found.provider));
        let manual = bind(
            cx,
            Button::new(
                "settings.web.accounts.manual",
                &t("settings-web-accounts-manual"),
            )
            .role(ButtonRole::Secondary),
            PersonSettingsEffect::SetUpManually,
        );
        items.push(row(Box::new(
            Row::new()
                .push(Box::new(add_button(cx)))
                .push(Box::new(manual))
                .push(Box::new(cancel_button(cx)))
                .push(Box::new(Spacer::new())),
        )));
        items
    }

    fn manual_step(&self, cx: &ViewCx<Settings>) -> Vec<Item> {
        vec![
            self.address_field(),
            labelled_field(
                &t("accounts-username"),
                TextInput::from_state(&self.username)
                    .with_kind(TextInputKind::Name)
                    .with_id("settings.web.accounts.username"),
            ),
            labelled_field(
                &t("settings-web-accounts-incoming"),
                TextInput::from_state(&self.incoming)
                    .with_kind(TextInputKind::Url)
                    .with_id("settings.web.accounts.incoming"),
            ),
            labelled_field(
                &t("settings-web-accounts-incoming-port"),
                TextInput::from_state(&self.incoming_port)
                    .with_kind(TextInputKind::Number)
                    .with_id("settings.web.accounts.incoming-port"),
            ),
            labelled_field(
                &t("settings-web-accounts-outgoing"),
                TextInput::from_state(&self.outgoing)
                    .with_kind(TextInputKind::Url)
                    .with_id("settings.web.accounts.outgoing"),
            ),
            labelled_field(
                &t("settings-web-accounts-outgoing-port"),
                TextInput::from_state(&self.outgoing_port)
                    .with_kind(TextInputKind::Number)
                    .with_id("settings.web.accounts.outgoing-port"),
            ),
            self.password_field(&t("accounts-password")),
            Self::buttons(cx, Some(add_button(cx))),
        ]
    }

    /// The step's own button, then Cancel.
    fn buttons(cx: &ViewCx<Settings>, primary: Option<Button>) -> Item {
        let mut buttons = Row::new();
        if let Some(primary) = primary {
            buttons = buttons.push(Box::new(primary));
        }
        row(Box::new(
            buttons
                .push(Box::new(cancel_button(cx)))
                .push(Box::new(Spacer::new())),
        ))
    }
}

/// An account's row and its switches.
fn account_rows(cx: &ViewCx<Settings>, account: &MailAccount) -> Vec<Item> {
    let title = if account.address.is_empty() {
        account.server.clone()
    } else {
        account.address.clone()
    };
    let detail = if account.provider == Provider::Nextcloud || account.has_secret {
        provider_name(account.provider)
    } else {
        t("settings-web-accounts-no-secret")
    };
    let remove = bind(
        cx,
        Button::new(
            &format!("settings.web.accounts.remove.{}", account.id),
            &t("settings-web-accounts-remove"),
        ),
        PersonSettingsEffect::RemoveMailAccount {
            account: account.id.clone(),
        },
    );
    let trailing: El = if account.has_secret || !signs_in_again(account.provider) {
        Box::new(remove)
    } else {
        let again = bind(
            cx,
            Button::new(
                &format!("settings.web.accounts.sign-in-again.{}", account.id),
                &t("settings-web-accounts-sign-in-again"),
            )
            .role(ButtonRole::Primary),
            PersonSettingsEffect::SignInAgain {
                account: account.id.clone(),
            },
        );
        Box::new(Row::new().push(Box::new(again)).push(Box::new(remove)))
    };
    let mut items = vec![row(Box::new(
        ListRow::new(format!("settings.web.accounts.{}", account.id), title)
            .avatar(provider_name(account.provider), account.provider.key())
            .avatar_glyph(provider_icon(account.provider))
            .square()
            .secondary(detail)
            .trailing(trailing),
    ))];
    let offered = account.provider.offers();
    for (service, offers, on, label) in [
        (
            "mail",
            offered.mail,
            account.services.mail,
            "settings-web-accounts-mail",
        ),
        (
            "calendar",
            offered.calendar,
            account.services.calendar,
            "settings-web-accounts-calendar",
        ),
    ] {
        if !offers {
            continue;
        }
        let toggle = Toggle::given(
            &format!("settings.web.accounts.{}.{service}", account.id),
            on,
        );
        let toggle = cx.actions().bind_effect(
            toggle,
            ServiceSwitch {
                account: account.id.clone(),
                service,
            },
        );
        items.push(provider_settings_ui::widgets::nested_setting_row(
            &t(label),
            None,
            Box::new(toggle),
        ));
    }
    items
}

/// Whether an account of `provider` signs in again on the provider's own
/// page; the others are kept with a password the person types.
fn signs_in_again(provider: Provider) -> bool {
    matches!(provider, Provider::Google | Provider::Microsoft)
}

/// A sign-in on a provider's page that has made no account yet.
fn signing_row(cx: &ViewCx<Settings>, signing: &SigningIn) -> Item {
    let provider = provider_name(signing.provider);
    let detail = signing.failed.clone().unwrap_or_else(|| {
        translated(
            "settings-web-accounts-signing-in",
            &[("provider", provider.clone().into())],
        )
    });
    let label = if signing.failed.is_some() {
        t("settings-web-accounts-dismiss")
    } else {
        t("accounts-cancel")
    };
    let stop = bind(
        cx,
        Button::new(
            &format!("settings.web.accounts.sign-in.{}", signing.id),
            &label,
        ),
        PersonSettingsEffect::CancelSignIn {
            sign_in: signing.id.clone(),
        },
    );
    row(Box::new(
        ListRow::new(
            format!("settings.web.accounts.sign-in.{}", signing.id),
            provider,
        )
        .avatar(provider_name(signing.provider), signing.provider.key())
        .avatar_glyph(provider_icon(signing.provider))
        .square()
        .secondary(detail)
        .trailing(Box::new(stop) as El),
    ))
}

/// One switch of an account's services, as the effect that sets it.
struct ServiceSwitch {
    account: String,
    service: &'static str,
}

impl Binds<SettingsEffect, bool> for ServiceSwitch {
    fn action_id(&self) -> &'static str {
        use ui::tilo_resource::Effect as _;
        PersonSettingsEffect::SetService {
            account: String::new(),
            service: String::new(),
            on: false,
        }
        .id()
    }

    fn arguments(&self, on: bool) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        use ui::tilo_resource::Effect as _;
        Ok(SettingsEffect::from(PersonSettingsEffect::SetService {
            account: self.account.clone(),
            service: self.service.to_owned(),
            on,
        })
        .arguments())
    }
}

fn bind<C: ui::framework::ActionControl<()>>(
    cx: &ViewCx<Settings>,
    control: C,
    effect: PersonSettingsEffect,
) -> C {
    cx.actions()
        .bind_effect_invocation(control, &SettingsEffect::from(effect))
}

fn sign_in_button(cx: &ViewCx<Settings>, provider: Provider) -> Button {
    bind(
        cx,
        Button::new(
            "settings.web.accounts.sign-in",
            &translated(
                "settings-web-accounts-sign-in-with",
                &[("provider", provider_name(provider).into())],
            ),
        )
        .role(ButtonRole::Primary),
        PersonSettingsEffect::SignIn,
    )
}

fn add_button(cx: &ViewCx<Settings>) -> Button {
    bind(
        cx,
        Button::new("settings.web.accounts.add", &t("settings-web-accounts-add"))
            .role(ButtonRole::Primary),
        PersonSettingsEffect::AddMailAccount,
    )
}

fn cancel_button(cx: &ViewCx<Settings>) -> Button {
    bind(
        cx,
        Button::new("settings.web.accounts.cancel", &t("accounts-cancel"))
            .role(ButtonRole::Secondary),
        PersonSettingsEffect::CancelAdding,
    )
}

/// The provider's name, as the person knows it.
fn provider_name(provider: Provider) -> String {
    t(&format!(
        "settings-web-accounts-provider-{}",
        provider.key()
    ))
}

fn provider_icon(provider: Provider) -> Icon {
    match provider {
        Provider::Google | Provider::Microsoft | Provider::Icloud => Icon::Cloud,
        Provider::Nextcloud => Icon::Folder,
        Provider::Other => Icon::Mail,
    }
}

/// A server and its port, as a value row shows them.
fn server_at(name: &str, port: u16) -> String {
    translated(
        "settings-web-accounts-server-at",
        &[("server", name.into()), ("port", port.to_string().into())],
    )
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
            provider: Provider::Google,
            services: Services {
                mail: true,
                calendar: true,
                files: false,
            },
            ..Default::default()
        }
    }

    fn found() -> FoundServers {
        FoundServers {
            provider: Provider::Other,
            incoming: person_accounts::ServerAt {
                name: "imap.example.test".into(),
                port: 993,
            },
            outgoing: person_accounts::ServerAt {
                name: "smtp.example.test".into(),
                port: 465,
            },
            username: None,
        }
    }

    /// A server that keeps one account, signs in to Google on its page, and
    /// finds `servers` for any address: an error when it has none.
    fn provider(servers: Option<FoundServers>) -> Arc<TestProvider> {
        Arc::new(TestProvider::new().answering(move |_, request| {
            let value = match request.target.address.action.as_str() {
                person_accounts::WATCH | person_accounts::SET_SERVICES => {
                    serde_json::to_value(MailAccounts {
                        accounts: vec![account()],
                        signs_in_with: vec![Provider::Google],
                        ..MailAccounts::default()
                    })
                }
                person_accounts::ADD_MAIL => serde_json::to_value(account()),
                person_accounts::BEGIN_SIGN_IN => serde_json::to_value(SignInStep {
                    id: "one".into(),
                    open: "https://accounts.example.test/sign-in".into(),
                }),
                person_accounts::FIND_SERVERS => match &servers {
                    Some(found) => serde_json::to_value(found),
                    None => {
                        return ui::tilo_actions::remote::Pending::ready(Ok(
                            ui::tilo_actions::remote::InvocationReceipt {
                                invocation: request.invocation,
                                status: ReceiptStatus::Failed {
                                    error: ui::tilo_actions::ActionFailure {
                                        code: "not-found".into(),
                                        message: "No mail servers were found for example.test."
                                            .into(),
                                        details: None,
                                        retryable: false,
                                    },
                                },
                            },
                        ));
                    }
                },
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

    fn model(provider: &Arc<TestProvider>) -> AccountsModel {
        let api: Arc<dyn ui::tilo_actions::remote::ActionProvider + Send + Sync> = provider.clone();
        AccountsModel::new(ui::RemoteActions::with_provider(
            api,
            Caller::local_person("owner"),
        ))
    }

    /// What the page asked the server with `action`, decoded.
    fn asked<T: serde::de::DeserializeOwned>(provider: &TestProvider, action: &str) -> T {
        let request = provider
            .asked()
            .into_iter()
            .rfind(|request| request.target.address.action == action)
            .unwrap_or_else(|| panic!("the page never asked {action}"));
        let InvokeArgument::Json(argument) = request.argument else {
            panic!("{action} travels as JSON");
        };
        serde_json::from_value(serde_json::Value::Object(argument)).unwrap()
    }

    /// The list is the server's live reading, and never carries a secret:
    /// the reading has no field for one.
    #[test]
    fn the_page_shows_what_the_server_keeps() {
        en();
        let provider = provider(None);
        let mut accounts = model(&provider);
        assert!(accounts.poll());
        assert_eq!(accounts.listed.accounts, vec![account()]);
    }

    /// Another provider's account takes the address first; the servers
    /// found for it are what the account is added with, and the password is
    /// asked for only then.
    #[test]
    fn an_address_finds_its_servers_before_a_password_is_asked() {
        en();
        let provider = provider(Some(found()));
        let mut accounts = model(&provider);
        accounts.poll();
        accounts
            .apply(PersonSettingsEffect::ChooseProvider {
                provider: "other".into(),
            })
            .unwrap();
        accounts.address.set(" ada@example.test ".into());
        accounts.apply(PersonSettingsEffect::FindServers).unwrap();
        let find: FindServers = asked(&provider, person_accounts::FIND_SERVERS);
        assert_eq!(find.address, "ada@example.test");
        accounts.poll();
        assert_eq!(accounts.adding, Adding::Found(found()));

        accounts.password.set("hunter2".into());
        accounts
            .apply(PersonSettingsEffect::AddMailAccount)
            .unwrap();
        let sent: person_accounts::AddMailAccount = asked(&provider, person_accounts::ADD_MAIL);
        assert_eq!(sent.address, "ada@example.test");
        assert_eq!(
            (sent.incoming_host.as_str(), sent.outgoing_host.as_str()),
            ("imap.example.test", "smtp.example.test")
        );
        assert_eq!(sent.password, "hunter2");
        assert!(sent.services.mail);

        accounts.poll();
        assert_eq!(accounts.adding, Adding::Choosing);
        assert_eq!(accounts.password.get(), "", "the password leaves the page");
        assert!(accounts.notice.is_some());
    }

    /// An address the server finds no servers for asks for them, and the
    /// reason shows in the person's words.
    #[test]
    fn no_servers_found_asks_for_them() {
        en();
        let provider = provider(None);
        let mut accounts = model(&provider);
        accounts
            .apply(PersonSettingsEffect::ChooseProvider {
                provider: "other".into(),
            })
            .unwrap();
        accounts.address.set("ada@example.test".into());
        accounts.apply(PersonSettingsEffect::FindServers).unwrap();
        accounts.poll();
        assert_eq!(accounts.adding, Adding::Manual);
        assert_eq!(
            accounts.step_error.as_deref(),
            Some("No mail servers were found for example.test.")
        );
    }

    /// Google signs in on its own page: the page asks the server to begin
    /// with what the person chose to use the account for, and nothing it
    /// sends carries a password.
    /// A cancelled step leaves nothing typed for the next: the servers, ports
    /// and password start over, so another account never inherits them.
    #[test]
    fn a_cancelled_step_starts_the_next_one_over() {
        en();
        let provider = provider(None);
        let mut accounts = model(&provider);
        accounts
            .apply(PersonSettingsEffect::ChooseProvider {
                provider: "other".into(),
            })
            .unwrap();
        accounts.incoming.set("imap.example.test".into());
        accounts.incoming_port.set("29124".into());
        accounts.outgoing_port.set("29125".into());
        accounts.password.set("secret".into());
        accounts.apply(PersonSettingsEffect::CancelAdding).unwrap();
        assert_eq!(accounts.incoming.get(), "");
        assert_eq!(accounts.incoming_port.get(), "993");
        assert_eq!(accounts.outgoing_port.get(), "465");
        assert_eq!(accounts.password.get(), "");
    }

    #[test]
    fn google_signs_in_on_its_own_page() {
        en();
        let provider = provider(None);
        let mut accounts = model(&provider);
        accounts.poll();
        accounts
            .apply(PersonSettingsEffect::ChooseProvider {
                provider: "google".into(),
            })
            .unwrap();
        accounts.calendar.set(false);
        accounts.apply(PersonSettingsEffect::SignIn).unwrap();
        let begin: BeginSignIn = asked(&provider, person_accounts::BEGIN_SIGN_IN);
        assert_eq!(begin.provider, Provider::Google);
        assert_eq!(
            begin.services,
            Services {
                mail: true,
                calendar: false,
                files: false,
            }
        );
        accounts.poll();
        assert_eq!(accounts.adding, Adding::Choosing);
    }

    /// A Google account whose sign-in stopped working signs in again to
    /// the same account, asking for what it already feeds; one kept with a
    /// password is not sent to a provider's page.
    #[test]
    fn a_stopped_sign_in_signs_in_again_to_the_same_account() {
        en();
        let provider = provider(None);
        let mut accounts = model(&provider);
        accounts.poll();
        let kept = &mut accounts.listed.accounts[0];
        kept.provider = Provider::Google;
        kept.has_secret = false;
        kept.services = Services {
            mail: true,
            calendar: false,
            files: false,
        };
        accounts
            .apply(PersonSettingsEffect::SignInAgain {
                account: "ada@example.test".into(),
            })
            .unwrap();
        let begin: BeginSignIn = asked(&provider, person_accounts::BEGIN_SIGN_IN);
        assert_eq!(begin.provider, Provider::Google);
        assert_eq!(begin.account, "ada@example.test");
        assert!(begin.services.mail && !begin.services.calendar);

        accounts.listed.accounts[0].provider = Provider::Other;
        assert!(
            accounts
                .apply(PersonSettingsEffect::SignInAgain {
                    account: "ada@example.test".into(),
                })
                .is_err()
        );
    }

    /// A server that follows the accounts without holding a held ask, so the
    /// page looks at the reading again on its interval: it answers what it
    /// keeps now, at once, every time.
    struct Unheld {
        listed: std::sync::Mutex<Vec<MailAccount>>,
    }

    impl ui::tilo_actions::remote::ActionProvider for Unheld {
        fn query(
            &self,
            _: &Caller,
            _: ui::tilo_actions::remote::CatalogueQuery,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::CataloguePage,
                ui::tilo_actions::remote::ActionCallError,
            >,
        > {
            ui::tilo_actions::remote::Pending::ready(Ok(
                ui::tilo_actions::remote::CataloguePage::default(),
            ))
        }

        fn invoke(
            &self,
            _: &Caller,
            request: ui::tilo_actions::remote::InvokeRequest,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::InvocationReceipt,
                ui::tilo_actions::remote::ActionCallError,
            >,
        > {
            ui::tilo_actions::remote::Pending::ready(Ok(
                ui::tilo_actions::remote::InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Accepted,
                },
            ))
        }

        fn observe(
            &self,
            _: &Caller,
            id: &ui::tilo_actions::InvocationId,
        ) -> Option<ui::tilo_actions::remote::InvocationState> {
            let accounts = self.listed.lock().unwrap().clone();
            Some(ui::tilo_actions::remote::InvocationState::new(
                *id,
                InvocationUpdate::Item {
                    invocation: *id,
                    sequence: accounts.len() as u64 + 1,
                    value: serde_json::to_value(MailAccounts {
                        accounts,
                        ..MailAccounts::default()
                    })
                    .unwrap(),
                },
            ))
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &ui::tilo_actions::InvocationId,
        ) -> ui::tilo_actions::remote::Pending<Result<(), ui::tilo_actions::remote::ActionCallError>>
        {
            ui::tilo_actions::remote::Pending::ready(Ok(()))
        }
    }

    /// An account kept by a sign-in that finished in another tab reaches the
    /// list though the server does not hold the page's held ask: the page
    /// looks again on its own.
    #[test]
    fn an_account_kept_elsewhere_shows_from_a_server_that_does_not_hold() {
        en();
        let clock = ui::background::Pumped::new();
        let _entered = clock.background().enter();
        let server = Arc::new(Unheld {
            listed: std::sync::Mutex::new(Vec::new()),
        });
        let api: Arc<dyn ui::tilo_actions::remote::ActionProvider + Send + Sync> = server.clone();
        let mut accounts = AccountsModel::new(ui::RemoteActions::with_provider(
            api,
            Caller::local_person("owner"),
        ));
        accounts.poll();
        assert!(accounts.listed.accounts.is_empty());

        server.listed.lock().unwrap().push(account());
        clock.advance(std::time::Duration::from_secs(1));
        accounts.poll();
        assert_eq!(accounts.listed.accounts, vec![account()]);
    }

    /// An account's switch sets what it feeds and keeps the rest.
    #[test]
    fn a_switch_turns_one_app_off() {
        en();
        let provider = provider(None);
        let mut accounts = model(&provider);
        accounts.poll();
        accounts
            .apply(PersonSettingsEffect::SetService {
                account: "ada@example.test".into(),
                service: "calendar".into(),
                on: false,
            })
            .unwrap();
        let set: SetServices = asked(&provider, person_accounts::SET_SERVICES);
        assert_eq!(set.account, "ada@example.test");
        assert!(set.services.mail);
        assert!(!set.services.calendar);
    }

    /// The server's own refusal reaches the person as it was written, and
    /// the step stays where they can fix what it says.
    #[test]
    fn a_refusal_is_shown_in_the_servers_words() {
        en();
        let provider = Arc::new(TestProvider::new().answering(|_, request| {
            if request.target.address.action == person_accounts::WATCH {
                return ui::tilo_actions::remote::Pending::ready(Ok(
                    ui::tilo_actions::remote::InvocationReceipt {
                        invocation: request.invocation,
                        status: ReceiptStatus::Completed {
                            output: ui::tilo_actions::ActionOutput {
                                value: Some(serde_json::to_value(MailAccounts::default()).unwrap()),
                                ..Default::default()
                            },
                        },
                    },
                ));
            }
            ui::tilo_actions::remote::Pending::ready(Err(
                ui::tilo_actions::remote::ActionCallError::Refused {
                    reason: "No mail servers were found for example.test.".into(),
                },
            ))
        }));
        let mut accounts = model(&provider);
        accounts.poll();
        accounts
            .apply(PersonSettingsEffect::ChooseProvider {
                provider: "other".into(),
            })
            .unwrap();
        accounts.address.set("ada@example.test".into());
        accounts.apply(PersonSettingsEffect::FindServers).unwrap();
        accounts.poll();
        assert_eq!(
            accounts.step_error.as_deref(),
            Some("No mail servers were found for example.test.")
        );
    }

    /// A request that cannot reach the server says so in the person's
    /// words, never the call layer's.
    #[test]
    fn an_unreachable_server_is_said_plainly() {
        en();
        let said =
            super::super::unreached(&ui::tilo_actions::remote::ActionCallError::Unavailable {
                reason: "Your devices can't be reached from here".into(),
            });
        assert!(!said.contains("remote actions"), "{said}");
        assert!(!said.contains("devices"), "{said}");
    }
}
