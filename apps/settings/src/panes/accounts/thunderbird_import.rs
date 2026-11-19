use std::path::Path;

use accounts::thunderbird::{Candidate, DiscoveryError, Transport};
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow, ViewCx};
use ui::tilo_resource;
use ui::widget::Button;

use super::{AccountsEffect, AccountsPane};
#[cfg(test)]
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    El, SettingsSection, caption, note, row, rows, section_with, thing_row,
};

/// A Thunderbird import the pane can start, as it publishes it: a row of the
/// `settings.account-import` type, offering to inspect, choose, commit or
/// cancel the import.
#[derive(ui::tilo_resource::Row)]
#[row(kind = "settings.account-import", label = "Account import",
    effects = AccountsEffect
)]
pub(super) struct AccountImport;

/// One account a Thunderbird import found, as the pane publishes it: a row
/// of the `settings.account-import-choice` type, offering to import it.
#[derive(ui::tilo_resource::Row)]
#[row(
    kind = "settings.account-import-choice",
    label = "Account import choice",
    effects = AccountsEffect
)]
pub(super) struct AccountImportChoice;

pub(super) fn discover(
    app: &SettingsApp,
    prefs: String,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let revision = {
        let shared = crate::panes::accounts::area(app)
            .ok_or_else(crate::area::missing)?
            .model();
        let mut pane = shared.borrow_mut();
        let root = pane.account_root.clone();
        let revision = pane.import_review.begin(&root)?;
        pane.changed = true;
        revision
    };
    Ok(app.job(
        cx,
        move || accounts::thunderbird::discover(Path::new(&prefs)),
        move |app: &SettingsApp, outcome, _| finish(app, revision, outcome),
    ))
}

pub(super) fn choose(app: &SettingsApp, cx: &mut EffectCx<Settings>) -> EffectAnswer {
    if !app.reaches_devices() {
        return Err(t("accounts-import-chooser-unavailable"));
    }
    let revision = {
        let shared = crate::panes::accounts::area(app)
            .ok_or_else(crate::area::missing)?
            .model();
        let mut pane = shared.borrow_mut();
        let root = pane.account_root.clone();
        let revision = pane.import_review.begin(&root)?;
        pane.changed = true;
        revision
    };
    let title = t("accounts-import-title");
    let accept = t("accounts-import-choose");
    Ok(app.job(
        cx,
        move || {
            let result = ui::portal::PickRequest::open(&title, &accept)
                .filter(ui::portal::FileFilter::new("prefs.js").glob("prefs.js"))
                .thread_name("settings-thunderbird-metadata")
                .send_open_file()
                .recv();
            match result {
                Ok(ui::portal::OpenFileOutcome::Opened(file)) => {
                    let name = file.display_name().to_owned();
                    accounts::thunderbird::read_selected(&name, file.into_file()).map(Some)
                }
                Ok(ui::portal::OpenFileOutcome::Cancelled) => Ok(None),
                _ => Err(DiscoveryError::Read),
            }
        },
        move |app: &SettingsApp, outcome, _| match outcome {
            Ok(Some(candidates)) => finish(app, revision, Ok(candidates)),
            Ok(None) => {
                let shared = crate::panes::accounts::area(app)
                    .ok_or_else(crate::area::missing)?
                    .model();
                let mut pane = shared.borrow_mut();
                if pane.import_review.revision == revision {
                    pane.import_review.cancel()?;
                    pane.changed = true;
                }
                Ok(Effected::done())
            }
            Err(error) => finish(app, revision, Err(error)),
        },
    ))
}

fn finish(
    app: &SettingsApp,
    revision: u64,
    outcome: Result<Vec<Candidate>, DiscoveryError>,
) -> EffectAnswer {
    let shared = crate::panes::accounts::area(app)
        .ok_or_else(crate::area::missing)?
        .model();
    let mut pane = shared.borrow_mut();
    let result = pane.import_review.receive(revision, outcome);
    pane.changed = true;
    result?;
    pane.import_review.inspect().map(Effected::value)
}

impl AccountsPane {
    pub(super) fn thunderbird_import_rows(&self) -> Vec<LiveRow> {
        let bindings = ui::tilo_resource::RowBindings::new()
            .offer(AccountsEffect::INSPECT_THUNDERBIRD_IMPORT)
            .offer(AccountsEffect::CHOOSE_THUNDERBIRD_METADATA)
            .bind(
                AccountsEffect::COMMIT_THUNDERBIRD_IMPORT,
                [("revision", self.import_review.revision.into())],
            )
            .offer(AccountsEffect::CANCEL_THUNDERBIRD_IMPORT);
        let mut root = LiveRow::typed_with::<AccountImport>(
            "settings.thunderbird-import",
            t("accounts-import-title"),
            &bindings,
        );
        for choice in &self.import_review.choices {
            let source = super::import_review::identity(&choice.candidate);
            let mut bindings = ui::tilo_resource::RowBindings::new();
            for auth in ["password", "oauth"] {
                let authentication = if auth == "oauth" {
                    accounts::email::AuthMethod::OAuth
                } else {
                    accounts::email::AuthMethod::Password
                };
                if choice.candidate.reviewed_config(authentication).is_ok() {
                    bindings = bindings.bind(
                        AccountsEffect::SELECT_THUNDERBIRD_ACCOUNT,
                        [
                            ("source", source.clone().into()),
                            ("authentication", auth.into()),
                            ("selected", (!choice.selected).into()),
                            (
                                "revision",
                                serde_json::Value::from(self.import_review.revision),
                            ),
                        ],
                    );
                }
            }
            root = root.row(LiveRow::typed_with::<AccountImportChoice>(
                format!("settings.thunderbird-account:{source}"),
                choice.candidate.email.clone(),
                &bindings,
            ));
        }
        vec![root]
    }

