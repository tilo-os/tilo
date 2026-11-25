//! A workspace's mail accounts, calendar accounts and model providers, kept
//! by the host the workspace runs on.
//!
//! A page has no mail connection and no vault, so the panes that add a mail
//! account or a provider key from a browser ask the host: it opens the mail
//! server's session to prove the sign-in, keeps the account and the key in
//! the workspace's vault, and answers what it kept. A secret travels in and
//! never back — a reading says only whether one is kept.
//!
//! They are the workspace's: one store per workspace
//! ([`Workspaces::workspace_root`]), for all its members, reached as the
//! workspace's policy says ([`super::workspace_stores`]). The stores are the
//! shared account and settings formats, so a device that reads them keeps
//! reading the same files.

/// The device's cloud-storage mounts: what the accounts' sync actions tell them.
pub(in crate::workspaces) mod cloud_control;
/// The sign-ins of the accounts and providers, as entries of the workspace's vault.
pub(in crate::workspaces) mod kept_sign_ins;
pub(in crate::workspaces) mod oauth_grant;
/// Signing in on a provider's own page: a Nextcloud server's login flow.
mod sign_in;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_account_probe::AccountTester;
use tilo_actions::remote::model_providers::{
    Assignment, ModelProviders, OfferedModel, ProviderView, SetAssignment, SetKey,
};
use tilo_actions::remote::person_accounts::{AddMailAccount, MailAccount, MailAccounts};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, model_providers, person_accounts,
};
use tilo_net::proxy::host::Destinations;
use tilo_user_identity::DeviceCert;

use super::model_providers::kept_location;
use super::vault::VaultAt;
use super::workspace_stores::Store;
use super::{Workspaces, system_time};
use kept_sign_ins::{Signing, places_of, sign_in_of};

mod person_mail;
#[cfg(feature = "agents")]
use person_mail::answers_in_public;
use person_mail::{kept_mail, mail_accounts};

impl Workspaces {
    /// Where this host keeps what may interrupt `person`. Theirs in every
    /// workspace, and nothing a workspace keeps.
    pub(super) fn person_settings_root(&self, person: &[u8; 32]) -> PathBuf {
        self.inner.dir.join("people").join(hex::encode(person))
    }

    /// The mail accounts `workspace` keeps, as [`person_accounts::LIST`]
    /// answers them: what a window following [`person_accounts::WATCH`] is
    /// sent again whenever they change.
    pub(super) async fn accounts_listing(&self, workspace: &str) -> Result<Value, ActionCallError> {
        let held = self
            .workspace_accounts(workspace)
            .await
            .map_err(ActionCallError::refused)?;
        answer(&Ok::<_, String>(mail_accounts(
            &held,
            &self.workspace_root(workspace),
        )))
    }

    /// What a workspace's accounts or providers kept here changed: a window
    /// following them reads them again.
    pub(super) fn person_settings_changed(&self) {
        self.inner
            .person_settings_moved
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Fires whenever a workspace's accounts or providers kept here change.
    #[must_use]
    pub fn person_settings_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.person_settings_moved.subscribe()
    }

