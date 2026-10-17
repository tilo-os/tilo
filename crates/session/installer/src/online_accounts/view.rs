//! Device-private account review. No field, hint or command is document-backed.
//!
//! The owner drains commands into its account actor and publishes only a bounded
//! status here. Rebuild `view` after provider/status/source changes; text inputs
//! and checkboxes keep their two-way bindings across rebuilds. Tokens never
//! enter this form. The prepared-medium password uses only a masked external
//! buffer, never document state or public actions. Hints are not authorization grants.

use std::collections::VecDeque;

use ui::LocalState;
use ui::tokens::TextRole;
use ui::widget::{Button, Checkbox, Column, Row, Text, TextInput, TextInputKind, Widget};

use crate::i18n::t;

const COMMAND_CAPACITY: usize = 8;
const MAX_HINTS: usize = 32;
const MAX_FIELD_BYTES: usize = 2048;

pub use account_custody::hints::{AccountHint, Provider};

trait ProviderView {
    fn label(self) -> &'static str;
    fn supports_mail(self) -> bool;
    fn supports_calendar(self) -> bool;
}

impl ProviderView for Provider {
    fn label(self) -> &'static str {
        match self {
            Self::Google => "installer-online-google",
            Self::Microsoft => "installer-online-microsoft",
            Self::Dropbox => "installer-online-dropbox",
            Self::Nextcloud => "installer-online-nextcloud",
        }
    }

    fn supports_mail(self) -> bool {
        matches!(self, Self::Google | Self::Microsoft)
    }

    fn supports_calendar(self) -> bool {
        matches!(self, Self::Google | Self::Nextcloud)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct AccountRequest {
    pub provider: Provider,
    pub email: String,
    pub server: String,
    pub files: bool,
    pub mail: bool,
    pub calendar: bool,
}

/// Private presentation of a successfully stored, selected account. The owner
/// derives services from actual grants, never the pending request or a hint.
#[derive(Clone, Eq, PartialEq)]
pub struct ConnectedAccount {
    pub provider: Provider,
    pub identity: String,
    pub files: bool,
    pub mail: bool,
    pub calendar: bool,
}

/// Private in-process messages. The owner must validate again before sign-in.
#[derive(Clone, Eq, PartialEq)]
pub enum AccountsCommand {
    Add(AccountRequest),
    Retry(AccountRequest),
    Cancel,
    SkipOffline,
    SelectCalendars(Vec<String>),
}

/// Fixed statuses keep URLs, tokens and arbitrary backend errors out of the UI.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum AccountsStatus {
    #[default]
    Ready,
    SigningIn,
    ReviewCalendars,
    Failed,
    Added,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormError {
    Busy,
    InvalidFields,
    QueueFull,
    StaleSource,
}

/// Standalone form/controller. Handles and the queue stay in this process only.
#[derive(Clone)]
pub struct AccountsForm {
    pub provider: LocalState<Provider>,
    pub email: LocalState<String>,
    pub server: LocalState<String>,
    pub files: LocalState<bool>,
    pub mail: LocalState<bool>,
    pub calendar: LocalState<bool>,
    collections: LocalState<Vec<(String, String, LocalState<bool>)>>,
    pub(super) windows: super::hints::WindowsPrompt,
    status: LocalState<AccountsStatus>,
    notice: LocalState<String>,
    source: LocalState<Option<String>>,
    hints: LocalState<Vec<AccountHint>>,
    connected: LocalState<Vec<ConnectedAccount>>,
    commands: LocalState<VecDeque<AccountsCommand>>,
    structure_revision: LocalState<u64>,
    synced_structure: LocalState<Option<(Provider, u64)>>,
}

impl Default for AccountsForm {
    fn default() -> Self {
        Self {
            provider: LocalState::new(Provider::Google),
            email: LocalState::new(String::new()),
            server: LocalState::new(String::new()),
            files: LocalState::new(true),
            mail: LocalState::new(false),
            calendar: LocalState::new(false),
            collections: LocalState::new(Vec::new()),
            windows: super::hints::WindowsPrompt::default(),
            status: LocalState::new(AccountsStatus::Ready),
            notice: LocalState::new(String::new()),
            source: LocalState::new(None),
            hints: LocalState::new(Vec::new()),
            connected: LocalState::new(Vec::new()),
            commands: LocalState::new(VecDeque::new()),
            structure_revision: LocalState::new(0),
            synced_structure: LocalState::new(None),
        }
    }
}

impl AccountsForm {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume structural changes without rebuilding for text or consent edits.
    /// Include the public provider binding so direct selections are observed too.
    pub(crate) fn take_structure_changed(&self) -> bool {
        let current = (
            self.provider.with(|provider| *provider),
            self.structure_revision.with(|revision| *revision),
        );
        let changed = self
            .synced_structure
            .with(|previous| *previous != Some(current));
        self.synced_structure.set(Some(current));
        changed
    }

    pub(super) fn invalidate_structure(&self) {
        self.structure_revision
            .update(|revision| *revision = revision.wrapping_add(1));
    }

    pub(crate) fn status(&self) -> AccountsStatus {
        self.status.with(|status| *status)
    }

    pub fn set_status(&self, status: AccountsStatus) {
        if self.status.with(|previous| *previous != status) {
            self.invalidate_structure();
        }
        self.status.set(status);
        if status != AccountsStatus::ReviewCalendars {
            self.collections.set(Vec::new());
        }
        self.notice.set(String::new());
    }

    /// Replace the private selected-account list from the actor's stored grants.
    /// Imported suggestions and pending authorizations must never call this.
    pub fn set_connected(&self, accounts: Vec<ConnectedAccount>) {
        if self.connected.with(|previous| *previous != accounts) {
            self.connected.set(accounts);
            self.invalidate_structure();
        }
    }

    /// Invalidate decrypted suggestions before starting a new source read.
    /// Call only while idle; source selection never cancels an actor implicitly.
    ///
    /// # Errors
    /// Returns `FormError::Busy` while authorization is pending.
    pub fn select_source(&self, source: Option<String>) -> Result<(), FormError> {
        if self.status.with(|s| {
            matches!(
                *s,
                AccountsStatus::SigningIn | AccountsStatus::ReviewCalendars
            )
        }) {
            return Err(FormError::Busy);
        }
        self.source.set(source);
        self.hints.set(Vec::new());
        self.email.set(String::new());
        self.server.set(String::new());
        self.files.set(false);
        self.mail.set(false);
        self.calendar.set(false);
        self.invalidate_structure();
        Ok(())
    }

    /// Accept decrypted hints only for the currently selected source. The owner
    /// must use a fresh source identifier for each read, including re-selection,
    /// so a late completion cannot repopulate a different source's suggestions.
    ///
    /// # Errors
    /// Returns `FormError::StaleSource` if the source is no longer selected.
    pub fn accept_hints(&self, source: &str, hints: Vec<AccountHint>) -> Result<(), FormError> {
        if !self.source.with(|s| s.as_deref() == Some(source)) {
            return Err(FormError::StaleSource);
        }
        if hints.len() > MAX_HINTS
            || hints
                .iter()
                .any(|hint| !bounded_field(&hint.email) || !safe_server(&hint.server))
        {
            return Err(FormError::InvalidFields);
        }
        self.hints.set(account_custody::hints::group_hints(hints));
        self.invalidate_structure();
        Ok(())
    }

    /// Copy a suggestion into the editable form without granting mail consent.
    ///
    /// # Errors
    /// Rejects pending authorization or an index outside the current hints.
    pub fn choose_hint(&self, index: usize) -> Result<(), FormError> {
        if self.status.with(|s| {
            matches!(
                *s,
                AccountsStatus::SigningIn | AccountsStatus::ReviewCalendars
            )
        }) {
            return Err(FormError::Busy);
        }
        let hint = self
            .hints
            .with(|hints| hints.get(index).cloned())
            .ok_or(FormError::InvalidFields)?;
        self.provider.set(hint.provider);
        self.email.set(hint.email);
        self.server.set(hint.server);
        self.files.set(false); // Suggestions never select permissions.
        self.mail.set(false);
        self.calendar.set(false);
        self.invalidate_structure();
        Ok(())
    }

    fn request(&self) -> Result<AccountRequest, FormError> {
        let provider = self.provider.with(|p| *p);
        if !self.email.with(|s| bounded_field(s)) || !self.server.with(|s| safe_server(s)) {
            return Err(FormError::InvalidFields);
        }
        let email = self.email.with(|s| s.trim().to_owned());
        let server = if provider == Provider::Nextcloud {
            self.server.with(|s| s.trim().to_owned())
        } else {
            String::new()
        };
        let files = self.files.with(|v| *v);
        let mail = provider.supports_mail() && self.mail.with(|v| *v);
        let calendar = provider.supports_calendar() && self.calendar.with(|v| *v);
        if !bounded_field(&email) || !bounded_field(&server) || (!files && !mail && !calendar) {
            return Err(FormError::InvalidFields);
        }
        if provider == Provider::Nextcloud && (server.is_empty() || !safe_server(&server)) {
            return Err(FormError::InvalidFields);
        }
        Ok(AccountRequest {
            provider,
            email,
            server,
            files,
            mail,
            calendar,
        })
    }

    /// Same entry point for local widgets and the owner's semantic actions.
    /// Retry takes a fresh snapshot, so corrections to email/server are honored.
    ///
    /// # Errors
    /// Rejects pending authorization, retries outside the failed state, invalid
    /// fields, or a full command queue.
    pub fn submit(&self, retry: bool) -> Result<(), FormError> {
        let status = self.status.with(|s| *s);
        if status == AccountsStatus::SigningIn || (retry && status != AccountsStatus::Failed) {
            return Err(FormError::Busy);
        }
        if status == AccountsStatus::ReviewCalendars {
            let selected = self.collections.with(|collections| {
                collections
                    .iter()
                    .filter(|(_, _, checked)| checked.with(|v| *v))
                    .map(|(id, _, _)| id.clone())
                    .collect::<Vec<_>>()
            });
            if selected.is_empty() {
                return Err(FormError::InvalidFields);
            }
            return self.enqueue(AccountsCommand::SelectCalendars(selected));
        }
        let request = self.request()?;
        let command = if retry {
            AccountsCommand::Retry(request)
        } else {
            AccountsCommand::Add(request)
        };
        self.enqueue(command)
    }

    /// Publish only the current actor's discovered collections into local review.
    /// IDs and labels are never exported as action arguments or document state.
    pub fn set_calendars(&self, collections: Vec<(String, String)>) {
        let unchanged = self.collections.with(|previous| {
            previous
                .iter()
                .map(|(id, name, _)| (id, name))
                .eq(collections.iter().map(|(id, name)| (id, name)))
        });
        if !unchanged {
            self.collections.set(
                collections
                    .into_iter()
                    .map(|(id, name)| (id, name, LocalState::new(false)))
                    .collect(),
            );
            self.invalidate_structure();
        }
    }

    #[cfg(test)]
    pub(crate) fn select_calendar_for_test(&self, index: usize) {
        self.collections
            .with(|collections| collections[index].2.set(true));
    }

    fn enqueue(&self, command: AccountsCommand) -> Result<(), FormError> {
        let mut accepted = false;
        self.commands.update(|commands| {
            if commands.len() < COMMAND_CAPACITY {
                commands.push_back(command);
                accepted = true;
            }
        });
        if !accepted {
            return Err(FormError::QueueFull);
        }
        self.set_status(AccountsStatus::SigningIn);
        Ok(())
    }

    /// Escape controls supersede queued starts and remain available when full.
    pub fn cancel(&self) {
        self.escape(AccountsCommand::Cancel);
    }

    /// Skipping does not claim any service has been connected.
    pub fn skip_offline(&self) {
        self.escape(AccountsCommand::SkipOffline);
    }

    fn escape(&self, command: AccountsCommand) {
        self.commands.update(|commands| {
            commands.clear();
            commands.push_back(command);
        });
        // Stay pending until the actor acknowledges cancellation/skip.
        self.set_status(AccountsStatus::SigningIn);
    }

    pub fn drain_commands(&self) -> Vec<AccountsCommand> {
        let mut drained = Vec::new();
        self.commands
            .update(|commands| drained.extend(commands.drain(..)));
        drained
    }

    fn providers(&self, idle: bool, provider: Provider) -> Row {
        let mut providers = Row::new();
        for choice in [
            Provider::Google,
            Provider::Microsoft,
            Provider::Dropbox,
            Provider::Nextcloud,
        ] {
            let form = self.clone();
            providers = providers.push(Box::new(
                Button::new(choice.label(), &t(choice.label()))
                    .with_enabled(idle && provider != choice)
                    .on_view(move || {
                        form.provider.set(choice);
                        form.mail.set(false);
                        form.calendar.set(false);
                    }),
            ));
        }
        providers
    }

    pub fn view(&self) -> Box<dyn Widget> {
        let provider = self.provider.with(|p| *p);
        let status = self.status.with(|s| *s);
        let idle = matches!(
            status,
            AccountsStatus::Ready | AccountsStatus::Failed | AccountsStatus::Added
        );
        let mut column = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new(&t("installer-online-title")).with_role(TextRole::Title),
            ))
            .push(Box::new(Text::new(&t("installer-online-private"))));
        column = self.windows_view(column);
        column = self.saved_and_suggested(column, idle);
        column = column
            .push(Box::new(
                Text::new(&t("installer-online-manual")).with_role(TextRole::Heading),
            ))
            .push(Box::new(self.providers(idle, provider)))
            .push(Box::new(Text::new(&t(provider.label()))))
            .push(Box::new(Text::new(&t("installer-online-email"))))
            .push(Box::new(
                TextInput::from_state(&self.email)
                    .with_kind(TextInputKind::Email)
                    .with_id("installer.online.email")
                    .with_accessible_name(t("installer-online-email"))
                    .disabled(!idle),
            ));
        if provider == Provider::Nextcloud {
            column = column
                .push(Box::new(Text::new(&t("installer-online-server"))))
                .push(Box::new(
                    TextInput::from_state(&self.server)
                        .with_id("installer.online.server")
                        .with_accessible_name(t("installer-online-server"))
                        .disabled(!idle),
                ));
        }
        column = column
            .push(Box::new(Text::new(&t("installer-online-review"))))
            .push(Box::new(
                Checkbox::from_state(&self.files)
                    .with_id("installer.online.files")
                    .with_label(t("installer-online-files"))
                    .with_enabled(idle),
            ))
            .push(Box::new(
                Checkbox::from_state(&self.mail)
                    .with_id("installer.online.mail")
                    .with_label(t("installer-online-mail"))
                    .with_enabled(idle && provider.supports_mail()),
            ))
            .push(Box::new(
                Checkbox::from_state(&self.calendar)
                    .with_id("installer.online.calendar")
                    .with_label(t("installer-online-calendar"))
                    .with_enabled(idle && provider.supports_calendar()),
            ))
            .push(Box::new(Text::new(&t(match status {
                AccountsStatus::Ready => "installer-online-ready",
                AccountsStatus::SigningIn => "installer-online-pending",
                AccountsStatus::ReviewCalendars => "installer-online-calendar-review",
                AccountsStatus::Failed => "installer-online-failed",
                AccountsStatus::Added => "installer-online-added",
            }))))
            .push(Box::new(Text::from_state(&self.notice, Clone::clone)));
        if status == AccountsStatus::ReviewCalendars {
            for (index, (_, name, selected)) in
                self.collections.with(Clone::clone).into_iter().enumerate()
            {
                column = column.push(Box::new(
                    Checkbox::from_state(&selected)
                        .with_id(&format!("installer.online.collection.{index}"))
                        .with_label(name),
                ));
            }
        }
        Box::new(
            column
                .push(Box::new(Text::new(&t("installer-online-skip-note"))))
                .push(Box::new(self.actions(status))),
        )
    }

    fn saved_and_suggested(&self, mut column: Column, idle: bool) -> Column {
        let connected = self.connected.with(Clone::clone);
        if !connected.is_empty() {
            column = column
                .push(Box::new(
                    Text::new(&t("installer-online-connected")).with_role(TextRole::Heading),
                ))
                .push(Box::new(Text::new(&t("installer-online-adopted"))))
                .push(Box::new(Text::new(&t("installer-online-loading"))));
        }
        for account in connected {
            column = column
                .push(Box::new(Text::new(&t(account.provider.label()))))
                .push(Box::new(Text::new(&account.identity)));
            for (granted, key) in [
                (account.files, "installer-online-connected-files"),
                (account.mail, "installer-online-connected-mail"),
                (account.calendar, "installer-online-connected-calendar"),
            ] {
                if granted {
                    column = column.push(Box::new(Text::new(&t(key))));
                }
            }
        }
        let hints = self.hints.with(Clone::clone);
        if !hints.is_empty() {
            column = column
                .push(Box::new(
                    Text::new(&t("installer-online-previous")).with_role(TextRole::Heading),
                ))
                .push(Box::new(Text::new(&t("installer-online-suggestions-note"))));
        }
        for (index, hint) in hints.into_iter().enumerate() {
            column = column.push(Box::new(Text::new(&t(hint.provider.label()))));
            for (present, key) in [
                (hint.sources.onedrive, "installer-online-source-onedrive"),
                (hint.sources.outlook, "installer-online-source-outlook"),
                (
                    hint.sources.google_web,
                    "installer-online-source-google-web",
                ),
            ] {
                if present {
                    column = column.push(Box::new(Text::new(&t(key))));
                }
            }
            let form = self.clone();
            column = column.push(Box::new(
                Button::new(
                    &format!("installer.online.hint.{index}"),
                    &t("installer-online-use-hint"),
                )
                .with_enabled(idle)
                .on_view(move || {
                    let _ = form.choose_hint(index);
                }),
            ));
            // Read-only hint text is also local, never a document path.
            let label = hint.email;
            column = column.push(Box::new(Text::new(&label)));
        }
        column
    }

    fn actions(&self, status: AccountsStatus) -> Row {
        let idle = status != AccountsStatus::SigningIn;
        let mut actions = Row::new();
        for (id, enabled) in [
            ("installer-online-add", idle),
            ("installer-online-retry", status == AccountsStatus::Failed),
            ("installer-online-cancel", true),
            ("installer-online-skip", true),
        ] {
            if id == "installer-online-retry" && status != AccountsStatus::Failed {
                continue;
            }
            let form = self.clone();
            actions = actions.push(Box::new(
                Button::new(
                    id,
                    &t(
                        if id == "installer-online-add" && status == AccountsStatus::ReviewCalendars
                        {
                            "installer-online-calendar-confirm"
                        } else {
                            id
                        },
                    ),
                )
                .with_enabled(enabled)
                .on_view(move || {
                    let result = match id {
                        "installer-online-add" => form.submit(false),
                        "installer-online-retry" => form.submit(true),
                        "installer-online-cancel" => {
                            form.cancel();
                            Ok(())
                        }
                        _ => {
                            form.skip_offline();
                            Ok(())
                        }
                    };
                    if result.is_err() {
                        form.notice.set(t("installer-online-invalid"));
                    }
                }),
            ));
        }
        actions
    }
}

