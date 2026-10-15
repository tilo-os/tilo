//! The account sheet: adding an account or changing one.
//!
//! A [`ModalDialog`] holding one [`Form`]. Every value the person types or
//! picks is local state the app reads when it saves, autofills or checks the
//! account, so the app never reaches into the widgets. The form is rebuilt
//! when its shape changes (password or browser sign-in, an error to show),
//! and the field that had the keyboard keeps it.
//!
//! Fields keep the indices agents address them with
//! (`mail.setup:field:{index}`): 0 display name, 1 email, 2 username,
//! 3 password, 4 incoming host, 5 incoming port, 6 outgoing host, 7 outgoing
//! port.

use std::cell::RefCell;
use std::rc::Rc;

use ui::icons::Icon;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Button, ButtonRole, Column, Container, Decorator, Density, Element, Field, Form, FormSection,
    ModalDialog, Row, SegmentedStyle, Text, TextInput, TextInputKind, Toggle, ToggleGroup, Tone,
    Widget,
};
use ui::{LocalState, TextRole};

use crate::model::{AccountConfig, AuthMethod};
use crate::oauth::OAuthProvider;

pub(crate) const DISPLAY_NAME: usize = 0;
pub(crate) const EMAIL: usize = 1;
pub(crate) const USERNAME: usize = 2;
pub(crate) const PASSWORD: usize = 3;
pub(crate) const IMAP_HOST: usize = 4;
pub(crate) const IMAP_PORT: usize = 5;
pub(crate) const SMTP_HOST: usize = 6;
pub(crate) const SMTP_PORT: usize = 7;
pub(crate) const FIELD_COUNT: usize = 8;

/// The id a field's input carries: its accessible key and focus key.
pub(crate) fn field_id(index: usize) -> String {
    format!("mail.setup:field:{index}")
}

/// The usual port of a server with TLS on or off.
pub(crate) fn default_port(server: Server, tls: bool) -> &'static str {
    match (server, tls) {
        (Server::Incoming, true) => "993",
        (Server::Incoming, false) => "143",
        (Server::Outgoing, true) => "465",
        (Server::Outgoing, false) => "587",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Server {
    Incoming,
    Outgoing,
}

/// What the person asked the sheet for. The app acts on it after the event
/// that raised it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupRequest {
    Save,
    Cancel,
    Remove,
    SignIn,
}

type Requests = Rc<RefCell<Vec<SetupRequest>>>;

/// What the address has told the sheet about its provider.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ProviderStatus {
    #[default]
    Idle,
    /// The servers are being looked up.
    Finding,
    /// The provider found for the address.
    Found { name: String },
}

/// What the sheet shows. Clones share the same values.
#[derive(Clone)]
pub(crate) struct SetupFields {
    values: Vec<LocalState<String>>,
    imap_tls: LocalState<bool>,
    smtp_tls: LocalState<bool>,
    auth: LocalState<AuthMethod>,
    import_pending: LocalState<bool>,
    oauth_source: LocalState<Option<AccountConfig>>,
    /// The browser sign-in provider the address belongs to, if any.
    provider: LocalState<Option<OAuthProvider>>,
    /// Why the last save, sign-in or discovery failed.
    error: LocalState<Option<String>>,
    /// The fields the last save found incomplete, keyed like
    /// [`AccountConfig::validate`].
    field_errors: LocalState<Vec<(&'static str, String)>>,
    /// Whether the server rows are shown. The servers are found from the
    /// address, so they stay out of the way until they need attention.
    advanced: LocalState<bool>,
    provider_status: LocalState<ProviderStatus>,
}

impl SetupFields {
    fn of(cfg: &AccountConfig) -> Self {
        let values = [
            cfg.display_name.clone(),
            cfg.email.clone(),
            cfg.username.clone(),
            cfg.password.clone(),
            cfg.imap_host.clone(),
            cfg.imap_port.to_string(),
            cfg.smtp_host.clone(),
            cfg.smtp_port.to_string(),
        ];
        Self {
            values: values.into_iter().map(LocalState::new).collect(),
            imap_tls: LocalState::new(cfg.imap_tls),
            smtp_tls: LocalState::new(cfg.smtp_tls),
            auth: LocalState::new(cfg.auth),
            import_pending: LocalState::new(cfg.import_pending),
            oauth_source: LocalState::new(None),
            provider: LocalState::new(
                cfg.email
                    .split_once('@')
                    .and_then(|(_, domain)| OAuthProvider::from_domain(domain)),
            ),
            error: LocalState::new(None),
            field_errors: LocalState::new(Vec::new()),
            advanced: LocalState::new(false),
            provider_status: LocalState::new(ProviderStatus::Idle),
        }
    }

