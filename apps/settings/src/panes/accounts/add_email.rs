//! Adding an email account with its secret, the published `add-email-account`
//! effect: the password goes to the workspace's vault first, through the
//! session (outside a session, to an explicit root's own vault), then the
//! account joins the workspace's accounts naming the vault entry it is kept in,
//! with the other names and addresses it sends as. An account given no secret
//! joins waiting for the person to sign it in.

use ui::framework::{EffectAnswer, EffectCx, Effected};

use super::AccountsArea;
use crate::accounts::AccountRoot;
use crate::resource::Settings;

pub(super) enum Sealing {
    Session(ui::collaboration::browser_service_sessions::Pending),
    /// Sealed already, in an explicit root's own vault, or nothing to seal.
    Done,
}

pub(super) struct AddingEmail {
    sealing: Sealing,
    root: AccountRoot,
    config: accounts::email::AccountConfig,
    identities: Vec<String>,
    reply: Option<ui::framework::EffectReply>,
}

impl AddingEmail {
    /// The outcome once the session answered: the vault entry the password is
    /// kept in (`None` where it was sealed apart, or there was none), or why not.
    pub(super) fn poll(&mut self) -> Option<Result<Option<String>, String>> {
        match &mut self.sealing {
            Sealing::Done => Some(Ok(None)),
            Sealing::Session(pending) => pending.poll().map(|reply| match reply {
                Ok(ui::collaboration::browser_service_sessions::BrowserServiceSessionReply::AccountSecretStored { account, item })
                    if account == self.config.email =>
                {
                    Ok(Some(item))
                }
                Ok(_) => Err("Account credentials answered something else".into()),
                Err(error) => Err(error),
            }),
        }
    }

    pub(super) fn finish(mut self, sealed: Result<Option<String>, String>) {
        let result = sealed.and_then(|item| {
            self.root
                .save_email_account(&self.config, &self.identities, item.as_deref())
        });
        if let Some(reply) = self.reply.take() {
            reply.finish(result.map(|()| ui::tilo_actions::ActionOutput {
                value: Some(serde_json::json!({ "account": self.config.email })),
                ..Default::default()
            }));
        }
    }
}

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
        identities,
    } = effect
    else {
        return Err("Not an email account to add".into());
    };
    let config = accounts::email::AccountConfig {
        identities: Vec::new(),
        display_name,
        username: username.unwrap_or_else(|| email.clone()),
        email,
        imap_host,
        imap_port,
        // The port says how the connection starts: 143 upgrades with
        // STARTTLS, every other port is TLS from the first byte.
        imap_tls: accounts::email::Security::of_imap(true, imap_port).wraps(),
        smtp_host,
        smtp_port,
        smtp_tls: true,
        auth: accounts::email::AuthMethod::Password,
        import_pending: password.is_none(),
        password: String::new(),
    };
    let identities = accounts::email::sender_identities(identities.unwrap_or_default())?;
    let area = super::area(app).ok_or_else(crate::area::missing)?;
    area.add_email_account(app.account_root().clone(), config, password, identities, cx)
}

impl AccountsArea {
    /// Add the email account `config` with `password`: the secret first, to
    /// the session's ownership (or, outside a session, the explicit root's
    /// vault), then the account into the person's accounts, so an account
    /// never shows without the secret it signs in with.
    pub(super) fn add_email_account(
        &self,
        root: AccountRoot,
        config: accounts::email::AccountConfig,
        password: Option<String>,
        identities: Vec<String>,
        cx: &mut EffectCx<Settings>,
    ) -> EffectAnswer {
        let invalid = config.validate();
        if let Some((_, problem)) = invalid.first() {
            return Err(problem.clone());
        }
        if password.as_ref().is_some_and(String::is_empty) {
            return Err("An email account needs its password".into());
        }
        if self.adding.borrow().is_some() {
            return Err("An email account is already being added".into());
        }
        let sealing = if let Some(password) = password.map(zeroize::Zeroizing::new) {
            self.seal(&root, &config, password)?
        } else {
            // It waits for the person to sign it in: nothing to seal.
            Sealing::Done
        };
        *self.adding.borrow_mut() = Some(AddingEmail {
            sealing,
            root,
            config,
            identities,
            reply: Some(cx.reply()),
        });
        Ok(Effected::Pending)
    }

    /// Seal `password` for the account `config`: in the session's ownership,
    /// or, outside a session, in the explicit root's own vault.
    fn seal(
        &self,
        root: &AccountRoot,
        config: &accounts::email::AccountConfig,
        password: zeroize::Zeroizing<String>,
    ) -> Result<Sealing, String> {
        Ok(if let Some(session_secrets) = &self.session_secrets {
            Sealing::Session(session_secrets.request(
                ui::collaboration::browser_service_sessions::BrowserServiceSessionCommand::StoreAccountSecret {
                    account: config.email.clone(),
                    password,
                    username: config.username.clone(),
                    servers: accounts::email::password_places(config)
                        .map_err(|error| error.to_string())?
                        .iter()
                        .map(|place| place.as_str().to_owned())
                        .collect(),
                },
                None,
            )?)
        } else if root.explicit().is_some() {
            // A profile outside a session (a test, `--data-dir`) keeps its
            // own vault; only its own root's is opened.
            root.seal_password(&accounts::AccountId::from(config.email.clone()), &password)?;
            Sealing::Done
        } else {
            return Err(
                "This device's account credentials is unavailable, so the account's password cannot be kept"
                    .into(),
            );
        })
    }

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