    /// Keep the mail account `address` in `workspace` with a sign-in, as
    /// adding it from a window keeps one.
    ///
    /// # Errors
    /// The account or its sign-in could not be kept.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn keep_mail_account_as_added(
        &self,
        workspace: &str,
        address: &str,
    ) -> Result<(), String> {
        let id = accounts::AccountId::from(address.to_owned());
        let mut account = accounts::Account::new(id, accounts::email::TYPE_ID);
        // The stand-in is every server: the account names one of its own, which its password
        // belongs to as an added account's does.
        account.config = serde_json::json!({
            "email": address,
            "username": address,
            "imap_host": "imap.stand-in.test",
            "imap_port": 993,
            "smtp_host": "smtp.stand-in.test",
            "smtp_port": 465,
        });
        let asker = self.acting_or_own(None);
        let reference = self
            .vault_for(workspace, &asker)
            .keep_sign_in(
                address,
                (accounts::vault_items::ItemKind::Login, places_of(&account)?),
                address,
                zeroize::Zeroizing::new("kept-for-a-test".to_owned()),
                None,
            )
            .await?;
        accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
        self.keep_workspace_account((workspace, &asker, None), &account)
            .await?;
        self.person_settings_changed();
        Ok(())
    }

    /// What proves an account's sign-in at its server before the account is
    /// kept, for a person's own add and for a use by reference alike: one
    /// tester, whose connections go only where this server's mail may.
    pub(super) fn account_tester(&self) -> Arc<dyn AccountTester> {
        Arc::clone(
            &self
                .inner
                .vault_uses
                .prover
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Prove the sign-ins of accounts added by reference through `prove`
    /// from now on, which is told the account and the password it would
    /// sign in with: a test stands in for the mail servers.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sign_in_accounts_through(
        &self,
        prove: impl Fn(&str, &str) -> Result<String, String> + Send + Sync + 'static,
    ) {
        struct Proving<F>(F);
        impl<F: Fn(&str, &str) -> Result<String, String> + Send + Sync> AccountTester for Proving<F> {
            fn test(
                &self,
                account: &accounts::Account,
                secret: &accounts::Secret,
            ) -> Result<String, String> {
                (self.0)(account.id.as_str(), &secret.password)
            }
        }
        *self
            .inner
            .vault_uses
            .prover
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(Proving(prove));
    }

    /// Sign in to mail servers trusting `extra` beside the system's own authorities from now
    /// on: a test's mail server, whose certificate no authority signed, answers over real
    /// sockets. Its connections go where they did before.
    #[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
    pub fn trust_mail_servers(&self, extra: Vec<rustls::pki_types::CertificateDer<'static>>) {
        let reach = self.inner.models.mail_reach();
        *self
            .inner
            .vault_uses
            .prover
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Arc::new(tilo_account_probe::TrustingMailTester {
                reach: reach.clone(),
                extra: extra.clone(),
            });
    }

    /// Work out where an address's mail lives through `find` from now on, which is told the
    /// address and may ask whether a server of a name may be asked anything. Sign-ins are
    /// proved by whatever proved them before: a test stands in for the network.
    #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
    pub fn find_mail_servers_through(
        &self,
        find: impl Fn(
            &str,
            &dyn Fn(&str) -> bool,
        ) -> Result<tilo_account_probe::mail_servers::MailServers, String>
        + Send
        + Sync
        + 'static,
    ) {
        struct Finding<F>(Arc<dyn AccountTester>, F);
        impl<F> AccountTester for Finding<F>
        where
            F: Fn(
                    &str,
                    &dyn Fn(&str) -> bool,
                ) -> Result<tilo_account_probe::mail_servers::MailServers, String>
                + Send
                + Sync,
        {
            fn find_mail_servers(
                &self,
                address: &str,
                allowed: &dyn Fn(&str) -> bool,
            ) -> Result<tilo_account_probe::mail_servers::MailServers, String> {
                (self.1)(address, allowed)
            }

            fn test(
                &self,
                account: &accounts::Account,
                secret: &accounts::Secret,
            ) -> Result<String, String> {
                self.0.test(account, secret)
            }
        }
        let mut prover = self
            .inner
            .vault_uses
            .prover
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let proving = Arc::clone(&prover);
        *prover = Arc::new(Finding(proving, find));
    }

    /// Control this device's cloud mounts through `cloud` from now on: a
    /// test's service stands in for the files service that mounts them.
    #[cfg(any(test, feature = "test-support"))]
    pub fn cloud_control_through(&self, cloud: std::sync::Arc<dyn cloud_control::CloudControl>) {
        *self
            .inner
            .cloud_control
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cloud);
    }

    /// What controls this device's cloud mounts: a test's service, else the
    /// files service that mounts them here.
    pub(in crate::workspaces) fn cloud_control(
        &self,
    ) -> std::sync::Arc<dyn cloud_control::CloudControl> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(cloud) = self
            .inner
            .cloud_control
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return cloud;
        }
        std::sync::Arc::new(cloud_control::DeviceCloud::here())
    }

    /// Keep iCloud accounts' calendars through `server_url` from now on: a
    /// test's calendar server stands in for iCloud's own.
    #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
    pub fn keep_icloud_calendars_at(&self, server_url: &str) {
        *self
            .inner
            .icloud_calendars_at
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(server_url.to_owned());
    }

    /// Sign drives in through `store` from now on: a test's stand-in provider
    /// stands in for Google's, Microsoft's or Dropbox's.
    #[cfg(any(test, feature = "test-support"))]
    pub fn cloud_sign_in_through(&self, store: account_credentials::CredentialStore) {
        *self
            .inner
            .cloud_sign_in
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(store);
    }

    /// Sign an `Apple ID` in through `auth` from now on: a test's stand-in
    /// stands in for Apple's own.
    #[cfg(any(test, feature = "test-support"))]
    pub fn icloud_sign_in_through(&self, auth: Arc<dyn account_credentials::icloud::ICloudAuth>) {
        *self
            .inner
            .icloud_sign_in
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(auth);
    }

    /// The person `caller` asks for, on the device `certificate` certifies,
    /// from a workspace `workspace_id` names: what a request that keeps a
    /// person's own secrets needs before it runs. Unknown and forbidden are
    /// the same answer.
    pub(super) fn asking_person(
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> Result<[u8; 32], ActionCallError> {
        workspace_id.ok_or(ActionCallError::UnknownAction)?;
        if !caller.acting().is_person() || caller.acting().app().is_some() {
            return Err(ActionCallError::UnknownAction);
        }
        let certificate = certificate
            .filter(|certificate| {
                caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionCallError::UnknownAction)?;
        Ok(certificate.user_public_key)
    }

    /// Answer a [`person_accounts::PROVIDER`] request: the workspace's
    /// accounts, as the workspace's policy lets the caller reach them at Mail
    /// ([`Store::MAIL`]).
    pub(super) async fn person_accounts_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let action = request.target.address.action.as_str();
        let effect = mail_effect(action);
        let asked = (workspace_id, caller, certificate);
        let reach = self
            .reaching_allowed(asked, Store::MAIL, action, effect, request)
            .await?;
        let here = reach.workspace.to_string();
        let root = self.workspace_root(&here);
        let by = reach.by();
        let signing = Signing {
            vault: self.vault_for(&here, &reach.acting).serving(request),
            root: &root,
            by: &by,
        };
        // Signing in on a provider's own page: begun, stopped, and the
        // accounts as they are.
        if matches!(
            action,
            person_accounts::BEGIN_SIGN_IN
                | person_accounts::BEGIN_CLOUD_SIGN_IN
                | person_accounts::SUBMIT_SIGN_IN_CODE
                | person_accounts::CANCEL_SIGN_IN
        ) {
            return sign_in::accounts_sign_in(self, reach.person, &here, request).await;
        }
        // An account added from an entry of the workspace's vault is that entry's use,
        // decided at the vault as well: the same path a provider's key takes.
        if action == person_accounts::ADD_MAIL_WITH_ITEM {
            return Box::pin(self.vault_use_invoke(workspace_id, caller, certificate, request))
                .await;
        }
        // What changes the accounts the workspace keeps is answered before
        // the rest: finding servers, adding an account, signing one in again,
        // saying what it feeds and removing it.
        if let Some(answered) =
            Box::pin(self.person_mail_action(signing, &here, &reach.acting, request)).await?
        {
            return Ok(answered);
        }
        // The workspace's mail accounts, without their secrets.
        if action == person_accounts::LIST {
            let held = signing
                .vault
                .accounts()
                .await
                .map_err(ActionCallError::refused)?;
            let mut listed = mail_accounts(&held, &root);
            if reach.agent.is_some() {
                // Whose sign-in is running is no agent's business.
                listed.signing_in.clear();
            }
            return answer(&Ok::<_, String>(listed));
        }
        Err(ActionCallError::UnknownAction)
    }
}

/// What `action` of the workspace's mail accounts does, as the workspace's policy decides it:
/// a reading, or a change kept in the accounts record and the vault. What leaves — a message
/// sent — is Mail's action, decided where the app's own actions are.
pub(super) fn mail_effect(action: &str) -> tilo_action_policy::ActionKind {
    use tilo_action_policy::ActionKind;
    match action {
        person_accounts::LIST
        | person_accounts::WATCH
        | person_accounts::FIND_SERVERS
        | person_accounts::SIGN_INS => ActionKind::Read,
        _ => ActionKind::Undoable,
    }
}

/// Turn one action's answer into what the caller reads, or the reason it was
/// refused.
pub(super) fn answer<T: serde::Serialize>(
    outcome: &Result<T, String>,
) -> Result<Value, ActionCallError> {
    match outcome {
        Ok(value) => serde_json::to_value(value).map_err(|error| ActionCallError::Refused {
            reason: error.to_string(),
        }),
        Err(reason) => Err(ActionCallError::Refused {
            reason: reason.clone(),
        }),
    }
}