    pub(crate) fn provider_status(&self) -> ProviderStatus {
        self.provider_status.with(Clone::clone)
    }

    pub(crate) fn set_provider_status(&self, status: ProviderStatus) {
        if self.provider_status.with(|current| *current != status) {
            self.provider_status.set(status);
        }
    }

    pub(crate) fn advanced(&self) -> bool {
        self.advanced.with(|open| *open)
    }

    pub(crate) fn set_advanced(&self, open: bool) {
        if self.advanced() != open {
            self.advanced.set(open);
        }
    }

    /// Field `index`'s text; empty past the last field.
    pub(crate) fn value(&self, index: usize) -> String {
        self.values
            .get(index)
            .map(|value| value.with(Clone::clone))
            .unwrap_or_default()
    }

    /// Set field `index`'s text; nothing past the last field.
    pub(crate) fn set_value(&self, index: usize, text: &str) {
        if let Some(value) = self.values.get(index)
            && value.with(|current| current != text)
        {
            value.set(text.to_owned());
        }
    }

    pub(crate) fn tls(&self, server: Server) -> bool {
        self.tls_state(server).with(|on| *on)
    }

    pub(crate) fn set_tls(&self, server: Server, on: bool) {
        self.tls_state(server).set(on);
    }

    fn tls_state(&self, server: Server) -> &LocalState<bool> {
        match server {
            Server::Incoming => &self.imap_tls,
            Server::Outgoing => &self.smtp_tls,
        }
    }

    pub(crate) fn auth(&self) -> AuthMethod {
        self.auth.with(|auth| *auth)
    }

    pub(crate) fn set_auth(&self, auth: AuthMethod) {
        if self.auth() != auth {
            self.auth.set(auth);
        }
    }

    pub(crate) fn provider(&self) -> Option<OAuthProvider> {
        OAuthProvider::for_account(&self.value(EMAIL), &self.value(IMAP_HOST))
    }

    pub(crate) fn set_provider(&self, provider: Option<OAuthProvider>) {
        if self.provider() != provider {
            self.provider.set(provider);
        }
    }

    pub(crate) fn error(&self) -> Option<String> {
        self.error.with(Clone::clone)
    }

    pub(crate) fn set_error(&self, error: Option<String>) {
        // A failure is where the servers may need correcting by hand.
        if error.is_some() {
            self.set_advanced(true);
        }
        if self.error.with(|current| *current != error) {
            self.error.set(error);
        }
    }

    pub(crate) fn field_errors(&self) -> Vec<(&'static str, String)> {
        self.field_errors.with(Clone::clone)
    }