fn safe_server(value: &str) -> bool {
    bounded_field(value)
        && (value.is_empty()
            || url::Url::parse(value).is_ok_and(|url| {
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            }))
}

fn bounded_field(value: &str) -> bool {
    value.len() <= MAX_FIELD_BYTES && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perform(form: &AccountsForm, actions: &str) {
        let mut harness =
            ui::headless::FrameHarness::new(ui::State::new(), form.view(), (1280, 1600));
        let heads = harness.state_mut().doc.get_heads();
        let spec = ui::headless::parse_spec(actions).unwrap();
        for action in &spec.actions {
            harness.perform(action).unwrap();
        }
        assert_eq!(harness.state_mut().doc.get_heads(), heads);
    }

    #[test]
    fn suggestion_widgets_prefill_editable_private_details_before_explicit_consent() {
        let form = AccountsForm::new();
        form.select_source(Some("source".into())).unwrap();
        form.accept_hints(
            "source",
            vec![AccountHint {
                provider: Provider::Nextcloud,
                email: "private@example.org".into(),
                server: "https://cloud.example.org".into(),
                sources: account_custody::hints::HintSources::default(),
            }],
        )
        .unwrap();
        perform(
            &form,
            r#"{"actions":[{"perform":{"target":"installer.online.hint.0","action":"activate"}}]}"#,
        );
        assert_eq!(form.email.with(Clone::clone), "private@example.org");
        assert_eq!(form.server.with(Clone::clone), "https://cloud.example.org");
        assert!(form.connected.with(Vec::is_empty));
        assert!(form.drain_commands().is_empty());
        assert!(!form.files.with(|value| *value));
        assert!(!form.mail.with(|value| *value));
        assert!(!form.calendar.with(|value| *value));
        perform(
            &form,
            r#"{"actions":[
            {"perform":{"target":"installer.online.email","action":"set_value","value":"edited@example.org"}},
            {"perform":{"target":"installer.online.server","action":"set_value","value":"https://edited.example.org"}},
            {"perform":{"target":"installer.online.files","action":"toggle_check"}},
            {"perform":{"target":"installer-online-add","action":"activate"}}
        ]}"#,
        );
        assert!(
            matches!(&form.drain_commands()[..], [AccountsCommand::Add(request)]
            if request.email == "edited@example.org"
                && request.server == "https://edited.example.org"
                && request.files && !request.mail && !request.calendar)
        );
    }

    #[test]
    fn continuing_widget_cancels_pending_request_without_forgetting_connected_services() {
        let form = AccountsForm::new();
        let account = ConnectedAccount {
            provider: Provider::Google,
            identity: "private@example.org".into(),
            files: true,
            mail: false,
            calendar: true,
        };
        form.set_connected(vec![account.clone()]);
        form.submit(false).unwrap();
        perform(
            &form,
            r#"{"actions":[{"perform":{"target":"installer-online-skip","action":"activate"}}]}"#,
        );
        assert!(matches!(
            &form.drain_commands()[..],
            [AccountsCommand::SkipOffline]
        ));
        assert!(form.connected.with(|accounts| accounts == &[account]));
        assert!(form.status() == AccountsStatus::SigningIn);
    }

    #[test]
    fn connected_accounts_are_owner_supplied_and_survive_hint_source_changes() {
        let form = AccountsForm::new();
        let account = ConnectedAccount {
            provider: Provider::Microsoft,
            identity: "work@example.org".into(),
            files: false,
            mail: true,
            calendar: false,
        };
        form.set_connected(vec![account.clone()]);
        form.take_structure_changed();
        form.set_connected(vec![account.clone()]);
        assert!(!form.take_structure_changed());
        form.select_source(Some("new-source".into())).unwrap();
        assert!(form.connected.with(|connected| connected == &[account]));
        assert!(form.hints.with(Vec::is_empty));
        assert!(form.drain_commands().is_empty());
    }

    #[test]
    fn merged_suggestions_are_editable_and_never_select_services_or_start_auth() {
        let form = AccountsForm::new();
        form.select_source(Some("source".into())).unwrap();
        let files = AccountHint {
            provider: Provider::Microsoft,
            email: "person@example.org".into(),
            server: String::new(),
            sources: account_custody::hints::HintSources {
                onedrive: true,
                ..Default::default()
            },
        };
        let mut mail = files.clone();
        mail.sources = account_custody::hints::HintSources {
            outlook: true,
            ..Default::default()
        };
        form.accept_hints("source", vec![files, mail]).unwrap();
        assert_eq!(form.hints.with(Vec::len), 1);
        form.choose_hint(0).unwrap();
        assert_eq!(form.email.with(Clone::clone), "person@example.org");
        assert!(!form.files.with(|v| *v));
        assert!(!form.mail.with(|v| *v));
        assert!(!form.calendar.with(|v| *v));
        assert!(form.drain_commands().is_empty());
        assert_eq!(form.submit(false), Err(FormError::InvalidFields));
        form.email.set("personal@example.org".into());
        form.mail.set(true);
        form.submit(false).unwrap();
        assert!(
            matches!(&form.drain_commands()[..], [AccountsCommand::Add(request)]
            if request.email == "personal@example.org" && request.mail && !request.files && !request.calendar)
        );
    }

    #[test]
    fn prefilled_server_is_editable_and_unsafe_direct_handles_are_rejected() {
        let form = AccountsForm::new();
        form.select_source(Some("source".into())).unwrap();
        form.accept_hints(
            "source",
            vec![AccountHint {
                provider: Provider::Nextcloud,
                email: "person@example.org".into(),
                server: "https://cloud.example.org".into(),
                sources: account_custody::hints::HintSources::default(),
            }],
        )
        .unwrap();
        form.choose_hint(0).unwrap();
        assert_eq!(form.server.with(Clone::clone), "https://cloud.example.org");
        form.files.set(true);
        for server in [
            "https://user:password@cloud.example.org",
            "https://cloud.example.org/?token=secret",
            "https://cloud.example.org/#secret",
            "https://cloud.example.org\n",
        ] {
            form.server.set(server.into());
            assert_eq!(form.submit(false), Err(FormError::InvalidFields));
        }
        form.server.set("https://edited.example.org".into());
        form.submit(false).unwrap();
        assert!(
            matches!(&form.drain_commands()[..], [AccountsCommand::Add(request)]
            if request.server == "https://edited.example.org")
        );
    }

    #[test]
    fn submit_is_private_snapshot_and_blocks_duplicate_start() {
        let form = AccountsForm::new();
        form.email.set("person@example.org".into());
        assert_eq!(form.submit(false), Ok(()));
        form.email.set("changed@example.org".into());
        assert_eq!(form.submit(false), Err(FormError::Busy));
        let commands = form.drain_commands();
        assert!(matches!(&commands[..], [AccountsCommand::Add(request)]
            if request.email == "person@example.org" && request.files && !request.mail));
    }

    #[test]
    fn calendar_consent_is_explicit_and_does_not_require_files_or_mail() {
        let form = AccountsForm::new();
        form.files.set(false);
        assert_eq!(form.submit(false), Err(FormError::InvalidFields));
        form.calendar.set(true);
        assert_eq!(form.submit(false), Ok(()));
        assert!(
            matches!(&form.drain_commands()[..], [AccountsCommand::Add(request)]
            if request.calendar && !request.files && !request.mail)
        );
        form.set_status(AccountsStatus::Ready);
        assert_eq!(form.select_source(None), Ok(()));
        assert!(!form.calendar.with(|v| *v));
    }

    #[test]
    fn discovered_calendars_require_selection_before_confirmation() {
        let form = AccountsForm::new();
        form.set_calendars(vec![("/calendars/work/".into(), "Work".into())]);
        form.set_status(AccountsStatus::ReviewCalendars);
        assert_eq!(form.submit(false), Err(FormError::InvalidFields));
        form.collections
            .with(|collections| collections[0].2.set(true));
        form.set_calendars(vec![("/calendars/work/".into(), "Work".into())]);
        assert_eq!(form.submit(false), Ok(()));
        assert!(
            matches!(&form.drain_commands()[..], [AccountsCommand::SelectCalendars(selected)]
            if selected == &["/calendars/work/"])
        );
    }

    #[test]
    fn unsupported_services_are_not_requested() {
        let form = AccountsForm::new();
        form.provider.set(Provider::Dropbox);
        form.mail.set(true);
        form.files.set(false);
        assert_eq!(form.submit(false), Err(FormError::InvalidFields));
        form.files.set(true);
        assert_eq!(form.submit(false), Ok(()));
        assert!(
            matches!(&form.drain_commands()[..], [AccountsCommand::Add(request)] if !request.mail)
        );
    }

    #[test]
    fn microsoft_mail_is_offered_without_files() {
        assert!(Provider::Microsoft.supports_mail());
        let form = AccountsForm::new();
        form.provider.set(Provider::Microsoft);
        form.mail.set(true);
        form.files.set(false);
        assert_eq!(form.submit(false), Ok(()));
        assert!(
            matches!(&form.drain_commands()[..], [AccountsCommand::Add(request)]
            if request.mail && !request.files)
        );
    }

    #[test]
    fn structural_changes_are_consumed_without_tracking_text_or_consent() {
        let form = AccountsForm::new();
        assert!(form.take_structure_changed());
        assert!(!form.take_structure_changed());
        form.email.set("person@example.org".into());
        form.files.set(false);
        form.mail.set(true);
        assert!(!form.take_structure_changed());
        form.provider.set(Provider::Nextcloud);
        assert!(form.take_structure_changed());
        assert!(!form.take_structure_changed());
        form.set_status(AccountsStatus::Failed);
        assert!(form.take_structure_changed());
        assert_eq!(form.select_source(Some("source".into())), Ok(()));
        assert!(form.take_structure_changed());
        assert_eq!(
            form.accept_hints(
                "source",
                vec![AccountHint {
                    provider: Provider::Nextcloud,
                    email: String::new(),
                    server: "https://cloud.example.org".into(),
                    sources: account_custody::hints::HintSources::default(),
                }]
            ),
            Ok(())
        );
        assert!(form.take_structure_changed());
        assert_eq!(form.choose_hint(0), Ok(()));
        assert!(form.take_structure_changed());
        assert!(!form.take_structure_changed());
        assert_eq!(
            form.accept_hints("stale", Vec::new()),
            Err(FormError::StaleSource)
        );
        assert!(!form.take_structure_changed());
    }

    #[test]
    fn retry_uses_corrected_server_and_escape_supersedes_queue() {
        let form = AccountsForm::new();
        form.provider.set(Provider::Nextcloud);
        form.server.set("http://example.org".into());
        assert_eq!(form.submit(false), Err(FormError::InvalidFields));
        form.set_status(AccountsStatus::Failed);
        form.server.set("https://cloud.example.org".into());
        assert_eq!(form.submit(true), Ok(()));
        assert!(
            matches!(&form.drain_commands()[..], [AccountsCommand::Retry(request)]
            if request.server == "https://cloud.example.org")
        );
        form.cancel();
        assert!(matches!(
            &form.drain_commands()[..],
            [AccountsCommand::Cancel]
        ));
        form.set_status(AccountsStatus::Ready);
        for _ in 0..COMMAND_CAPACITY {
            assert_eq!(form.submit(false), Ok(()));
            form.set_status(AccountsStatus::Ready);
        }
        assert_eq!(form.submit(false), Err(FormError::QueueFull));
        form.skip_offline();
        assert!(matches!(
            &form.drain_commands()[..],
            [AccountsCommand::SkipOffline]
        ));
    }

    #[test]
    fn hints_require_current_source_and_explicit_review() {
        let form = AccountsForm::new();
        let hint = AccountHint {
            provider: Provider::Microsoft,
            email: "person@example.org".into(),
            server: String::new(),
            sources: account_custody::hints::HintSources::default(),
        };
        assert_eq!(form.select_source(Some("source-a-read-1".into())), Ok(()));
        assert_eq!(
            form.accept_hints("source-a-read-1", vec![hint.clone()]),
            Ok(())
        );
        assert!(form.email.with(String::is_empty));
        assert_eq!(form.choose_hint(0), Ok(()));
        assert_eq!(form.provider.with(|p| *p), Provider::Microsoft);
        assert!(!form.mail.with(|v| *v));
        assert!(form.drain_commands().is_empty());
        assert_eq!(form.select_source(Some("source-b-read-2".into())), Ok(()));
        assert_eq!(
            form.accept_hints("source-a-read-1", vec![hint]),
            Err(FormError::StaleSource)
        );
        assert!(form.email.with(String::is_empty));
        assert_eq!(form.choose_hint(0), Err(FormError::InvalidFields));
    }
}
