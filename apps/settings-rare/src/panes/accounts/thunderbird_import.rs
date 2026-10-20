use std::path::Path;

use accounts::thunderbird::{Candidate, DiscoveryError, Transport};
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::Effect;
use ui::widget::Button;

use super::{AccountsEffect, AccountsPane};
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{El, SettingsSection, caption, note, row, rows, section_with};

pub(super) fn discover(
    app: &SettingsApp,
    prefs: String,
    cx: &mut EffectCx<Settings, SettingsApp>,
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
    Ok(crate::device_read::job(
        cx,
        move || accounts::thunderbird::discover(Path::new(&prefs)),
        move |app: &mut SettingsApp, outcome, _| finish(app, revision, outcome),
    ))
}

pub(super) fn choose(app: &SettingsApp, cx: &mut EffectCx<Settings, SettingsApp>) -> EffectAnswer {
    if !app.host().reaches_devices() {
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
    Ok(crate::device_read::job(
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
        move |app: &mut SettingsApp, outcome, _| match outcome {
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
        let mut root = LiveRow::new(
            "settings.thunderbird-import",
            "settings.account-import",
            t("accounts-import-title"),
        )
        .operation(SettingsEffect::from(AccountsEffect::InspectThunderbirdImport).bound())
        .operation(SettingsEffect::from(AccountsEffect::ChooseThunderbirdMetadata).bound())
        .operation(
            SettingsEffect::from(AccountsEffect::CommitThunderbirdImport {
                revision: self.import_review.revision,
            })
            .bound(),
        )
        .operation(SettingsEffect::from(AccountsEffect::CancelThunderbirdImport).bound());
        for choice in &self.import_review.choices {
            let source = super::import_review::identity(&choice.candidate);
            let mut row = LiveRow::new(
                format!("settings.thunderbird-account:{source}"),
                "settings.account-import-choice",
                choice.candidate.email.clone(),
            );
            for auth in ["password", "oauth"] {
                let authentication = if auth == "oauth" {
                    accounts::email::AuthMethod::OAuth
                } else {
                    accounts::email::AuthMethod::Password
                };
                if choice.candidate.reviewed_config(authentication).is_ok() {
                    row = row.operation(
                        SettingsEffect::from(AccountsEffect::SelectThunderbirdAccount {
                            source: source.clone(),
                            authentication: auth.into(),
                            selected: !choice.selected,
                            revision: self.import_review.revision,
                        })
                        .bound(),
                    );
                }
            }
            root = root.row(row);
        }
        vec![root]
    }

    pub(super) fn thunderbird_import_card(&self) -> SettingsSection {
        let button = Box::new(self.bound(
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
            body.extend(self.import_choice_widgets(choice).into_iter().map(row));
        }
        if !self.import_review.choices.is_empty() {
            body.push(row(Box::new(self.bound(
                Button::new(
                    "settings.accounts.import.commit",
                    &t("accounts-import-continue"),
                ),
                AccountsEffect::CommitThunderbirdImport {
                    revision: self.import_review.revision,
                },
            ))));
            body.push(row(Box::new(self.bound(
                Button::new(
                    "settings.accounts.import.cancel",
                    &t("accounts-import-cancel"),
                ),
                AccountsEffect::CancelThunderbirdImport,
            ))));
        }
        section_with(&t("accounts-import-title"), button, rows(body))
    }
    fn import_choice_widgets(&self, choice: &super::import_review::Choice) -> Vec<El> {
        let mut body = Vec::new();
        let candidate = &choice.candidate;
        body.push(caption(&t_args(
            "accounts-import-identity",
            &[
                ("name", candidate.display_name.as_str().into()),
                ("email", candidate.email.as_str().into()),
            ],
        )));
        body.push(caption(&t_args(
            "accounts-import-endpoints",
            &[
                ("imap", candidate.incoming.host.as_str().into()),
                ("imap-port", u32::from(candidate.incoming.port).into()),
                ("smtp", candidate.outgoing.host.as_str().into()),
                ("smtp-port", u32::from(candidate.outgoing.port).into()),
                (
                    "incoming-transport",
                    transport(candidate.incoming.transport).into(),
                ),
                (
                    "outgoing-transport",
                    transport(candidate.outgoing.transport).into(),
                ),
            ],
        )));
        body.push(caption(&t_args(
            "accounts-import-logins",
            &[
                ("imap-user", candidate.incoming.username.as_str().into()),
                ("smtp-user", candidate.outgoing.username.as_str().into()),
            ],
        )));
        if candidate.incoming.authentication.is_none()
            || candidate.outgoing.authentication.is_none()
        {
            body.push(caption(&t("accounts-import-auth-unspecified")));
        }
        let source = super::import_review::identity(candidate);
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
                body.push(Box::new(self.bound(
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