    pub(crate) fn set_field_errors(&self, errors: Vec<(&'static str, String)>) {
        if errors.iter().any(|(key, _)| {
            key.starts_with("imap") || key.starts_with("smtp") || *key == "username"
        }) {
            self.set_advanced(true);
        }
        if self.field_errors.with(|current| *current != errors) {
            self.field_errors.set(errors);
        }
    }

    /// The account the fields describe. An account that signs in through
    /// the browser holds a sealed token, never a password, so its password
    /// stays empty and saving cannot overwrite the token.
    pub(crate) fn config(&self) -> AccountConfig {
        let auth = self.auth();
        AccountConfig {
            display_name: self.value(DISPLAY_NAME),
            email: self.value(EMAIL),
            username: self.value(USERNAME),
            password: if auth == AuthMethod::OAuth {
                String::new()
            } else {
                self.value(PASSWORD)
            },
            imap_host: self.value(IMAP_HOST),
            imap_port: self.value(IMAP_PORT).trim().parse().unwrap_or(993),
            smtp_host: self.value(SMTP_HOST),
            smtp_port: self.value(SMTP_PORT).trim().parse().unwrap_or(465),
            imap_tls: self.tls(Server::Incoming),
            smtp_tls: self.tls(Server::Outgoing),
            auth,
            import_pending: self.import_pending.with(|pending| *pending)
                && (auth == AuthMethod::OAuth
                    || self.provider().is_some()
                    || self.value(PASSWORD).trim().is_empty()),
        }
    }

    /// Fence the secure handoff to the account configuration the person reviewed.
    pub(crate) fn expect_oauth(&self) {
        self.oauth_source.set(Some(self.config()));
    }

    pub(crate) fn complete_oauth(&self, account: &str) {
        let current = self.config();
        let matches = self
            .oauth_source
            .with(|source| source.as_ref() == Some(&current) && current.email.trim() == account);
        if matches {
            self.import_pending.set(false);
            self.oauth_source.set(None);
        }
    }

    /// The label of the browser sign-in button for the address's provider.
    pub(crate) fn sign_in_label(&self) -> String {
        let provider = self.provider().map_or_else(
            || crate::i18n::t("mail-setup-oauth-provider-fallback"),
            |provider| provider.label().to_string(),
        );
        crate::i18n::t_args(
            "mail-setup-oauth-sign-in",
            &[("provider", ui::i18n::ArgValue::from(provider))],
        )
    }
}

/// Everything that changes which widgets the form holds.
#[derive(Clone, PartialEq)]
struct Shape {
    auth: AuthMethod,
    provider: Option<OAuthProvider>,
    error: Option<String>,
    field_errors: Vec<(&'static str, String)>,
    advanced: bool,
    provider_status: ProviderStatus,
}

impl Shape {
    fn of(fields: &SetupFields) -> Self {
        Self {
            auth: fields.auth(),
            provider: fields.provider(),
            error: fields.error(),
            field_errors: fields.field_errors(),
            advanced: fields.advanced(),
            provider_status: fields.provider_status(),
        }
    }