/// The arguments of a request, as the value its action names.
pub(super) fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionCallError> {
    let InvokeArgument::Json(argument) = &request.argument else {
        return Err(ActionCallError::invalid_request(format!(
            "a request names {what}"
        )));
    };
    serde_json::from_value(Value::Object(argument.clone())).map_err(|error| {
        ActionCallError::invalid_request(format!(
            "{what} is not readable: {}",
            without_values(&error)
        ))
    })
}

/// `error` said without the value it was about, which may be a password:
/// serde names a field it missed between backticks, and quotes a value it
/// could not read (`invalid type: string "…", expected u16`), so its words
/// are kept up to the first quoted value.
fn without_values(error: &serde_json::Error) -> String {
    let said = error.to_string();
    match said.split_once('"') {
        Some((before, _)) => before.trim_end().to_owned(),
        None => said,
    }
}

/// The sign-in a mail account of `workspace` reads with, as the session would be given it.
#[cfg(test)]
pub(crate) async fn account_secret_for_test(
    workspaces: &Workspaces,
    workspace: &str,
    id: &str,
) -> Result<accounts::Secret, String> {
    let root = workspaces.workspace_root(workspace);
    let asker = workspaces.acting_or_own(None);
    let signing = Signing {
        vault: workspaces.vault_for(workspace, &asker),
        root: &root,
        by: "",
    };
    account_with_secret(signing, id)
        .await
        .map(|(_, secret)| secret)
}

