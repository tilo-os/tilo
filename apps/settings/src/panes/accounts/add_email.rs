//! Adding an email account with its secret, the published
//! `add-email-account` effect: the secret goes to the session's account
//! custody first (outside a session, to an explicit root's own vault), then
//! the account joins the person's accounts.

use ui::framework::{EffectAnswer, EffectCx, Effected};

use super::AccountsArea;
use crate::accounts::AccountRoot;
use crate::resource::Settings;

/// Where an added email account's secret is on its way.
pub(super) enum Sealing {
    /// To the session's custody, which answers on the window's route.
    Custody(ui::collaboration::browser_service_sessions::Pending),
    /// Sealed already, in an explicit root's own vault.
    Done,
}

/// An email account being added ([`AccountsArea::add_email_account`]).
pub(super) struct AddingEmail {
    sealing: Sealing,
    root: AccountRoot,
    config: accounts::email::AccountConfig,
    reply: Option<ui::framework::EffectReply>,
}

impl AddingEmail {
    /// The outcome once custody answered: the secret sealed, or why not.
    pub(super) fn poll(&mut self) -> Option<Result<(), String>> {
        match &mut self.sealing {
            Sealing::Done => Some(Ok(())),
            Sealing::Custody(pending) => pending.poll().map(|reply| match reply {
                Ok(ui::collaboration::browser_service_sessions::BrowserServiceSessionReply::AccountSecretStored { account })
                    if account == self.config.email =>
                {
                    Ok(())
                }
                Ok(_) => Err("Account custody answered something else".into()),
                Err(error) => Err(error),
            }),
        }
    }

    /// With the secret sealed, add the account to the person's accounts and
    /// answer the effect.
    pub(super) fn finish(mut self, sealed: Result<(), String>) {
        let result = sealed.and_then(|()| self.root.save_email_account(&self.config));
        if let Some(reply) = self.reply.take() {
            reply.finish(result.map(|()| ui::tilo_actions::ActionOutput {
                value: Some(serde_json::json!({ "account": self.config.email })),
                ..Default::default()
            }));
        }
    }
}

/// Answer `add-email-account`.
pub(super) fn answer(
    app: &crate::settings_app::SettingsApp,
    effect: super::AccountsEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let super::AccountsEffect::AddEmailAccount {
        display_name,
        email,
        username,
        imap_host,
        imap_port,
        smtp_host,
        smtp_port,
        password,
    } = effect
    else {
        return Err("Not an email account to add".into());
    };
    let config = accounts::email::AccountConfig {
        display_name,
        username: username.unwrap_or_else(|| email.clone()),
        email,
        imap_host,
        imap_port,
        imap_tls: true,
        smtp_host,
        smtp_port,
        smtp_tls: true,
        auth: accounts::email::AuthMethod::Password,
        import_pending: false,
        password: String::new(),
    };
    let area = super::area(app).ok_or_else(crate::area::missing)?;
    area.add_email_account(app.account_root().clone(), config, password, cx)
}

impl AccountsArea {
    /// Add the email account `config` with `password`: the secret first, to
    /// the session's custody (or, outside a session, the explicit root's
    /// vault), then the account into the person's accounts, so an account
    /// never shows without the secret it signs in with.
    pub(super) fn add_email_account(
        &self,
        root: AccountRoot,
        config: accounts::email::AccountConfig,
        password: String,
        cx: &mut EffectCx<Settings>,
    ) -> EffectAnswer {
        let invalid = config.validate();
        if let Some((_, problem)) = invalid.first() {
            return Err(problem.clone());
        }
        if password.is_empty() {
            return Err("An email account needs its password".into());
        }
        if self.adding.borrow().is_some() {
            return Err("An email account is already being added".into());
        }
        let password = zeroize::Zeroizing::new(password);
        let sealing = if let Some(custody) = &self.custody {
            Sealing::Custody(custody.request(
                ui::collaboration::browser_service_sessions::BrowserServiceSessionCommand::StoreAccountSecret {
                    account: config.email.clone(),
                    password,
                },
                None,
            )?)
        } else {
            root.seal_password(&accounts::AccountId::from(config.email.clone()), &password)?;
            Sealing::Done
        };
        *self.adding.borrow_mut() = Some(AddingEmail {
            sealing,
            root,
            config,
            reply: Some(cx.reply()),
        });
        Ok(Effected::Pending)
    }

    /// Take in custody's answer for an email account being added.
    pub(super) fn poll_adding(&self) {
        let added = self
            .adding
            .borrow_mut()
            .as_mut()
            .and_then(AddingEmail::poll);
        if let Some(finish) = added {
            if let Some(adding) = self.adding.borrow_mut().take() {
                adding.finish(finish);
            }
            if let Some(model) = self.model.get()
                && let Ok(mut pane) = model.try_borrow_mut()
            {
                pane.changed = true;
            }
        }
    }
}