    /// The messages for the validation keys `keys`, joined.
    fn error_for(&self, keys: &[&str]) -> Option<String> {
        let messages: Vec<&str> = self
            .field_errors
            .iter()
            .filter(|(key, _)| keys.contains(key))
            .map(|(_, message)| message.as_str())
            .collect();
        (!messages.is_empty()).then(|| messages.join(" "))
    }
}

fn with_error(field: Field, error: Option<String>) -> Field {
    match error {
        Some(message) => field.with_error(message),
        None => field,
    }
}

fn input(fields: &SetupFields, index: usize) -> TextInput {
    let input = fields
        .values
        .get(index)
        .map_or_else(TextInput::local, TextInput::from_state)
        .with_id(&field_id(index));
    match index {
        DISPLAY_NAME => input
            .with_kind(TextInputKind::Name)
            .with_placeholder("Your name"),
        EMAIL => input
            .with_kind(TextInputKind::Email)
            .with_placeholder("you@example.com"),
        USERNAME => input
            .with_kind(TextInputKind::Username)
            .with_placeholder("Usually your email"),
        PASSWORD => input
            .with_kind(TextInputKind::Password)
            .with_secret(true)
            .with_placeholder("Account password"),
        IMAP_HOST => input
            .with_kind(TextInputKind::Url)
            .with_placeholder("imap.example.com"),
        SMTP_HOST => input
            .with_kind(TextInputKind::Url)
            .with_placeholder("smtp.example.com"),
        _ => input
            .with_kind(TextInputKind::Number)
            .with_char_filter(|c| c.is_ascii_digit()),
    }
}

/// How the account signs in: a password, or the provider's page in the
/// browser.
fn sign_in_choice(fields: &SetupFields) -> ToggleGroup {
    let chosen = fields.clone();
    ToggleGroup::new(
        vec![
            Segment::new(crate::i18n::t("mail-setup-auth-password")),
            Segment::new(crate::i18n::t("mail-setup-auth-oauth")),
        ],
        usize::from(fields.auth() == AuthMethod::OAuth),
    )
    .with_style(SegmentedStyle::Segmented)
    .with_id("mail.setup:auth")
    .on_change(move |index| {
        chosen.set_auth(if index == 1 {
            AuthMethod::OAuth
        } else {
            AuthMethod::Password
        });
    })
}

/// The browser sign-in button and what it does.
fn browser_sign_in(fields: &SetupFields, requests: &Requests) -> Element {
    let requests = Rc::clone(requests);
    let button = Button::new("mail.setup:oauth-sign-in", &fields.sign_in_label())
        .role(ButtonRole::Secondary)
        .on_effect(move || requests.borrow_mut().push(SetupRequest::SignIn));
    let available = fields.provider().is_some_and(OAuthProvider::is_configured);
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Row::new().push(Box::new(button))));
    if !available {
        column = column.push(Box::new(
            Text::new(&crate::i18n::t("mail-setup-oauth-note-unavailable"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    }
    Box::new(column)
}

/// A server's host, port and TLS switch on one row. Changing TLS moves the
/// port to the usual one for it.
fn server_row(fields: &SetupFields, server: Server) -> Element {
    let (host, port, name) = match server {
        Server::Incoming => (IMAP_HOST, IMAP_PORT, "IMAP"),
        Server::Outgoing => (SMTP_HOST, SMTP_PORT, "SMTP"),
    };
    let switched = fields.clone();
    let tls = Toggle::from_state(fields.tls_state(server))
        .with_id(match server {
            Server::Incoming => "mail.setup:imap-tls",
            Server::Outgoing => "mail.setup:smtp-tls",
        })
        .with_accessible_name(&format!("{name} TLS"))
        .on_change(move |on| switched.set_value(port, default_port(server, on)));
    Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(
                input(fields, host).with_accessible_name(format!("{name} server")),
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    // Room for a port's five digits and the field's insets.
                    .with_width(ui::tokens::Spacing::BASE.s16 + ui::tokens::Spacing::BASE.s3)
                    .with_child(Box::new(
                        input(fields, port).with_accessible_name(format!("{name} port")),
                    )),
            ))
            .push(Box::new(tls))
            .push(Box::new(
                Text::new(&crate::i18n::t("mail-setup-tls")).with_role(TextRole::Label),
            )),
    )
}

/// Why the last save, sign-in or lookup failed; nothing while all is well.
fn status_line(message: &str) -> Element {
    Box::new(
        Text::new(message)
            .with_role(TextRole::Caption)
            .tone(Tone::Danger)
            .with_live(ui::a11y::Politeness::Polite),
    )
}

/// The address with, under it, what it has told the sheet: the provider
/// found, or that its servers are being looked up.
fn address_input(fields: &SetupFields, status: &ProviderStatus) -> Element {
    let address =
        input(fields, EMAIL).with_accessible_name(crate::i18n::t("mail-setup-label-email"));
    let note = match status {
        ProviderStatus::Idle => return Box::new(address),
        ProviderStatus::Finding => crate::i18n::t("mail-setup-finding-provider"),
        ProviderStatus::Found { name } => name.clone(),
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(address))
            .push(Box::new(
                Text::new(&note).with_role(TextRole::Caption).dimmed(),
            )),
    )
}

/// The switch that shows or hides the server rows.
fn advanced_switch(fields: &SetupFields, open: bool) -> Element {
    let fields = fields.clone();
    Box::new(
        Row::new().push(Box::new(
            Button::new(
                "mail.setup:advanced",
                &crate::i18n::t("mail-setup-advanced"),
            )
            .role(ButtonRole::Plain)
            .with_icon(if open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
            .on_effect(move || fields.set_advanced(!fields.advanced())),
        )),
    )
}

fn form(fields: &SetupFields, requests: &Requests, editing: bool, shape: &Shape) -> Element {
    let label = crate::i18n::t;
    // What a person gives: the address and how to sign in. The servers are
    // found from the address.
    let email = Field::custom(
        label("mail-setup-label-email"),
        address_input(fields, &shape.provider_status),
    );
    let mut account = FormSection::untitled().field(with_error(email, shape.error_for(&["email"])));
    account = match shape.auth {
        AuthMethod::Password => account.field(Field::new(
            label("mail-setup-label-password"),
            input(fields, PASSWORD),
        )),
        AuthMethod::OAuth => account.field(Field::custom(
            label("mail-setup-label-account"),
            browser_sign_in(fields, requests),
        )),
    };
    account = account.control_row(advanced_switch(fields, shape.advanced));
    let servers = FormSection::untitled()
        .field(Field::new(
            label("mail-setup-label-display-name"),
            input(fields, DISPLAY_NAME),
        ))
        .field(with_error(
            Field::new(label("mail-setup-label-username"), input(fields, USERNAME)),
            shape.error_for(&["username"]),
        ))
        .field(Field::new(
            label("mail-setup-label-sign-in"),
            sign_in_choice(fields),
        ))
        .field(with_error(
            Field::custom(
                label("mail-setup-label-imap-host"),
                server_row(fields, Server::Incoming),
            ),
            shape.error_for(&["imap_host", "imap_port"]),
        ))
        .field(with_error(
            Field::custom(
                label("mail-setup-label-smtp-host"),
                server_row(fields, Server::Outgoing),
            ),
            shape.error_for(&["smtp_host", "smtp_port"]),
        ));
    let mut form = Form::new()
        .with_density(Some(Density::Compact))
        .section(account);
    if shape.advanced {
        form = form.section(servers);
    }
    if let Some(message) = &shape.error {
        form = form.section(FormSection::untitled().control_row(status_line(message)));
    }
    if editing {
        let requests = Rc::clone(requests);
        form = form.section(
            FormSection::untitled().control_row(Box::new(
                Row::new().push(Box::new(
                    Button::new("mail.setup:remove", &label("mail-setup-remove"))
                        .role(ButtonRole::Destructive)
                        .on_effect(move || requests.borrow_mut().push(SetupRequest::Remove)),
                )),
            )),
        );
    }
    Box::new(form)
}

/// The id of the focused widget under `widget`, if it has one.
fn focused_id(widget: &dyn Widget) -> Option<String> {
    if widget.is_focused()
        && let Some(id) = widget.test_id()
    {
        return Some(id.to_owned());
    }
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = focused_id(child);
        }
    });
    found
}