    pub(super) fn thunderbird_import_card(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let button = Box::new(Self::bound(
            cx,
            Button::new(
                "settings.accounts.import.choose",
                &t("accounts-import-choose"),
            ),
            AccountsEffect::ChooseThunderbirdMetadata,
        ));
        let mut body = vec![note(&t("accounts-import-about"))];
        if let Some(status) = &self.import_review.status {
            body.push(note(status));
        }
        for choice in &self.import_review.choices {
            body.extend(self.import_choice_widgets(cx, choice).into_iter().map(row));
        }
        if !self.import_review.choices.is_empty() {
            body.push(row(Box::new(Self::bound(
                cx,
                Button::new(
                    "settings.accounts.import.commit",
                    &t("accounts-import-continue"),
                ),
                AccountsEffect::CommitThunderbirdImport {
                    revision: self.import_review.revision,
                },
            ))));
            body.push(row(Box::new(Self::bound(
                cx,
                Button::new(
                    "settings.accounts.import.cancel",
                    &t("accounts-import-cancel"),
                ),
                AccountsEffect::CancelThunderbirdImport,
            ))));
        }
        section_with(&t("accounts-import-title"), button, rows(body))
    }
    fn import_choice_widgets(
        &self,
        cx: &ViewCx<Settings>,
        choice: &super::import_review::Choice,
    ) -> Vec<El> {
        let mut body: Vec<El> = Vec::new();
        let candidate = &choice.candidate;
        let source = super::import_review::identity(candidate);
        let name = candidate.display_name.trim();
        let title = if name.is_empty() {
            candidate.email.clone()
        } else {
            name.to_owned()
        };
        let details: Vec<String> = if name.is_empty() {
            Vec::new()
        } else {
            vec![candidate.email.clone()]
        };
        body.push(Box::new(thing_row(
            &format!("settings.thunderbird-account:{source}"),
            &title,
            &details,
            None,
        )));
        body.push(caption(&t_args(
            "accounts-import-incoming",
            &[
                ("host", candidate.incoming.host.as_str().into()),
                ("port", u32::from(candidate.incoming.port).into()),
                ("transport", transport(candidate.incoming.transport).into()),
            ],
        )));
        body.push(caption(&t_args(
            "accounts-import-outgoing",
            &[
                ("host", candidate.outgoing.host.as_str().into()),
                ("port", u32::from(candidate.outgoing.port).into()),
                ("transport", transport(candidate.outgoing.transport).into()),
            ],
        )));
        body.push(caption(&t_args(
            "accounts-import-incoming-login",
            &[("user", candidate.incoming.username.as_str().into())],
        )));
        body.push(caption(&t_args(
            "accounts-import-outgoing-login",
            &[("user", candidate.outgoing.username.as_str().into())],
        )));
        if candidate.incoming.authentication.is_none()
            || candidate.outgoing.authentication.is_none()
        {
            body.push(caption(&t("accounts-import-auth-unspecified")));
        }
        for (auth, key, method) in [
            (
                "password",
                "accounts-import-password-choice",
                accounts::email::AuthMethod::Password,
            ),
            (
                "oauth",
                "accounts-import-oauth-choice",
                accounts::email::AuthMethod::OAuth,
            ),
        ] {
            if candidate.reviewed_config(method).is_ok() {
                body.push(Box::new(Self::bound(
                    cx,
                    Button::new(
                        &format!("settings.accounts.import.{source}.{auth}"),
                        &t(if choice.selected {
                            "accounts-import-unselect"
                        } else {
                            key
                        }),
                    ),
                    AccountsEffect::SelectThunderbirdAccount {
                        source: source.clone(),
                        authentication: auth.into(),
                        selected: !choice.selected,
                        revision: self.import_review.revision,
                    },
                )));
            }
        }
        body
    }
}

fn transport(value: Transport) -> String {
    t(match value {
        Transport::Plain => "accounts-import-plain",
        Transport::StartTls => "accounts-import-starttls",
        Transport::Tls => "accounts-import-tls",
    })
}

#[cfg(test)]
mod tests;
