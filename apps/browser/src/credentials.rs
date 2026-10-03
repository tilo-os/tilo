//! Device-local chrome consent. Password values never enter this model.
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use ui::{
    LocalState,
    text_render::TextKind,
    tokens::TextRole,
    webview::{
        CredentialAccount, CredentialAction, CredentialForm, CredentialReply, CredentialRequest,
        SharedEngine, WebViewCommand,
    },
    widget::{
        Actions, Button, ButtonRole, Column, Container, ListRow, Row, ScrollAxis, Scrollable,
        Section, SectionHeader, Spacer, Text, Widget,
    },
};

#[derive(Clone, Debug)]
enum Action {
    Open,
    Addresses,
    FillAddress,
    Close,
    Refresh,
    Form(String),
    Account(String),
    Capture,
    Save,
    Update,
    Fill,
    Delete,
    ConfirmDelete,
    CancelDelete,
}
struct Pending {
    action: &'static str,
    deadline: Instant,
}
pub(crate) struct Credentials {
    pub open: bool,
    private: bool,
    address_mode: bool,
    addresses: crate::address_controls::AddressControls,
    origin: Option<String>,
    document: String,
    sequence: u32,
    pending: BTreeMap<u32, Pending>,
    forms: Vec<CredentialForm>,
    accounts: Vec<CredentialAccount>,
    form: Option<String>,
    account: Option<String>,
    capture: Option<(String, String, Instant)>,
    delete: Option<String>,
    notice: String,
    action: LocalState<Option<Action>>,
}
impl Credentials {
    pub fn new(private: bool) -> Self {
        Self::with_address_path(
            private,
            if private {
                None
            } else {
                crate::storage::config_dir().map(|p| p.join("addresses.json"))
            },
        )
    }
    pub fn with_address_path(private: bool, path: Option<std::path::PathBuf>) -> Self {
        Self {
            open: false,
            private,
            address_mode: false,
            addresses: crate::address_controls::AddressControls::new(private, path),
            origin: None,
            document: String::new(),
            sequence: 0,
            pending: BTreeMap::new(),
            forms: vec![],
            accounts: vec![],
            form: None,
            account: None,
            capture: None,
            delete: None,
            notice: String::new(),
            action: LocalState::new(None),
        }
    }
    fn request(&mut self, action: CredentialAction) -> Vec<CredentialRequest> {
        if self.private
            || self.origin.is_none()
            || self.sequence >= i32::MAX as u32
            || self.pending.len() >= 16
        {
            return vec![];
        }
        if !matches!(action, CredentialAction::Status | CredentialAction::Cancel)
            && self.document.is_empty()
        {
            return vec![];
        }
        self.sequence += 1;
        if !matches!(action, CredentialAction::Cancel) {
            self.pending.insert(
                self.sequence,
                Pending {
                    action: action.name(),
                    deadline: ui::time::now() + Duration::from_secs(35),
                },
            );
        }
        vec![CredentialRequest {
            request_id: self.sequence,
            origin: self.origin.clone().unwrap_or_default(),
            document: self.document.clone(),
            action,
        }]
    }
    pub fn close(&mut self) -> Vec<CredentialRequest> {
        let requests = if self.open {
            self.request(CredentialAction::Cancel)
        } else {
            vec![]
        };
        self.open = false;
        self.addresses.clear();
        self.document.clear();
        self.pending.clear();
        self.forms.clear();
        self.accounts.clear();
        self.form = None;
        self.account = None;
        self.capture = None;
        self.delete = None;
        self.action.set(None);
        requests
    }
    pub fn navigation(&mut self, uri: &str) -> Vec<CredentialRequest> {
        let requests = self.close();
        self.origin = crate::site_controls::origin(uri).filter(|s| s.starts_with("https://"));
        requests
    }
    fn open_addresses(&mut self, supported: bool) -> Vec<CredentialRequest> {
        self.address_mode = true;
        self.open(supported)
    }
    fn open(&mut self, supported: bool) -> Vec<CredentialRequest> {
        let mut requests = self.close();
        self.open = true;
        if self.address_mode {
            self.addresses.refresh();
        }
        self.notice = if self.private {
            "Passwords and address autofill are unavailable in private windows."
        } else if !supported {
            "Form filling is unavailable with this browser engine."
        } else if self.origin.is_none() {
            "Open a secure HTTPS page to fill a form."
        } else {
            "Checking this page…"
        }
        .into();
        if !self.private && supported && self.origin.is_some() {
            requests.extend(self.request(CredentialAction::Status));
        }
        requests
    }
    pub fn apply(&mut self, reply: CredentialReply) -> Vec<CredentialRequest> {
        let Some(pending) = self.pending.get(&reply.request_id) else {
            return vec![];
        };
        if !self.open || pending.action != reply.action || ui::time::now() >= pending.deadline {
            return vec![];
        }
        if reply.success
            && (self.origin.as_deref() != Some(reply.origin.as_str())
                || (reply.action != "status" && reply.document != self.document))
        {
            return vec![];
        }
        self.pending.remove(&reply.request_id);
        self.notice = reply.message;
        if !reply.success || reply.late {
            return vec![];
        }
        match reply.action.as_str() {
            "status" => {
                if reply.document.is_empty() {
                    return vec![];
                }
                self.document = reply.document;
                if self.address_mode {
                    return self.request(CredentialAction::DiscoverAddresses);
                }
                let mut requests = self.request(CredentialAction::Discover);
                requests.extend(self.request(CredentialAction::List));
                return requests;
            }
            "discover" | "discover_addresses" => {
                self.forms = reply.forms;
                self.form = match self.forms.as_slice() {
                    [form] => Some(form.id.clone()),
                    _ => None,
                };
            }
            "list" => {
                self.accounts = reply.accounts;
                self.account = match self.accounts.as_slice() {
                    [account] => Some(account.id.clone()),
                    _ => None,
                };
            }
            "capture" => {
                self.capture = reply.capture.map(|id| {
                    (
                        id,
                        reply.username.unwrap_or_default(),
                        ui::time::now() + Duration::from_secs(30),
                    )
                });
            }
            "save" | "update" | "delete" => return self.request(CredentialAction::List),
            _ => {}
        }
        vec![]
    }
    fn act(&mut self, action: Action) -> Vec<CredentialRequest> {
        if matches!(action, Action::Close) {
            return self.close();
        }
        if !self.open || self.private {
            return vec![];
        }
        match action {
            Action::Form(id)
                if self.pending.is_empty() && self.forms.iter().any(|f| f.id == id) =>
            {
                self.form = Some(id);
                self.capture = None;
            }
            Action::Account(id)
                if self.pending.is_empty() && self.accounts.iter().any(|a| a.id == id) =>
            {
                self.account = Some(id);
                self.delete = None;
            }
            Action::CancelDelete => self.delete = None,
            _ if !self.pending.is_empty() => return vec![],
            Action::FillAddress if self.address_mode => {
                if let (Some(form), Some(profile)) =
                    (self.form.clone(), self.addresses.selected_fields())
                {
                    return self.request(CredentialAction::FillAddress { form, profile });
                }
                self.notice = "Select an enabled, saved address before filling.".into();
            }
            Action::Capture => {
                if let Some(form) = self.form.clone() {
                    self.capture = None;
                    return self.request(CredentialAction::Capture { form });
                }
            }
            Action::Fill => {
                if let (Some(form), Some(account)) = (self.form.clone(), self.account.clone()) {
                    return self.request(CredentialAction::Fill { form, account });
                }
            }
            Action::Save | Action::Update => {
                if matches!(action, Action::Update) && self.account.is_none() {
                    return vec![];
                }
                if let Some((capture, _, expires)) = self.capture.take() {
                    if ui::time::now() >= expires {
                        self.notice = "Capture expired. Read the login again.".into();
                        return vec![];
                    }
                    let action = if matches!(action, Action::Save) {
                        CredentialAction::Save { capture }
                    } else {
                        CredentialAction::Update {
                            capture,
                            account: self.account.clone().unwrap_or_default(),
                        }
                    };
                    return self.request(action);
                }
            }
            Action::Delete => self.delete = self.account.clone(),
            Action::ConfirmDelete => {
                if let Some(account) = self.delete.take() {
                    return self.request(CredentialAction::Delete { account });
                }
            }
            _ => {}
        }
        vec![]
    }
    pub fn send(engine: &SharedEngine, requests: Vec<CredentialRequest>) {
        for request in requests {
            engine.send(WebViewCommand::Credentials(request));
        }
    }
    pub fn sync(&mut self, engine: &SharedEngine) -> bool {
        let mut dirty = false;
        if let Some(action) = self.action.get() {
            self.action.set(None);
            dirty = true;
            let requests = match action {
                Action::Open => {
                    self.address_mode = false;
                    self.open(engine.supports_native_filtering())
                }
                Action::Addresses => self.open_addresses(engine.supports_native_filtering()),
                Action::Refresh => self.open(engine.supports_native_filtering()),
                other => self.act(other),
            };
            Self::send(engine, requests);
        }
        if self.open && self.address_mode {
            dirty |= self.addresses.sync();
        }
        let now = ui::time::now();
        if self.pending.values().any(|p| now >= p.deadline)
            || self.capture.as_ref().is_some_and(|c| now >= c.2)
        {
            let requests = self.request(CredentialAction::Cancel);
            Self::send(engine, requests);
            self.pending.clear();
            self.capture = None;
            self.delete = None;
            self.notice="Request expired. Refresh before trying again; an operation already started may still finish.".into();
            dirty = true;
        }
        dirty
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|p| p.deadline)
            .chain(self.capture.iter().map(|c| c.2))
            .min()
    }
    fn button(&self, id: &str, label: &str, action: Action) -> Button {
        Button::new(id, label, None).on_effect(self.command(action))
    }
    fn command(&self, action: Action) -> impl FnMut() + 'static {
        let pending = self.action.clone();
        move || pending.set(Some(action.clone()))
    }
    /// A maker of the closure that opens the passwords panel, or the address
    /// autofill panel when `addresses` is set. The settings page asks it for
    /// a fresh closure each time it composes.
    pub fn opener(&self, addresses: bool) -> impl Fn() -> Box<dyn FnMut()> + 'static {
        let pending = self.action.clone();
        move || {
            let pending = pending.clone();
            Box::new(move || {
                pending.set(Some(if addresses {
                    Action::Addresses
                } else {
                    Action::Open
                }));
            })
        }
    }
    /// One choice among a few (a form, an account): the chosen row is lifted.
    fn choice(
        &self,
        id: String,
        title: &str,
        detail: Option<&str>,
        chosen: bool,
        action: Action,
    ) -> ListRow {
        let mut row = ListRow::new(id, title)
            .selected(chosen)
            .on_activate(self.command(action));
        if let Some(detail) = detail {
            row = row.secondary(detail);
        }
        row
    }
    pub fn panel(&self, width: f32, height: f32) -> Box<dyn Widget> {
        let ready = self.pending.is_empty() && !self.document.is_empty();
        let mut column = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(
                        Text::new(if self.address_mode {
                            "Address autofill"
                        } else {
                            "Passwords"
                        })
                        .with_role(TextRole::Title),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(self.button(
                        "browser.credentials.close",
                        "Done",
                        Action::Close,
                    ))),
            ))
            .push(Box::new(
                Text::new(self.origin.as_deref().unwrap_or("No secure page"))
                    .with_role(TextRole::Caption)
                    .with_kind(TextKind::Identifier)
                    .dimmed(),
            ));
        if !self.notice.is_empty() {
            column = column.push(line(&self.notice));
        }
        column = if self.address_mode {
            self.address_controls(column, ready)
        } else if !self.private && self.origin.is_some() {
            self.password_controls(column, ready)
        } else {
            column
        };
        Box::new(
            Container::new()
                .with_focus_trap()
                .with_no_padding()
                .with_width(width)
                .with_height(height)
                .with_child(Box::new(
                    Scrollable::new(ScrollAxis::Vertical)
                        .with_focus_reveal()
                        .with_height(height)
                        .with_child(Box::new(Container::new().with_child(Box::new(column)))),
                )),
        )
    }
    fn address_controls(&self, mut column: Column, ready: bool) -> Column {
        column = column.push(self.addresses.panel());
        if !self.private && self.origin.is_some() && !self.addresses.is_editing() {
            let forms: Vec<Box<dyn Widget>> = self
                .forms
                .iter()
                .enumerate()
                .map(|(index, form)| {
                    Box::new(self.choice(
                        format!("browser.address.form.{index}"),
                        &format!("Form {}", index + 1),
                        None,
                        self.form.as_ref() == Some(&form.id),
                        Action::Form(form.id.clone()),
                    )) as Box<dyn Widget>
                })
                .collect();
            if forms.is_empty() {
                column = column.push(line("No supported address form found on this page."));
            } else {
                column = column.push(Box::new(
                    Section::new(forms).with_header(SectionHeader::group("Address form")),
                ));
            }
            column = column.push(Box::new(
                Actions::new(
                    self.button(
                        "browser.address.fill",
                        "Fill selected address",
                        Action::FillAddress,
                    )
                    .role(ButtonRole::Primary)
                    .with_enabled(ready && self.form.is_some() && self.addresses.has_selection()),
                )
                .secondary(
                    self.button("browser.address.refresh", "Refresh forms", Action::Refresh)
                        .with_enabled(!self.addresses.is_editing()),
                ),
            ));
        }
        column
    }

    fn password_controls(&self, mut column: Column, ready: bool) -> Column {
        if self.document.is_empty() {
            return column.push(Box::new(self.button(
                "browser.credentials.refresh",
                "Refresh forms and accounts",
                Action::Refresh,
            )));
        }
        if self.forms.is_empty() {
            column = column.push(line("No supported login form found."));
        } else {
            let forms: Vec<Box<dyn Widget>> = self
                .forms
                .iter()
                .enumerate()
                .map(|(index, form)| {
                    Box::new(self.choice(
                        format!("browser.credentials.form.{index}"),
                        &format!("Form {}", index + 1),
                        Some(if form.kind == "new-password" {
                            "New password"
                        } else {
                            "Sign in"
                        }),
                        self.form.as_ref() == Some(&form.id),
                        Action::Form(form.id.clone()),
                    )) as Box<dyn Widget>
                })
                .collect();
            column = column.push(Box::new(
                Section::new(forms).with_header(SectionHeader::group("Login form")),
            ));
        }
        if self.accounts.is_empty() {
            column = column.push(line("No saved accounts for this site."));
        } else {
            let accounts: Vec<Box<dyn Widget>> = self
                .accounts
                .iter()
                .enumerate()
                .map(|(index, account)| {
                    Box::new(self.choice(
                        format!("browser.credentials.account.{index}"),
                        if account.username.is_empty() {
                            "Unnamed account"
                        } else {
                            &account.username
                        },
                        None,
                        self.account.as_ref() == Some(&account.id),
                        Action::Account(account.id.clone()),
                    )) as Box<dyn Widget>
                })
                .collect();
            column = column.push(Box::new(
                Section::new(accounts).with_header(SectionHeader::group("Saved accounts")),
            ));
        }
        self.password_actions(column, ready)
    }

    fn password_actions(&self, mut column: Column, ready: bool) -> Column {
        column = column.push(Box::new(
            Actions::new(
                self.button(
                    "browser.credentials.fill",
                    "Fill selected account",
                    Action::Fill,
                )
                .role(ButtonRole::Primary)
                .with_enabled(ready && self.form.is_some() && self.account.is_some()),
            )
            .secondary(
                self.button(
                    "browser.credentials.capture",
                    "Read login from selected form",
                    Action::Capture,
                )
                .with_enabled(ready && self.form.is_some()),
            )
            .secondary(self.button(
                "browser.credentials.refresh",
                "Refresh forms and accounts",
                Action::Refresh,
            )),
        ));
        if let Some((_, username, _)) = &self.capture {
            column = column
                .push(Box::new(
                    Text::new(&format!("Save login for {username}?"))
                        .with_role(TextRole::Subheading),
                ))
                .push(line(
                    "The password stays in the native browser and macOS Keychain.",
                ))
                .push(Box::new(
                    Actions::new(
                        self.button("browser.credentials.save", "Save new account", Action::Save)
                            .with_enabled(ready),
                    )
                    .secondary(
                        self.button(
                            "browser.credentials.update",
                            "Update selected account",
                            Action::Update,
                        )
                        .with_enabled(ready && self.account.is_some()),
                    ),
                ));
        }
        if self.delete.is_some() {
            column = column
                .push(line("Delete the selected saved account from Keychain?"))
                .push(Box::new(
                    Actions::new(
                        self.button(
                            "browser.credentials.delete.confirm",
                            "Delete account",
                            Action::ConfirmDelete,
                        )
                        .role(ButtonRole::Destructive)
                        .with_enabled(ready),
                    )
                    .secondary(self.button(
                        "browser.credentials.delete.cancel",
                        "Keep account",
                        Action::CancelDelete,
                    )),
                ));
        } else {
            column = column.push(Box::new(
                self.button(
                    "browser.credentials.delete",
                    "Delete selected account",
                    Action::Delete,
                )
                .role(ButtonRole::Destructive)
                .with_enabled(ready && self.account.is_some()),
            ));
        }
        column
    }
}