/// Give the keyboard to the widget under `widget` whose id is `id`.
fn focus_id(widget: &mut dyn Widget, id: &str) -> bool {
    if widget.test_id() == Some(id) && widget.is_focusable() {
        widget.set_focus(true);
        return true;
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| focus_id(child, id))
}

/// The form, rebuilt whenever its [`Shape`] changes.
pub(crate) struct SetupForm {
    fields: SetupFields,
    requests: Requests,
    editing: bool,
    content: Element,
    built_for: Shape,
    structure_dirty: bool,
    /// Typing starts at the address when the sheet opens.
    focus_first: bool,
}

impl SetupForm {
    fn new(fields: &SetupFields, requests: &Requests, editing: bool) -> Self {
        let shape = Shape::of(fields);
        Self {
            fields: fields.clone(),
            requests: Rc::clone(requests),
            editing,
            content: form(fields, requests, editing, &shape),
            built_for: shape,
            structure_dirty: false,
            focus_first: true,
        }
    }

    /// Whether the field `index` has the keyboard.
    pub(crate) fn field_focused(&self, index: usize) -> bool {
        focused_id(self.content.as_ref()).is_some_and(|id| id == field_id(index))
    }
}

impl Decorator for SetupForm {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        let shape = Shape::of(&self.fields);
        if shape != self.built_for {
            let focused = focused_id(self.content.as_ref());
            self.content = form(&self.fields, &self.requests, self.editing, &shape);
            self.built_for = shape;
            self.structure_dirty = true;
            if let Some(id) = focused {
                focus_id(self.content.as_mut(), &id);
            }
        }
        self.content.sync(state, path);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let rebuilt = std::mem::take(&mut self.structure_dirty);
        self.content.take_structure_dirty() || rebuilt
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_first)
    }

    fn name(&self) -> &'static str {
        "SetupForm"
    }
}