/// The provider `account` belongs to, as a workspace's accounts reading names it.
pub(crate) fn provider_key(account: &accounts::Account) -> String {
    serde_json::to_value(person_mail::provider_of_account(account))
        .ok()
        .and_then(|provider| provider.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The services `account` feeds, as its row reads them.
pub(crate) fn services_of(account: &accounts::Account) -> person_accounts::Services {
    person_mail::services_of(account, person_mail::provider_of_account(account))
}

/// The `provider` a removing action names.
pub(super) fn provider_argument(request: &InvokeRequest) -> Result<String, ActionCallError> {
    let value: Map<String, Value> = argument(request, "the provider")?;
    value
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ActionCallError::invalid_request("the provider is named by its id"))
}

/// Add `adding` once `tester` saw its server accept its sign-in: the secret
/// into the workspace's vault first, then the account into the shared store, so
/// an account never shows without the sign-in it needs. Where the sign-in is an
/// entry the vault keeps already (`kept`), the account names that entry and the
/// password `adding` carries is only what is proved; nothing is kept twice. A
/// test's tester proves the store's side without a server.
pub(super) async fn add_mail_with(
    tester: Arc<dyn AccountTester>,
    (vault, root): (VaultAt<'_>, &Path),
    adding: AddMailAccount,
    kept: Option<tilo_actions::remote::person_vault::ItemRef>,
    provider: person_accounts::Provider,
    linked: Option<(accounts::Account, String)>,
) -> Result<MailAccount, String> {
    let keeps_identities = adding.identities.is_empty();
    let config = accounts::email::AccountConfig {
        display_name: adding.display_name.trim().to_owned(),
        username: adding
            .username
            .filter(|username| !username.trim().is_empty())
            .unwrap_or_else(|| adding.address.trim().to_owned()),
        email: adding.address.trim().to_owned(),
        imap_host: adding.incoming_host.trim().to_owned(),
        imap_port: adding.incoming_port,
        // The port says how the connection starts: 143 upgrades with
        // STARTTLS, every other port is TLS from the first byte.
        imap_tls: accounts::email::Security::of_imap(true, adding.incoming_port).wraps(),
        smtp_host: adding.outgoing_host.trim().to_owned(),
        smtp_port: adding.outgoing_port,
        smtp_tls: true,
        auth: accounts::email::AuthMethod::Password,
        // An account another mail program moved in arrives with its servers
        // and its identities. With the sign-in that program saved it is
        // proved and kept at once; without one it is kept waiting for the
        // sign-in its person completes in Settings.
        import_pending: adding.password.is_empty(),
        password: String::new(),
        identities: accounts::email::sender_identities(adding.identities)?,
    };
    if let Some((_, problem)) = config.validate().first() {
        return Err(problem.clone());
    }
    let mut account = accounts::Account::new(
        accounts::AccountId::from(config.email.clone()),
        accounts::email::TYPE_ID,
    );
    if config.display_name.is_empty() {
        config.email.clone_into(&mut account.display_name);
    } else {
        config.display_name.clone_into(&mut account.display_name);
    }
    // Which of its services its person uses: what every later reading
    // answers and what Mail reads it by.
    let mut settings = config.to_value().map_err(|error| error.to_string())?;
    if let Value::Object(settings) = &mut settings {
        settings.insert(
            "services".into(),
            serde_json::to_value(adding.services.within(provider))
                .map_err(|error| error.to_string())?,
        );
        // The calendar account kept as its service: what reads the link back
        // when the account's services, or the account itself, go.
        if let Some((calendar, server)) = &linked {
            settings.insert("calendar_account".into(), calendar.id.as_str().into());
            settings.insert("server".into(), server.clone().into());
        }
    }
    account.config = settings;

    // An edit that names no identities keeps the ones the account had, as it keeps every other
    // setting this form does not own.
    let held = vault.accounts().await?;
    let existing = held.account(account.id.as_str());
    let before = existing
        .as_ref()
        .and_then(|before| sign_in_of(&before.config));
    if keeps_identities
        && let Some(identities) =
            existing.and_then(|before| before.config.get("identities").cloned())
        && let Some(settings) = account.config.as_object_mut()
    {
        settings.insert("identities".into(), identities);
    }
    // An edit that keeps the sign-in the account had is not waiting for one.
    if adding.password.is_empty()
        && before.is_some()
        && let Some(settings) = account.config.as_object_mut()
    {
        settings.insert("import_pending".into(), Value::Bool(false));
    }
    if !adding.password.is_empty() {
        let checking = account.clone();
        let secret = accounts::Secret::password(adding.password.clone());
        tokio::task::spawn_blocking(move || tester.test(&checking, &secret))
            .await
            .map_err(|_| "the mail server check did not finish".to_string())??;
        let reference = match kept {
            Some(kept) => {
                // An account kept before with a sign-in of its own lets that one go.
                if let Some(before) = &before
                    && before != &kept
                {
                    vault.forget_sign_in(account.id.as_str(), before).await?;
                }
                kept
            }
            None => {
                vault
                    .keep_sign_in(
                        account.id.as_str(),
                        (accounts::vault_items::ItemKind::Login, places_of(&account)?),
                        &config.username,
                        zeroize::Zeroizing::new(adding.password),
                        before.as_ref(),
                    )
                    .await?
            }
        };
        accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
    } else if let Some(before) = &before {
        // Kept again without a password, it keeps the sign-in it had.
        accounts::email::set_password_item(&mut account.config, Some(&before.to_string()));
    }
    // One change keeps every account this action makes: the mail account and,
    // where its provider keeps calendars, the calendar account beside it.
    match linked {
        Some((calendar, _)) => {
            vault
                .keep_and_remove_accounts(&[account.clone(), calendar], &[])
                .await?;
        }
        None => vault.keep_account(&account).await?,
    }
    kept_mail(&vault.accounts().await?, root, account.id.as_str())
}

/// Remove `account` and its own sign-in together. A member's entry it signed in with stays
/// theirs.
async fn remove_mail(
    (vault, root): (VaultAt<'_>, &Path),
    account: &str,
) -> Result<MailAccounts, String> {
    let held = vault.accounts().await?;
    let kept = held.account(account);
    if kept.is_none() {
        return Err("That account is not kept here".into());
    }
    let sign_in = kept.as_ref().and_then(|kept| sign_in_of(&kept.config));
    // The calendar account kept as its service goes with it, in one change
    // to the accounts record.
    let linked = kept.as_ref().and_then(|kept| {
        linked_calendar(
            &held,
            kept.config.get("calendar_account").and_then(Value::as_str),
            account,
        )
    });
    let linked_sign_in = linked
        .as_ref()
        .and_then(|id| held.account(id))
        .and_then(|kept| sign_in_of(&kept.config));
    let mut remove = vec![account];
    if let Some(calendar) = &linked {
        remove.push(calendar.as_str());
    }
    vault.keep_and_remove_accounts(&[], &remove).await?;
    if let Some(sign_in) = &sign_in {
        vault.forget_sign_in(account, sign_in).await?;
    }
    if let Some(calendar) = &linked
        && let Some(sign_in) = &linked_sign_in
    {
        vault.forget_sign_in(calendar, sign_in).await?;
    }
    Ok(mail_accounts(&vault.accounts().await?, root))
}

/// The calendar account kept as the mail account `mail`'s service: the one
/// it names, else the one kept for it.
fn linked_calendar(
    held: &accounts::person::AccountList,
    named: Option<&str>,
    mail: &str,
) -> Option<String> {
    let accounts = held.of_type(accounts::calendar::TYPE_ID);
    if let Some(named) = named
        && accounts.iter().any(|account| account.id.as_str() == named)
    {
        return Some(named.to_owned());
    }
    accounts
        .into_iter()
        .find(|account| account.config.get("mail_account").and_then(Value::as_str) == Some(mail))
        .map(|account| account.id.to_string())
}

/// The account `id` names, with the sign-in the workspace's vault keeps for it, the use
/// written down: what a page cannot read for itself, and what the host opens the server's
/// session with.
async fn account_with_secret(
    signing: Signing<'_>,
    id: &str,
) -> Result<(accounts::Account, accounts::Secret), String> {
    let account = signing
        .vault
        .accounts()
        .await?
        .account(id)
        .filter(|account| account.type_id.as_str() == accounts::email::TYPE_ID)
        .ok_or_else(|| "That account is not kept here".to_owned())?;
    // It reads with the entry it names, while its settings name the servers
    // that entry belongs to.
    let secret = sign_in::mail_secret(signing.vault, signing.root, &account, signing.by).await?;
    Ok((account, secret))
}

pub(super) fn providers(root: &Path) -> ModelProviders {
    providers_of(&provider_config(root))
}

/// `config`'s providers as the pane reads them, each saying whether it names a key.
fn providers_of(config: &agent_provider_config::ProviderConfig) -> ModelProviders {
    ModelProviders {
        providers: config.profiles.iter().map(view_of).collect(),
        default_provider: config.default_profile.clone(),
        assignments: config
            .interaction_models
            .iter()
            .map(|(kind, chosen)| Assignment {
                kind: kind.clone(),
                provider: chosen.profile.clone(),
                model: chosen.model.clone(),
            })
            .collect(),
        presets: agent_provider_config::presets::Preset::ALL
            .iter()
            .copied()
            .filter(|preset| !preset.signs_in_only())
            .map(|preset| model_providers::ProviderPreset {
                preset: preset.slug().to_owned(),
                name: preset.label().to_owned(),
                description: preset.description().to_owned(),
                protocol: match preset.protocol() {
                    agent_provider_config::ProviderProtocol::OpenAiCompatible => {
                        model_providers::Protocol::OpenAiCompatible
                    }
                    agent_provider_config::ProviderProtocol::Anthropic => {
                        model_providers::Protocol::Anthropic
                    }
                },
            })
            .collect(),
    }
}

/// One provider as the pane reads it.
pub(super) fn view_of(profile: &agent_provider_config::ProviderProfile) -> ProviderView {
    ProviderView {
        id: profile.id.clone(),
        name: profile.label.clone(),
        protocol: match profile.protocol {
            agent_provider_config::ProviderProtocol::OpenAiCompatible => {
                model_providers::Protocol::OpenAiCompatible
            }
            agent_provider_config::ProviderProtocol::Anthropic => {
                model_providers::Protocol::Anthropic
            }
        },
        endpoint: profile.endpoint.clone(),
        configured: key_kept(profile),
        enabled: profile.enabled,
        signs_in: profile.sign_in.is_some(),
        models: profile
            .models
            .iter()
            .map(|model| OfferedModel {
                id: model.id.clone(),
                label: model.label.clone(),
            })
            .collect(),
        default_model: profile.default_model.clone(),
        reasoning: profile
            .reasoning
            .as_ref()
            .filter(|kept| kept.param != agent_provider_config::ReasoningParam::Omit)
            .map(|kept| kept.effort.clone()),
        headers: profile
            .headers
            .iter()
            .map(|header| model_providers::RequestHeader {
                name: header.name.clone(),
                value: header.value.clone(),
            })
            .collect(),
    }
}

/// The person's own model configuration.
pub(super) fn provider_config(root: &Path) -> agent_provider_config::ProviderConfig {
    let store = settings_store::profile(root);
    agent_provider_config::ProviderConfig::load_in(&*store)
}

pub(super) fn save_provider_config(
    root: &Path,
    config: &agent_provider_config::ProviderConfig,
) -> Result<(), String> {
    let store = settings_store::profile(root);
    config.save_in(&*store).map_err(|error| error.to_string())
}

/// The entry of the workspace's vault `profile` names as its key, or as the grant its
/// sign-in left.
pub(super) fn key_reference(
    profile: &agent_provider_config::ProviderProfile,
) -> Option<tilo_actions::remote::person_vault::ItemRef> {
    let named = match &profile.sign_in {
        Some(sign_in) => sign_in.kept.as_ref(),
        None => profile.credential.as_ref(),
    };
    named
        .and_then(agent_provider_config::SecretReference::workspace_vault_item)
        .and_then(tilo_actions::remote::person_vault::ItemRef::parse)
}

/// Whether `profile` names a key, or a grant, in the workspace's vault.
pub(super) fn key_kept(profile: &agent_provider_config::ProviderProfile) -> bool {
    key_reference(profile).is_some()
}

/// Name `reference` as `profile`'s key, or as its sign-in's grant, or with `None` name none.
fn name_key(
    profile: &mut agent_provider_config::ProviderProfile,
    reference: Option<&tilo_actions::remote::person_vault::ItemRef>,
) {
    let named = reference.map(|reference| {
        agent_provider_config::SecretReference::workspace_vault(&reference.to_string())
    });
    match &mut profile.sign_in {
        Some(sign_in) => sign_in.kept = named,
        None => profile.credential = named,
    }
}

/// The place a provider's key is sent to: its endpoint's origin.
fn key_place(
    profile: &agent_provider_config::ProviderProfile,
) -> Result<accounts::vault_items::Place, String> {
    accounts::vault_items::Place::parse(&profile.endpoint)
        .ok()
        .filter(accounts::vault_items::Place::is_web)
        .ok_or_else(|| "A provider's endpoint is a URL".to_owned())
}

/// The workspace's provider keys: entries of its vault, each named by its provider's settings
/// ([`key_reference`]) and bound to the origin of its provider's endpoint, so a key is only
/// ever sent where it was entered for. Each key read is a use the entry writes down, for `by`.
#[derive(Clone, Copy)]
pub(super) struct ProviderKeys<'a> {
    vault: VaultAt<'a>,
    by: &'a str,
}

impl<'a> ProviderKeys<'a> {
    /// The keys in `vault`, read for `by` (empty for a person or this host itself).
    pub(super) const fn of(vault: VaultAt<'a>, by: &'a str) -> Self {
        Self { vault, by }
    }

    /// The vault the keys are entries of.
    pub(super) const fn vault(&self) -> VaultAt<'a> {
        self.vault
    }

    /// The key `profile` is asked with, the use written down: none where it names none, or
    /// where the entry it names is gone or belongs to another origin than its endpoint's, which
    /// it is never sent to.
    pub(super) async fn get(
        &self,
        profile: &agent_provider_config::ProviderProfile,
    ) -> Result<Option<zeroize::Zeroizing<String>>, String> {
        let Some(reference) = key_reference(profile) else {
            return Ok(None);
        };
        let place = key_place(profile)?;
        let used = self
            .vault
            .try_sign_in(
                &reference,
                (
                    accounts::vault_items::ItemKind::Key,
                    std::slice::from_ref(&place),
                ),
                self.by,
                PROVIDER_CALL,
            )
            .await?;
        Ok(used.map(|released| released.secret))
    }

    /// Keep `key` as `profile`'s own key (or its sign-in's grant) in place of the one it kept
    /// before, and name it in `profile` for whoever keeps the settings to save.
    pub(super) async fn put(
        &self,
        profile: &mut agent_provider_config::ProviderProfile,
        key: &str,
    ) -> Result<(), String> {
        let place = key_place(profile)?;
        let reference = self
            .vault
            .keep_sign_in(
                &kept_sign_ins::provider_owner(&profile.id),
                (accounts::vault_items::ItemKind::Key, vec![place]),
                "",
                zeroize::Zeroizing::new(key.to_owned()),
                key_reference(profile).as_ref(),
            )
            .await?;
        name_key(profile, Some(&reference));
        Ok(())
    }

    /// Name the entry `reference` a member keeps as `profile`'s key: the key stays theirs, and
    /// the key the provider kept of its own before goes.
    pub(super) async fn name(
        &self,
        profile: &mut agent_provider_config::ProviderProfile,
        reference: &tilo_actions::remote::person_vault::ItemRef,
    ) -> Result<(), String> {
        if let Some(before) = key_reference(profile)
            && &before != reference
        {
            self.vault
                .forget_sign_in(&kept_sign_ins::provider_owner(&profile.id), &before)
                .await?;
        }
        name_key(profile, Some(reference));
        Ok(())
    }

    /// Let `profile`'s key go: its own entry is forgotten, a member's stays theirs, and
    /// `profile` names none.
    pub(super) async fn remove(
        &self,
        profile: &mut agent_provider_config::ProviderProfile,
    ) -> Result<(), String> {
        if let Some(before) = key_reference(profile) {
            self.vault
                .forget_sign_in(&kept_sign_ins::provider_owner(&profile.id), &before)
                .await?;
        }
        name_key(profile, None);
        Ok(())
    }
}

/// What a provider's key is used for: calling the provider.
const PROVIDER_CALL: accounts::vault_items::UsedFor<'static> = accounts::vault_items::UsedFor {
    using: "model-call",
    what: "",
    allowed: None,
};