fn line(text: &str) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_role(TextRole::Body))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reply(request: &CredentialRequest, document: &str) -> CredentialReply {
        CredentialReply {
            request_id: request.request_id,
            action: request.action.name().into(),
            origin: request.origin.clone(),
            document: document.into(),
            success: true,
            message: "Ready".into(),
            forms: vec![],
            accounts: vec![],
            capture: None,
            username: None,
            late: false,
        }
    }
    /// Form discovery, private and insecure pages, late replies.
    #[test]
    fn credential_discovery_is_scoped_to_safe_current_documents() {
        // Phase `address_mode_discovers_forms_without_keychain_account_requests`.
        {
            let directory = tempfile::tempdir().unwrap();
            let mut model = Credentials::with_address_path(
                false,
                Some(directory.path().join("addresses.json")),
            );
            model.navigation("https://example.com");
            let status = model.open_addresses(true).remove(0);
            let requests = model.apply(reply(&status, "1"));
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].action, CredentialAction::DiscoverAddresses);
            let mut response = reply(&requests[0], "1");
            response.forms.push(CredentialForm {
                id: "4".into(),
                username: String::new(),
                kind: String::new(),
            });
            assert!(model.apply(response).is_empty());
            assert_eq!(model.form.as_deref(), Some("4"));
            assert!(
                model.act(Action::FillAddress).is_empty(),
                "profile selection is required"
            );
        }

        // Phase `private_and_insecure_pages_never_request_credentials`.
        {
            let mut private = Credentials::new(true);
            private.navigation("https://example.com");
            assert!(private.open(true).is_empty());
            let mut normal = Credentials::new(false);
            normal.navigation("http://example.com");
            assert!(normal.open(true).is_empty());
        }

        // Phase `navigation_and_close_reject_late_document_replies`.
        {
            let mut model = Credentials::new(false);
            model.navigation("https://example.com/a");
            let request = model.open(true).remove(0);
            model.navigation("https://example.com/b");
            assert!(model.apply(reply(&request, "1")).is_empty());
            assert!(model.document.is_empty());
            let request = model.open(true).remove(0);
            model.close();
            assert!(model.apply(reply(&request, "2")).is_empty());
            assert!(model.document.is_empty());
        }
    }

    /// Account selection, deletion confirmation and capture lifetime.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn credential_selection_and_capture_require_explicit_fresh_choices() {
        // Phase `multiple_accounts_require_selection_and_delete_confirmation`.
        {
            let mut model = Credentials::new(false);
            model.navigation("https://example.com");
            let status = model.open(true).remove(0);
            let requests = model.apply(reply(&status, "1"));
            for request in requests {
                let mut response = reply(&request, "1");
                if matches!(request.action, CredentialAction::List) {
                    response.accounts = vec![
                        CredentialAccount {
                            id: "a".into(),
                            username: "first".into(),
                        },
                        CredentialAccount {
                            id: "b".into(),
                            username: "second".into(),
                        },
                    ];
                }
                model.apply(response);
            }
            assert!(model.account.is_none());
            assert!(model.act(Action::ConfirmDelete).is_empty());
            assert!(model.act(Action::Account("unknown".into())).is_empty());
            assert!(model.account.is_none());
            model.act(Action::Account("b".into()));
            assert!(model.act(Action::Delete).is_empty());
            let deletion = model.act(Action::ConfirmDelete).remove(0);
            assert_eq!(
                deletion.action,
                CredentialAction::Delete {
                    account: "b".into()
                }
            );
            assert!(model.act(Action::ConfirmDelete).is_empty());
        }

        // Phase `expired_capture_cannot_be_saved`.
        {
            let mut model = Credentials::new(false);
            model.navigation("https://example.com");
            model.open = true;
            model.document = "1".into();
            model.capture = Some((
                "2".into(),
                "person".into(),
                ui::time::now().checked_sub(Duration::from_secs(1)).unwrap(),
            ));
            assert!(model.act(Action::Save).is_empty());
            assert!(model.capture.is_none());
        }

        // Phase `capture_waits_for_explicit_save_and_is_consumed_once`.
        {
            let mut model = Credentials::new(false);
            model.navigation("https://example.com");
            let status = model.open(true).remove(0);
            let requests = model.apply(reply(&status, "1"));
            assert_eq!(requests.len(), 2);
            let discover = requests
                .iter()
                .find(|r| matches!(r.action, CredentialAction::Discover))
                .unwrap();
            let mut forms = reply(discover, "1");
            forms.forms.push(CredentialForm {
                id: "1".into(),
                username: "person".into(),
                kind: "current-password".into(),
            });
            model.apply(forms);
            let list = requests
                .iter()
                .find(|r| matches!(r.action, CredentialAction::List))
                .unwrap();
            model.apply(reply(list, "1"));
            let capture = model.act(Action::Capture).remove(0);
            let mut captured = reply(&capture, "1");
            captured.capture = Some("8".into());
            captured.username = Some("person".into());
            assert!(model.apply(captured).is_empty());
            let save = model.act(Action::Save).remove(0);
            assert_eq!(
                save.action,
                CredentialAction::Save {
                    capture: "8".into()
                }
            );
            assert!(model.act(Action::Save).is_empty());
        }
    }
}
