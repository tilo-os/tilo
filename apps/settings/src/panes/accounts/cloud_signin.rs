//! Sign-in for the one account Settings never takes a typed password for: Nextcloud, through
//! its Login Flow v2. The person approves this device on their server's own page, and the
//! server gives the device an app password of its own.

use std::sync::Arc;

use account_credentials::nextcloud::{self, NextcloudError};
use ui::widget::{Button, Row, Spacer, TextInput, TextInputKind};

use crate::accounts::AccountKind;
use crate::channel::{self, Receiver, TryRecvError};
use crate::i18n::{t, t_args};
use provider_settings_ui::widgets::{Item, SettingsSection, field_row, note, row, rows, section};

pub trait BrowserOpener: Send + Sync {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel;
}

pub struct PortalOpener;

impl BrowserOpener for PortalOpener {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        ui::portal::send_open_uri(url)
    }
}

/// A Nextcloud sign-in the person approved: what is kept for the account.
pub struct Completed {
    pub existing_id: Option<String>,
    pub display_name: String,
    pub config: serde_json::Value,
    pub secret: accounts::Secret,
}

enum Stage {
    Form,
    Waiting {
        /// The login page still to be opened (the server names it first), and whether the
        /// session already shows it in a window it closes once the sign-in is over.
        urls: Option<Receiver<(String, bool)>>,
        launch: Option<ui::portal::LaunchChannel>,
        result: Receiver<Result<Completed, String>>,
        cancel: std::sync::mpsc::Sender<()>,
    },
}

pub struct SignInFlow {
    existing_id: Option<String>,
    opener: Arc<dyn BrowserOpener>,
    name: ui::LocalState<String>,
    server: ui::LocalState<String>,
    stage: Stage,
    ready: Option<Result<Completed, String>>,
    message: Option<String>,
}

impl SignInFlow {
    pub fn renews_an_account(&self) -> bool {
        self.existing_id.is_some()
    }