/// Keep the provider `adding` names, at an address this host calls
/// (`destinations`, [`kept_location`]).
pub(super) fn add_provider(
    root: &Path,
    adding: &model_providers::AddProvider,
    destinations: Destinations,
) -> Result<ProviderView, String> {
    let name = adding.name.trim();
    if name.is_empty() {
        return Err("A provider needs a name".into());
    }
    let endpoint = adding.endpoint.trim();
    let location = kept_location(endpoint, destinations)?;
    let mut config = provider_config(root);
    let protocol = match adding.protocol {
        model_providers::Protocol::OpenAiCompatible => {
            agent_provider_config::ProviderProtocol::OpenAiCompatible
        }
        model_providers::Protocol::Anthropic => agent_provider_config::ProviderProtocol::Anthropic,
    };
    let headers = adding
        .headers
        .iter()
        .map(|header| {
            let header = agent_provider_config::ProviderHeader {
                name: header.name.trim().to_owned(),
                value: header.value.clone(),
            };
            header.check().map(|()| header)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let profile = agent_provider_config::ProviderProfile {
        id: uuid::Uuid::new_v4().to_string(),
        label: name.to_owned(),
        protocol,
        endpoint: endpoint.to_owned(),
        location,
        credential: None,
        enabled: true,
        headers,
        ..Default::default()
    };
    let view = view_of(&profile);
    config.profiles.push(profile);
    save_provider_config(root, &config)?;
    Ok(view)
}

/// Offer `setting`'s provider to the workspace's agents, or stop offering it, and answer the
/// provider as kept. A provider stopped keeps its key and its models; the kinds of work assigned
/// to it follow the default again, so no work runs on a provider the person stopped.
pub(super) fn set_enabled(
    root: &Path,
    setting: &model_providers::SetEnabled,
) -> Result<ProviderView, String> {
    let mut config = provider_config(root);
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == setting.provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    profile.enabled = setting.enabled;
    let view = view_of(profile);
    if !setting.enabled {
        if config.default_profile.as_deref() == Some(setting.provider.as_str()) {
            config.default_profile = None;
        }
        config
            .interaction_models
            .retain(|_, chosen| chosen.profile != setting.provider);
    }
    save_provider_config(root, &config)?;
    Ok(view)
}

/// Keep `setting`'s key as its provider's: typed, its own entry of the workspace's vault; or,
/// where `kept` names a member's entry it was read from, that entry, which is not copied.
pub(super) async fn set_key(
    (keys, root): (ProviderKeys<'_>, &Path),
    setting: SetKey,
    kept: Option<&tilo_actions::remote::person_vault::ItemRef>,
) -> Result<ProviderView, String> {
    let SetKey { provider, key } = setting;
    let key = zeroize::Zeroizing::new(key);
    if key.trim().is_empty() {
        return Err("An API key cannot be empty".into());
    }
    let mut profile = provider_config(root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    match kept {
        Some(kept) => keys.name(&mut profile, kept).await?,
        None => keys.put(&mut profile, key.trim()).await?,
    }
    save_named_key(root, &profile)
}

/// Keep the key `profile` names now in the settings under `root`, read again after the vault
/// was written so a change made to them meanwhile stays, answering the provider as kept.
pub(super) fn save_named_key(
    root: &Path,
    profile: &agent_provider_config::ProviderProfile,
) -> Result<ProviderView, String> {
    let mut config = provider_config(root);
    let named = config
        .profiles
        .iter_mut()
        .find(|kept| kept.id == profile.id)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    named.credential.clone_from(&profile.credential);
    named.sign_in.clone_from(&profile.sign_in);
    let view = view_of(named);
    save_provider_config(root, &config)?;
    Ok(view)
}

pub(super) async fn remove_key(
    (keys, root): (ProviderKeys<'_>, &Path),
    provider: &str,
) -> Result<ProviderView, String> {
    let mut profile = provider_config(root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    keys.remove(&mut profile).await?;
    save_named_key(root, &profile)
}

pub(super) async fn remove_provider(
    (keys, root): (ProviderKeys<'_>, &Path),
    provider: &str,
) -> Result<ModelProviders, String> {
    let mut removed = provider_config(root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    keys.remove(&mut removed).await?;
    let mut config = provider_config(root);
    config.profiles.retain(|profile| profile.id != provider);
    if config.default_profile.as_deref() == Some(provider) {
        config.default_profile = None;
    }
    config
        .interaction_models
        .retain(|_, chosen| chosen.profile != provider);
    save_provider_config(root, &config)?;
    Ok(providers(root))
}

pub(super) fn set_assignment(
    root: &Path,
    choosing: SetAssignment,
) -> Result<ModelProviders, String> {
    let mut config = provider_config(root);
    let kind = agent_provider_config::Interaction::from_id(&choosing.kind)
        .ok_or_else(|| "That is not a kind of work".to_owned())?;
    let chosen = match (choosing.provider, choosing.model) {
        (Some(profile), Some(model)) => Some(agent_provider_config::AgentModel { profile, model }),
        (None, None) => None,
        _ => return Err("A model choice names both a provider and its model".into()),
    };
    config
        .set_interaction_model(kind, chosen)
        .map_err(str::to_owned)?;
    save_provider_config(root, &config)?;
    Ok(providers(root))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    #[test]
    fn an_account_it_cannot_read_is_refused_without_its_values() {
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: person_accounts::target(person_accounts::ADD_MAIL),
            argument: InvokeArgument::Json(
                serde_json::json!({
                    "address": "ada@example.test",
                    "incomingHost": "imap.example.test",
                    "outgoingHost": "smtp.example.test",
                    "password": "a-secret-value",
                    // A field it does not know is read past, so what it cannot
                    // read is a value of the wrong type, which serde quotes.
                    "incomingPort": "a-secret-value",
                })
                .as_object()
                .cloned()
                .unwrap(),
            ),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let Err(ActionCallError::InvalidRequest { reason }) =
            argument::<person_accounts::AddMailAccount>(&request, "an account to add")
        else {
            panic!("an account of the wrong shape is refused");
        };
        assert!(!reason.contains("a-secret-value"), "{reason}");
        assert!(reason.contains("invalid type"), "{reason}");
    }

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary root")
    }

    /// Every file under `dir`, for the proof that none of them carries a
    /// secret in the clear.
    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut waiting = vec![dir.to_path_buf()];
        while let Some(dir) = waiting.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // Only regular files carry a document's bytes: key storage
                // keeps other kinds of entry beside them.
                match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => waiting.push(path),
                    Ok(metadata) if metadata.is_file() => files.push(path),
                    _ => {}
                }
            }
        }
        files
    }

    /// A workspace's vault held for one test: what the vault document keeps on a host.
    pub(super) fn held() -> &'static super::super::vault::TestStores {
        Box::leak(Box::default())
    }

    /// Every entry `vault` keeps: its password and the places it belongs to.
    pub(super) fn entries_of(
        vault: &std::sync::Mutex<accounts::vault_items::VaultItems>,
    ) -> Vec<(String, Vec<String>)> {
        vault
            .lock()
            .unwrap()
            .kept()
            .into_iter()
            .map(|(entry, _)| {
                (
                    entry.password.clone(),
                    entry.origins.iter().cloned().collect(),
                )
            })
            .collect()
    }

    /// Whether any file under `root` holds `secret` as it is.
    fn files_holding(root: &Path, secret: &str) -> Vec<PathBuf> {
        files_under(root)
            .into_iter()
            .filter(|file| {
                std::fs::read(file).is_ok_and(|bytes| {
                    bytes
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes())
                })
            })
            .collect()
    }

    /// A provider key is an entry of the workspace's vault, bound to its endpoint's origin,
    /// never in the settings document or any other file the host keeps, and the read back says
    /// only that one is kept.
    #[tokio::test]
    async fn a_provider_key_is_kept_in_the_vault_and_never_read_back() {
        let root = temp_root();
        let vault = held();
        let keys = ProviderKeys::of(VaultAt::Held(vault), "");
        let added = add_provider(
            root.path(),
            &model_providers::AddProvider {
                name: "OpenAI".into(),
                protocol: model_providers::Protocol::OpenAiCompatible,
                endpoint: "https://api.openai.com/v1".into(),
                headers: Vec::new(),
            },
            Destinations::Public,
        )
        .expect("a provider is added");
        assert!(!added.configured);

        let kept = set_key(
            (keys, root.path()),
            SetKey {
                provider: added.id.clone(),
                key: "sk-live-secret".into(),
            },
            None,
        )
        .await
        .expect("the key is kept");
        assert!(kept.configured);
        assert_eq!(
            entries_of(vault),
            [(
                "sk-live-secret".to_owned(),
                vec!["https://api.openai.com".to_owned()]
            )]
        );
        assert_eq!(
            files_holding(root.path(), "sk-live-secret"),
            Vec::<PathBuf>::new(),
            "the settings name the key; no file the host keeps holds it"
        );
        let listed = providers(root.path());
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("sk-live-secret"), "{json}");
        assert!(json.contains("\"configured\":true"), "{json}");

        remove_key((keys, root.path()), &added.id)
            .await
            .expect("the key is removed");
        assert!(!providers(root.path()).providers[0].configured);
        assert!(entries_of(vault).is_empty(), "the key's entry went with it");
    }

    /// A provider removed takes its key with it, and its model choices stop
    /// naming it.
    #[tokio::test]
    async fn removing_a_provider_takes_its_key_and_choices() {
        let root = temp_root();
        let vault = held();
        let keys = ProviderKeys::of(VaultAt::Held(vault), "");
        let added = add_provider(
            root.path(),
            &model_providers::AddProvider {
                name: "Anthropic".into(),
                protocol: model_providers::Protocol::Anthropic,
                endpoint: "https://api.anthropic.com".into(),
                headers: Vec::new(),
            },
            Destinations::Public,
        )
        .expect("a provider is added");
        set_key(
            (keys, root.path()),
            SetKey {
                provider: added.id.clone(),
                key: "sk-ant-secret".into(),
            },
            None,
        )
        .await
        .expect("the key is kept");
        assert_eq!(entries_of(vault).len(), 1);
        let remaining = remove_provider((keys, root.path()), &added.id)
            .await
            .expect("the provider is removed");
        assert!(remaining.providers.is_empty());
        assert!(entries_of(vault).is_empty());
    }

    /// A key is kept for its provider's endpoint origin, as a Tilo computer
    /// keeps it: pointed elsewhere, the provider has no key until one is
    /// entered for the new address, and the old one is never sent there.
    #[tokio::test]
    async fn a_key_does_not_follow_an_edited_endpoint() {
        let root = temp_root();
        let keys = ProviderKeys::of(VaultAt::Held(held()), "");
        let added = add_provider(
            root.path(),
            &model_providers::AddProvider {
                name: "Mine".into(),
                protocol: model_providers::Protocol::OpenAiCompatible,
                endpoint: "https://models.example.com/v1".into(),
                headers: Vec::new(),
            },
            Destinations::Public,
        )
        .expect("a provider is added");
        let kept = set_key(
            (keys, root.path()),
            SetKey {
                provider: added.id,
                key: "sk-for-example".into(),
            },
            None,
        )
        .await
        .expect("the key is kept");
        assert!(kept.configured);
        let at_home = provider_config(root.path()).profiles[0].clone();
        assert_eq!(
            keys.get(&at_home)
                .await
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("sk-for-example")
        );

        let mut config = provider_config(root.path());
        config.profiles[0].endpoint = "https://elsewhere.example.net/v1".into();
        save_provider_config(root.path(), &config).unwrap();
        let moved = &provider_config(root.path()).profiles[0];
        assert_eq!(keys.get(moved).await.unwrap(), None);
    }

    /// A provider without a name or a URL is refused before anything is
    /// kept, so a person sees why rather than an empty entry.
    #[test]
    fn an_unusable_provider_is_refused() {
        let root = temp_root();
        for (name, endpoint) in [("  ", "https://x.test"), ("Mine", "api.x.test")] {
            let refused = add_provider(
                root.path(),
                &model_providers::AddProvider {
                    name: name.into(),
                    protocol: model_providers::Protocol::OpenAiCompatible,
                    endpoint: endpoint.into(),
                    headers: Vec::new(),
                },
                Destinations::Public,
            );
            assert!(refused.is_err(), "{name} {endpoint}");
        }
        assert!(providers(root.path()).providers.is_empty());
    }

    /// A mail account's stored settings never hold its password, only the entry of the
    /// workspace's vault that does. A removal takes the account's own entry with it, and leaves
    /// a member's entry another account signed in with.
    #[tokio::test]
    async fn a_mail_account_keeps_its_secret_out_of_the_store() {
        let root = temp_root();
        let vault = held();
        let at = VaultAt::Held(vault);
        // The server check needs a server; keeping and removing are what the
        // store and vault do, so they are proven without one.
        for (address, own) in [("ada@example.test", true), ("bo@example.test", false)] {
            let mut account = accounts::Account::new(address, accounts::email::TYPE_ID);
            account.config = serde_json::json!({
                "email": address,
                "imap_host": "imap.example.test",
                "imap_port": 993,
                "smtp_host": "smtp.example.test",
                "smtp_port": 465,
            });
            let places = kept_sign_ins::places_of(&account).unwrap();
            let password = format!("{}-hunter2", &address[..2]);
            let reference = if own {
                at.keep_sign_in(
                    address,
                    (accounts::vault_items::ItemKind::Login, places),
                    address,
                    zeroize::Zeroizing::new(password),
                    None,
                )
                .await
                .unwrap()
            } else {
                let added = vault
                    .lock()
                    .unwrap()
                    .add(accounts::vault_items::NewItem::new(
                        accounts::vault_items::ItemKind::Login,
                        places,
                        address.to_owned(),
                        password,
                    ))
                    .unwrap();
                tilo_actions::remote::person_vault::ItemRef::from_id(added.id.as_str()).unwrap()
            };
            accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
            vault.keep_account(&account);
        }

        let listed = mail_accounts(&at.accounts().await.unwrap(), root.path());
        assert_eq!(listed.accounts.len(), 2);
        assert!(listed.accounts.iter().all(|account| account.has_secret));
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("hunter2"), "{json}");
        assert_eq!(files_holding(root.path(), "hunter2"), Vec::<PathBuf>::new());

        let removed = remove_mail((at, root.path()), "ada@example.test")
            .await
            .expect("the account is removed");
        assert_eq!(removed.accounts.len(), 1);
        remove_mail((at, root.path()), "bo@example.test")
            .await
            .expect("the other is removed");
        assert_eq!(
            entries_of(vault)
                .into_iter()
                .map(|(password, _)| password)
                .collect::<Vec<_>>(),
            ["bo-hunter2"],
            "the account's own sign-in went with it, and the member's entry stays theirs"
        );
    }
}