/// The open account sheet.
pub(crate) struct SetupSheet {
    pub(crate) dialog: ModalDialog<SetupForm>,
    pub(crate) fields: SetupFields,
    requests: Requests,
    /// Whether the email field had the keyboard at the last look, so the
    /// app can fill the servers in when it leaves.
    email_focused: bool,
    /// The address at the last look, so the app can fill the servers in as
    /// a known address is typed.
    last_email: String,
    /// Field errors show once a save found the account incomplete, and
    /// follow the fields from then on.
    validated: bool,
}

impl SetupSheet {
    /// The sheet for `cfg`, open. `editing` shows the way to remove it.
    pub(crate) fn open(cfg: &AccountConfig, editing: bool, max_width: f64) -> Self {
        let fields = SetupFields::of(cfg);
        // Changing an account starts from the servers it has.
        fields.set_advanced(editing);
        let requests = Requests::default();
        let save = Rc::clone(&requests);
        let cancel = Rc::clone(&requests);
        let title = crate::i18n::t(if editing {
            "mail-setup-heading-edit"
        } else {
            "mail-setup-heading-add"
        });
        let dialog = ModalDialog::new(title, SetupForm::new(&fields, &requests, editing))
            .with_test_id("mail.setup")
            .with_max_width(max_width)
            .primary_action(crate::i18n::t("mail-setup-save"), move || {
                save.borrow_mut().push(SetupRequest::Save);
            })
            .secondary_action(crate::i18n::t("mail-setup-cancel"), move || {
                cancel.borrow_mut().push(SetupRequest::Cancel);
            })
            .open(true);
        Self {
            dialog,
            last_email: fields.value(EMAIL),
            fields,
            requests,
            email_focused: false,
            validated: false,
        }
    }

    /// What the person asked for since the last call.
    pub(crate) fn take_requests(&self) -> Vec<SetupRequest> {
        std::mem::take(&mut *self.requests.borrow_mut())
    }

    /// Ask for `request` as the sheet's own controls do.
    pub(crate) fn request(&self, request: SetupRequest) {
        self.requests.borrow_mut().push(request);
    }

    /// Whether the email field just gave up the keyboard.
    pub(crate) fn take_email_left(&mut self) -> bool {
        let focused = self.dialog.content().field_focused(EMAIL);
        let left = self.email_focused && !focused;
        self.email_focused = focused;
        left
    }

    /// Whether the address changed since the last call.
    pub(crate) fn take_email_changed(&mut self) -> bool {
        let email = self.fields.value(EMAIL);
        if email == self.last_email {
            return false;
        }
        self.last_email = email;
        true
    }

    /// Show the fields `cfg` leaves incomplete, from now on.
    pub(crate) fn show_field_errors(&mut self) {
        self.validated = true;
        self.refresh_field_errors();
    }

    /// Follow the fields once errors are shown.
    pub(crate) fn refresh_field_errors(&self) {
        if self.validated {
            self.fields
                .set_field_errors(self.fields.config().validate());
        }
    }

    /// Give a field focus through the retained control tree.
    pub(crate) fn focus_field(&mut self, index: usize) -> bool {
        focus_id(&mut self.dialog, &field_id(index))
    }

    /// The Save action says what is happening and waits while it does.
    pub(crate) fn set_busy(&mut self, label: &str, busy: bool) {
        self.dialog.set_primary_label(label);
        if self.dialog.primary_enabled() == busy {
            self.dialog.set_primary_enabled(!busy);
            self.dialog.invalidate_layout();
        }
    }
}

#[cfg(test)]
mod import_tests;