    pub fn new(existing: Option<&accounts::Account>, opener: Arc<dyn BrowserOpener>) -> Self {
        let server = existing
            .and_then(|account| account.config.get("server_url"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Self {
            existing_id: existing.map(|account| account.id.to_string()),
            opener,
            name: ui::LocalState::new(
                existing
                    .map(|account| account.display_name.clone())
                    .unwrap_or_default(),
            ),
            server: ui::LocalState::new(server),
            stage: Stage::Form,
            ready: None,
            message: None,
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Waiting { .. })
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    #[doc(hidden)]
    pub fn set_fields_for_test(&self, name: &str, server: &str) {
        self.name.set(name.into());
        self.server.set(server.into());
    }

    fn display_name(&self) -> String {
        let name = self.name.with(|name| name.trim().to_string());
        if name.is_empty() {
            AccountKind::Nextcloud.label().to_string()
        } else {
            name
        }
    }

    pub fn submit(&mut self) {
        if self.busy() {
            return;
        }
        let server = self.server.with(|server| server.trim().to_string());
        if let Err(error) = nextcloud::server_url(&server) {
            self.message = Some(error.to_string());
            return;
        }
        let (url_sender, urls) = channel::sync_channel(1);
        let (sender, result) = channel::sync_channel(1);
        let (cancel, cancelled) = std::sync::mpsc::channel();
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let _ = crate::device_read::thread(
            "settings-nextcloud-sign-in",
            Box::new(move || {
                let page = |url: String, shown: bool| {
                    let _ = url_sender.send((url, shown));
                };
                // Settings' sandbox has no network: the session follows the sign-in where it
                // connects for Settings, and Settings does only where it has a network itself.
                let outcome = match crate::account_probe::session() {
                    Some(session) => session
                        .nextcloud_sign_in(
                            &server,
                            page,
                            &cancelled,
                            nextcloud::POLL_INTERVAL,
                            nextcloud::FLOW_LIFETIME,
                        )
                        .map(|login| (login.server, login.login_name, login.app_password)),
                    None => nextcloud::begin(&server)
                        .and_then(|flow| {
                            page(flow.login_url().to_string(), false);
                            flow.wait(
                                &cancelled,
                                nextcloud::POLL_INTERVAL,
                                nextcloud::FLOW_LIFETIME,
                            )
                        })
                        .map(|login| (login.server, login.login_name, login.app_password))
                        .map_err(|error| match error {
                            NextcloudError::Cancelled => "sign-in was cancelled".to_owned(),
                            error => error.to_string(),
                        }),
                };
                let _ = sender.send(outcome.map(|(server, login_name, app_password)| Completed {
                    existing_id,
                    display_name: display_name.clone(),
                    config: serde_json::json!({
                        "label": display_name,
                        "server_url": server,
                        "username": login_name,
                        "app_password": true,
                    }),
                    secret: accounts::Secret::password(app_password.as_str()),
                }));
            }),
        );
        self.message = Some(
            "Contacting your Nextcloud server… Approve access in the browser page that opens; \
             Nextcloud then gives this device its own app password."
                .into(),
        );
        self.stage = Stage::Waiting {
            urls: Some(urls),
            launch: None,
            result,
            cancel,
        };
    }

    pub fn cancel(&mut self) {
        self.ready = None;
        if let Stage::Waiting { cancel, .. } = &self.stage {
            let _ = cancel.send(());
        }
        self.stage = Stage::Form;
    }

    pub fn poll(&mut self) -> (Option<Completed>, bool) {
        let Stage::Waiting { urls, launch, .. } = &mut self.stage else {
            return (None, false);
        };
        let changed = if let Some(receiver) = urls
            && let Ok((url, shown)) = receiver.try_recv()
        {
            if !shown {
                *launch = Some(self.opener.open(&url));
            }
            *urls = None;
            self.message = Some(
                "Continue in your browser: sign in to Nextcloud and grant access. This page \
                 updates when you are done."
                    .into(),
            );
            true
        } else {
            false
        };
        let mut launch_failed = None;
        if let Some(channel) = launch {
            match channel.try_recv() {
                Ok(ui::portal::LaunchOutcome::Opened) | Err(TryRecvError::Disconnected) => {
                    *launch = None;
                }
                Ok(ui::portal::LaunchOutcome::Failed(error)) => launch_failed = Some(error),
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(error) = launch_failed {
            self.cancel();
            self.message = Some(format!(
                "Could not open your browser: {error}. Nothing was saved."
            ));
            return (None, true);
        }
        let Stage::Waiting { result, .. } = &mut self.stage else {
            return (None, changed);
        };
        let received = match self.ready.take() {
            Some(outcome) => Ok(outcome),
            None => result.try_recv(),
        };
        let outcome = match received {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return (None, changed),
            Err(TryRecvError::Disconnected) => Err("sign-in stopped unexpectedly".into()),
        };
        self.stage = Stage::Form;
        match outcome {
            Ok(completed) => {
                // Storing the account is the system's job: the sheet closes when it lands and
                // says why if it does not.
                self.message = None;
                (Some(completed), true)
            }
            Err(error) => {
                self.message = Some(format!("Could not sign in: {error}"));
                (None, true)
            }
        }
    }

    /// Block until the running sign-in reports (opening the login page the worker learns on
    /// the way), keeping the outcome for the next [`Self::poll`]. Returns whether it reported
    /// before `timeout`.
    #[doc(hidden)]
    pub fn wait_for_test(&mut self, timeout: std::time::Duration) -> bool {
        let deadline = ui::time::now() + timeout;
        let remaining = || deadline.saturating_duration_since(ui::time::now());
        let Stage::Waiting {
            urls,
            launch,
            result,
            ..
        } = &mut self.stage
        else {
            return false;
        };
        if let Some(receiver) = urls.take() {
            match receiver.recv_timeout(remaining()) {
                Ok((url, false)) => *launch = Some(self.opener.open(&url)),
                Ok((_, true)) | Err(channel::RecvTimeoutError::Disconnected) => {}
                Err(channel::RecvTimeoutError::Timeout) => return false,
            }
        }
        match result.recv_timeout(remaining()) {
            Ok(outcome) => {
                self.ready = Some(outcome);
                true
            }
            Err(_) => false,
        }
    }

    pub fn save_failed(&mut self, error: &str) {
        self.message = Some(format!("Could not save the account: {error}"));
    }

    pub fn section(
        &self,
        bind: &dyn Fn(Button, super::AccountsEffect) -> Button,
    ) -> SettingsSection {
        let service = AccountKind::Nextcloud.label();
        let title = t_args(
            if self.existing_id.is_some() {
                "accounts-sign-in-again-to"
            } else {
                "accounts-sign-in-connect"
            },
            &[("service", service.into())],
        );
        let mut fields: Vec<Item> = Vec::new();
        let waiting = self.busy();
        if !waiting {
            self.form_fields(&mut fields);
        }
        if let Some(message) = &self.message {
            fields.push(note(message));
        }
        let mut buttons = Row::new();
        if !waiting {
            buttons = buttons.push(Box::new(bind(
                Button::new(
                    "settings.accounts.form.save",
                    &t("accounts-services-continue"),
                )
                .role(ui::widget::ButtonRole::Primary),
                super::AccountsEffect::SaveAccountForm,
            )));
        }
        buttons = buttons
            .push(Box::new(bind(
                Button::new("settings.accounts.form.cancel", &t("accounts-form-cancel"))
                    .role(ui::widget::ButtonRole::Secondary),
                super::AccountsEffect::CancelAccountForm,
            )))
            .push(Box::new(Spacer::new()));
        fields.push(row(Box::new(buttons)));
        section(&title, rows(fields))
    }

    fn form_fields(&self, fields: &mut Vec<Item>) {
        fields.push(field_row(
            &t("accounts-display-name"),
            Box::new(
                TextInput::from_state(&self.name)
                    .with_kind(TextInputKind::Name)
                    .with_id("accounts.signin.name")
                    .with_accessible_name(t("settings-a11y-display-name"))
                    .with_placeholder(AccountKind::Nextcloud.label()),
            ),
        ));
        fields.push(field_row(
            "Server",
            Box::new(
                TextInput::from_state(&self.server)
                    .with_kind(TextInputKind::Url)
                    .with_id("accounts.signin.server")
                    .with_accessible_name(t("settings-a11y-server-url"))
                    .with_placeholder("https://cloud.example.com"),
            ),
        ));
        fields.push(note(
            "Continue opens your Nextcloud's sign-in page in the browser. After you approve, \
             Nextcloud issues an app password for this device only; you can revoke it there \
             under Security at any time.",
        ));
    }
}