#[cfg(test)]
mod accounts_sign_in_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// A tester that accepts every sign-in, and counts what it was asked.
    #[derive(Default)]
    struct Accepting {
        asked: std::sync::Mutex<Vec<String>>,
    }
    impl AccountTester for Accepting {
        fn test(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
        ) -> Result<String, String> {
            self.asked
                .lock()
                .unwrap()
                .push(format!("{} with {}", account.id, secret.password));
            Ok("signed in".into())
        }
    }

    /// An account and its sign-in, as a test keeps them: the account in the
    /// workspace's accounts, naming its entry of the workspace's vault.
    async fn account_with_sign_in(signing: Signing<'_>) {
        let mut account = accounts::Account::new(
            accounts::AccountId::from("ada@example.test"),
            accounts::email::TYPE_ID,
        );
        account.config = serde_json::json!({
            "email": "ada@example.test",
            "username": "ada@example.test",
            "imap_host": "imap.example.test",
            "imap_port": 993,
            "smtp_host": "smtp.example.test",
            "smtp_port": 465
        });
        let reference = signing
            .vault
            .keep_sign_in(
                "ada@example.test",
                (
                    accounts::vault_items::ItemKind::Login,
                    places_of(&account).unwrap(),
                ),
                "ada@example.test",
                zeroize::Zeroizing::new("fixture-password".to_owned()),
                None,
            )
            .await
            .unwrap();
        accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
        signing.vault.keep_account(&account).await.unwrap();
    }

    /// An account another mail program moved in lands in the person's
    /// store with the other names it sends as and the sign-in it carried,
    /// proved before anything keeps it: the sign-in is in the vault and
    /// nowhere in the clear.
    #[test]
    fn a_migrated_account_keeps_its_identities_and_its_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let vault = super::tests::held();
        let tester = Arc::new(Accepting::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let kept = runtime
            .block_on(add_mail_with(
                tester.clone(),
                (VaultAt::Held(vault), root),
                AddMailAccount {
                    address: "ada@example.test".into(),
                    display_name: "Ada".into(),
                    username: None,
                    incoming_host: "imap.example.test".into(),
                    incoming_port: 993,
                    outgoing_host: "smtp.example.test".into(),
                    outgoing_port: 465,
                    password: "thunderbird-secret".into(),
                    identities: vec!["Ada at work <ada@work.example.test>".into()],
                    services: person_accounts::Services::default(),
                },
                None,
                person_accounts::Provider::Other,
                None,
            ))
            .unwrap();
        assert!(kept.has_secret);
        assert_eq!(kept.address, "ada@example.test");
        assert_eq!(
            tester.asked.lock().unwrap().clone(),
            vec!["ada@example.test with thunderbird-secret".to_owned()],
            "the sign-in is proved before the account is kept"
        );

        let account = vault
            .account("ada@example.test")
            .expect("the account is kept");
        let config = accounts::email::AccountConfig::from_value(account.config).unwrap();
        assert_eq!(
            config.identities,
            vec!["Ada at work <ada@work.example.test>".to_owned()]
        );
        assert!(!config.import_pending);
        assert_eq!(
            super::tests::entries_of(vault),
            [(
                "thunderbird-secret".to_owned(),
                vec![
                    "imap://imap.example.test:993".to_owned(),
                    "smtp://smtp.example.test:465".to_owned()
                ]
            )],
            "the sign-in is the account's own entry of the workspace's vault, at its servers"
        );

        for path in walk(root) {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes
                    .windows(19)
                    .any(|window| window == b"thunderbird-secret"),
                "{} carries the sign-in in the clear",
                path.display()
            );
        }
    }

    /// An account moved in without a saved sign-in is kept waiting for it,
    /// and no session is opened with nothing.
    #[test]
    fn an_account_moved_in_without_a_sign_in_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let tester = Arc::new(Accepting::default());
        let vault = super::tests::held();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let kept = runtime
            .block_on(add_mail_with(
                tester.clone(),
                (VaultAt::Held(vault), root),
                AddMailAccount {
                    address: "ada@example.test".into(),
                    display_name: String::new(),
                    username: None,
                    incoming_host: "imap.example.test".into(),
                    incoming_port: 993,
                    outgoing_host: "smtp.example.test".into(),
                    outgoing_port: 465,
                    password: String::new(),
                    identities: Vec::new(),
                    services: person_accounts::Services::default(),
                },
                None,
                person_accounts::Provider::Other,
                None,
            ))
            .unwrap();
        assert!(!kept.has_secret);
        assert!(tester.asked.lock().unwrap().is_empty());
        let account = vault
            .account("ada@example.test")
            .expect("the account is kept");
        let config = accounts::email::AccountConfig::from_value(account.config).unwrap();
        assert!(config.import_pending);
    }

    /// A sign-in belongs to the servers its account names when it is kept. An
    /// account that comes to name another server signs in nowhere with it,
    /// whoever asks, until it names its own servers again.
    #[test]
    fn a_sign_in_is_used_at_its_own_servers_alone() {
        const ADA: &str = "ada@example.test";
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let stores = super::tests::held();
        let signing = Signing {
            vault: VaultAt::Held(stores),
            root,
            by: "",
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(account_with_sign_in(signing));
        let names = |incoming: &str| {
            let mut account = stores.account(ADA).unwrap();
            account.config["imap_host"] = incoming.into();
            stores.keep_account(&account);
        };
        let (_, secret) = runtime.block_on(account_with_secret(signing, ADA)).unwrap();
        assert_eq!(secret.password, "fixture-password");

        names("imap.elsewhere.example");
        let refused = runtime
            .block_on(account_with_secret(signing, ADA))
            .unwrap_err();
        assert!(refused.contains("other servers"), "{refused}");
        let agent = Signing {
            by: "scout",
            ..signing
        };
        assert!(runtime.block_on(account_with_secret(agent, ADA)).is_err());

        names("imap.example.test");
        assert!(runtime.block_on(account_with_secret(signing, ADA)).is_ok());
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut waiting = vec![dir.to_path_buf()];
        while let Some(dir) = waiting.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => waiting.push(path),
                    Ok(metadata) if metadata.is_file() => files.push(path),
                    _ => {}
                }
            }
        }
        files
    }
}
