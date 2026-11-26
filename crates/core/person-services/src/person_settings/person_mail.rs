//! The workspace's mail accounts as their holder keeps them: the servers of an
//! address worked out from the address alone, an account added once its
//! server took its sign-in, and which of its services its person uses.
//!
//! Nothing here carries a secret back: a reading says an account's address
//! and its servers, never its password.

use std::path::Path;

use serde_json::{Map, Value};
use tilo_account_probe::mail_servers::MailServers;
use tilo_actions::remote::ActionCallError;
use tilo_workspace_service::workspace_accounts::{
    AccountServices, AccountsAction, AddMailAccount, FoundServers, MailAccount, MailAccounts,
    MailSignIn, MailSignIns, Provider, ServerAt, SignedInAccount,
};

use super::{add_mail_with, answer, remove_mail};
use crate::VaultAt;
use crate::person_settings::cloud_control::Request;
use crate::person_settings::kept_sign_ins::sign_in_of;
use crate::person_settings::kept_sign_ins::{
    NO_SIGN_IN, SIGN_IN, Signing, has_sign_in, places_of, sign_in_of as kept_sign_in_of,
};

impl crate::PersonServices {
    /// Where the mail of `address` lives, worked out from the address alone.
    /// Only a server every address of which is public is asked anything,
    /// except where this server's operator lets it call its own network.
    async fn servers_of(&self, address: &str) -> Result<MailServers, String> {
        let anywhere = self.inner.models.private_addresses();
        let tester = self.account_tester();
        let of = address.trim().to_owned();
        tokio::task::spawn_blocking(move || {
            tester.find_mail_servers(&of, &|name| anywhere || answers_in_public(name))
        })
        .await
        .map_err(|_| "the address's servers were not looked up to the end".to_owned())?
    }

    /// Where the mail of `address` lives, as a person reads it
    /// (`FIND_SERVERS`).
    pub(super) async fn find_servers(&self, address: &str) -> Result<FoundServers, String> {
        match self.servers_of(address).await {
            Ok(found) => Ok(found_servers(found)),
            Err(why) => Err(no_servers(&why, address)),
        }
    }

    /// Add the mail account `adding`, finding its servers from the address
    /// where its person typed none (`ADD_MAIL`). A provider that keeps
    /// calendars with the same sign-in keeps them beside the account where its
    /// person asked for calendars: one connect feeds Mail and Calendar alike.
    pub(super) async fn add_mail(
        &self,
        signing: Signing<'_>,
        mut adding: AddMailAccount,
    ) -> Result<MailAccount, String> {
        // The servers are written as their name and, where it is not the usual one, its port.
        if let Some((host, port)) =
            accounts::email::parse_server(&adding.incoming_host, adding.incoming_port)
                .map_err(|why| format!("The incoming server: {why}"))?
        {
            adding.incoming_host = host;
            adding.incoming_port = port;
        }
        if let Some((host, port)) =
            accounts::email::parse_server(&adding.outgoing_host, adding.outgoing_port)
                .map_err(|why| format!("The outgoing server: {why}"))?
        {
            adding.outgoing_host = host;
            adding.outgoing_port = port;
        }
        let finding =
            adding.incoming_host.trim().is_empty() || adding.outgoing_host.trim().is_empty();
        let provider = if finding {
            let mut found = self
                .servers_of(&adding.address)
                .await
                .map_err(|why| no_servers(&why, &adding.address))?;
            let provider = provider_of(&found);
            if adding.incoming_host.trim().is_empty() {
                found.incoming.name.clone_into(&mut adding.incoming_host);
                adding.incoming_port = found.incoming.port;
            }
            if adding.outgoing_host.trim().is_empty() {
                found.outgoing.name.clone_into(&mut adding.outgoing_host);
                adding.outgoing_port = found.outgoing.port;
            }
            if adding.username.is_none() {
                adding.username = found.username.take();
            }
            provider
        } else {
            provider_of_servers(&adding.incoming_host, &adding.outgoing_host)
        };
        // What the person chose of the services this provider has: an address
        // whose provider keeps calendars does not keep them because the
        // servers of its mail were typed.
        adding.services = adding.services.within(provider);
        // An iCloud account's calendars are kept with its app password, where
        // its person asked for them: the calendar account is made first (its
        // server is asked, its sign-in kept), so one change below keeps both
        // records. A calendar server that does not take them leaves the mail
        // account as it is, feeding mail alone.
        #[cfg(feature = "agents")]
        let mut linked: Option<(accounts::Account, String)> = None;
        #[cfg(feature = "agents")]
        if provider == Provider::Icloud && adding.services.calendar && !adding.password.is_empty() {
            let server = self.icloud_calendar_server();
            match keep_calendar_account(
                signing.vault,
                &server,
                &adding.address,
                &adding.password,
                &adding.display_name,
                Some(&adding.address),
            )
            .await
            {
                Ok(calendar) => linked = Some((calendar, server)),
                Err(why) => {
                    tracing::warn!(
                        address = %adding.address,
                        %why,
                        "mail: an iCloud account's calendars were not kept, so it feeds mail alone"
                    );
                    adding.services.calendar = false;
                }
            }
        }
        #[cfg(not(feature = "agents"))]
        let linked: Option<(accounts::Account, String)> = None;
        let kept = Box::pin(add_mail_with(
            self.account_tester(),
            (signing.vault, signing.root),
            adding,
            None,
            provider,
            linked.clone(),
        ))
        .await;
        // A mail account refused after its calendars were prepared leaves no
        // sign-in of theirs in the vault.
        #[cfg(feature = "agents")]
        if kept.is_err()
            && let Some((calendar, _)) = &linked
            && let Some(reference) = accounts::email::password_item(&calendar.config)
                .and_then(tilo_workspace_service::workspace_vault::ItemRef::from_id)
        {
            let _ = signing
                .vault
                .forget_sign_in(calendar.id.as_str(), &reference)
                .await;
        }
        kept
    }

    /// Sign `account` in again with the entry `workspace`'s vault keeps for it
    /// (`RECONNECT`): the same sign-in adding it made, and what the account's
    /// row then reads is how it went. A sign-in the servers no longer take is
    /// reported as needing one, and any other failure as it was.
    async fn reconnect(
        &self,
        signing: Signing<'_>,
        workspace: &str,
        acting: &tilo_action_policy::Acting,
        account: &str,
    ) -> Result<(), String> {
        let call = signing.vault.call();
        let (kept, secret) = crate::person_settings::account_with_secret(signing, account).await?;
        let tester = self.account_tester();
        let checking = kept.clone();
        let outcome = tokio::task::spawn_blocking(move || tester.test(&checking, &secret))
            .await
            .map_err(|_| "the account's sign-in did not finish".to_owned())?;
        let (health, signed_in, words) = match &outcome {
            Ok(_) => (accounts::SyncHealth::Healthy, true, String::new()),
            Err(reason) if reason.to_ascii_lowercase().contains("rejected") => (
                accounts::SyncHealth::NeedsSignIn,
                false,
                reason
                    .chars()
                    .take(accounts::person::SYNC_MESSAGE_LIMIT)
                    .collect(),
            ),
            Err(reason) => (
                accounts::SyncHealth::Error,
                false,
                reason
                    .chars()
                    .take(accounts::person::SYNC_MESSAGE_LIMIT)
                    .collect(),
            ),
        };
        self.report_account_sync(
            (workspace, acting, call),
            account,
            health,
            signed_in,
            &words,
        )
        .await?;
        outcome.map(|_| ()).map_err(|reason| {
            let name = kept.display_name.trim();
            if name.is_empty() {
                format!("The account could not be signed in again: {reason}")
            } else {
                format!("{name} could not be signed in again: {reason}")
            }
        })
    }

    /// Check that `account`'s server takes the sign-in the workspace's vault keeps
    /// (`CHECK`), answering the probe's own words and writing nothing.
    async fn check_saved_sign_in(
        &self,
        signing: Signing<'_>,
        account: &str,
    ) -> Result<String, String> {
        let (kept, secret) = crate::person_settings::account_with_secret(signing, account).await?;
        let tester = self.account_tester();
        tokio::task::spawn_blocking(move || tester.test(&kept, &secret))
            .await
            .map_err(|_| "the account's check did not finish".to_owned())?
    }

    /// Replace `account`'s saved sign-in with `password` once its server takes it
    /// (`REPLACE_PASSWORD`): the account keeps the same vault reference.
    async fn replace_password(
        &self,
        signing: Signing<'_>,
        (account, password): (&str, String),
    ) -> Result<(), String> {
        let (kept, _) = super::account_with_secret(signing, account).await?;
        let places = super::kept_sign_ins::places_of(&kept)?;
        let before = super::kept_sign_ins::sign_in_of(&kept.config);
        let checking = kept.clone();
        let secret = accounts::Secret::password(password.clone());
        let tester = self.account_tester();
        tokio::task::spawn_blocking(move || tester.test(&checking, &secret))
            .await
            .map_err(|_| "the account's check did not finish".to_owned())??;
        let username = kept
            .config
            .get("username")
            .and_then(Value::as_str)
            .map_or_else(|| kept.id.to_string(), str::to_owned);
        signing
            .vault
            .keep_sign_in(
                kept.id.as_str(),
                (accounts::vault_items::ItemKind::Login, places),
                &username,
                zeroize::Zeroizing::new(password),
                before.as_ref(),
            )
            .await?;
        // A sign-in its person gives ends the account's wait for one.
        let mut account = kept;
        if let Some(settings) = account.config.as_object_mut() {
            settings.insert("import_pending".into(), Value::Bool(false));
            signing.vault.keep_account(&account).await?;
        }
        Ok(())
    }

    /// Ask `account`'s calendar server for the calendars it holds (`DISCOVER_CALENDARS`) and keep
    /// them on the account: the ones it held before keep the choice its person made for them, and
    /// a calendar the server holds now shows.
    async fn discover_calendars(&self, signing: Signing<'_>, account: &str) -> Result<(), String> {
        let held = signing.vault.accounts().await?;
        let kept = held
            .account(account)
            .filter(|kept| kept.type_id.as_str() == accounts::calendar::TYPE_ID)
            .ok_or_else(|| "That account is no calendar account".to_owned())?;
        let reference = kept_sign_in_of(&kept.config).ok_or_else(|| NO_SIGN_IN.to_owned())?;
        let places = places_of(&kept)?;
        let released = signing
            .vault
            .use_sign_in(
                &reference,
                (accounts::vault_items::ItemKind::Login, &places),
                signing.by,
                SIGN_IN,
            )
            .await?;
        let tester = self.account_tester();
        let asking = kept.clone();
        let secret = accounts::Secret::password(released.secret.to_string());
        let found =
            tokio::task::spawn_blocking(move || tester.discover_calendars(&asking, &secret))
                .await
                .map_err(|_| "the calendar server check did not finish".to_owned())??;
        let mut kept = kept;
        let read_only = is_read_only(&kept);
        let calendars = calendars_found(found, &kept, read_only)?;
        if let Some(config) = kept.config.as_object_mut() {
            config.insert("calendars".into(), calendars);
        }
        signing.vault.keep_account(&kept).await
    }

    /// Connect the calendar account `username` signs in to at the `CalDAV` server `server` with
    /// `password`, shown as `name` (`ADD_CALENDAR`), once that server took its sign-in: the calendars it lists are kept on the account, each on
    /// show, and the sign-in in the workspace's vault. The server is asked only where this
    /// server's mail may go. Added again, it is the same account, signed in with what was typed
    /// now: an account connected for reading only stays so, whoever adds it, until its person
    /// removes it.
    async fn add_calendar(
        &self,
        signing: Signing<'_>,
        (server, username, password): (&str, &str, &str),
        (name, read_only): (&str, bool),
    ) -> Result<MailAccount, String> {
        let username = username.trim().to_owned();
        if username.is_empty() {
            return Err("A calendar account has a user name".to_owned());
        }
        if password.is_empty() {
            return Err("A calendar account has a password".to_owned());
        }
        let server = calendar_server(server)?;
        let id = caldav_id(&username, &server);
        let held = signing.vault.accounts().await?;
        let before = held
            .account(&id)
            .filter(|kept| kept.type_id.as_str() == accounts::calendar::TYPE_ID);
        let read_only = read_only || before.as_ref().is_some_and(is_read_only);
        let mut asking = accounts::Account::new(
            accounts::AccountId::from(id.clone()),
            accounts::calendar::TYPE_ID,
        );
        asking.config = serde_json::json!({ "server_url": server.as_str(), "username": username });
        if let Some(before) = before {
            let calendars = before.config.get("calendars").cloned().unwrap_or_default();
            if let Some(config) = asking.config.as_object_mut() {
                config.insert("calendars".into(), calendars);
            }
        }
        let tester = self.account_tester();
        let secret = accounts::Secret::password(password.to_owned());
        let looked = asking.clone();
        let found =
            tokio::task::spawn_blocking(move || tester.discover_calendars(&looked, &secret))
                .await
                .map_err(|_| "the calendar server check did not finish".to_owned())??;
        let calendars = calendars_found(found, &asking, read_only)?;
        let mut stored = keep_calendar_account(
            signing.vault,
            server.as_str(),
            &username,
            password,
            name,
            None,
        )
        .await?;
        if let Some(config) = stored.config.as_object_mut() {
            config.insert("calendars".into(), calendars);
            if read_only {
                config.insert("read_only".into(), Value::Bool(true));
            }
        }
        signing.vault.keep_account(&stored).await?;
        Ok(calendar_row(&stored))
    }

    /// Keep on show the calendars `choosing` names (`SET_CALENDARS`): the ones it leaves out stay
    /// on the account with their events out of the calendar.
    async fn set_calendars(
        &self,
        signing: Signing<'_>,
        (account, calendars): (&str, &[String]),
    ) -> Result<(), String> {
        let held = signing.vault.accounts().await?;
        let mut kept = held
            .account(account)
            .filter(|kept| kept.type_id.as_str() == accounts::calendar::TYPE_ID)
            .ok_or_else(|| "That account is no calendar account".to_owned())?;
        let on_show: std::collections::BTreeSet<&str> =
            calendars.iter().map(String::as_str).collect();
        let known: std::collections::BTreeSet<String> = kept
            .config
            .get("calendars")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|calendar| calendar.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        if let Some(unknown) = on_show.iter().find(|id| !known.contains(**id)) {
            return Err(format!("{unknown} is not this account's calendar"));
        }
        if let Some(calendars) = kept
            .config
            .get_mut("calendars")
            .and_then(Value::as_array_mut)
        {
            for calendar in calendars {
                let Some(object) = calendar.as_object_mut() else {
                    continue;
                };
                let Some(id) = object.get("id").and_then(Value::as_str).map(str::to_owned) else {
                    continue;
                };
                object.insert("visible".into(), Value::Bool(on_show.contains(id.as_str())));
            }
        }
        signing.vault.keep_account(&kept).await
    }

    /// Say which of an account's services its person uses (`SET_SERVICES`).
    /// Its mail account stays whatever is said, and Mail hides it where its
    /// mail is off.
    #[cfg_attr(
        not(feature = "agents"),
        allow(
            clippy::unused_async,
            reason = "it keeps the shape of the build that does keep calendars"
        )
    )]
    /// The logins the workspace's vault keeps that belong to mail servers, each by its
    /// reference with its name, sign-in name and servers, never its secret
    /// (`SIGN_INS`): what a person picks from to add an account without typing its
    /// password again.
    pub(super) async fn vault_sign_ins(&self, workspace: &str) -> Result<MailSignIns, String> {
        let Some(opened) = self.read_made_vault(workspace).await? else {
            return Ok(MailSignIns::default());
        };
        let held = self.workspace_accounts(workspace).await?;
        let items = opened.items();
        let sign_ins = opened
            .entries()
            .iter()
            .filter(|(entry, _)| entry.kind == accounts::SecretItemKind::Login)
            .filter_map(|(entry, _)| {
                let id = tilo_workspace_service::workspace_vault::ItemRef::from_id(&entry.id.0)?;
                let account = accounts::vault_items::ItemId::parse(&entry.id.0)
                    .and_then(|id| items.kept_for(&id).map(str::to_owned))
                    .and_then(|kept| held.account(&kept))
                    .map(|account| signed_in_account(&account));
                Some(MailSignIn {
                    id: id.to_string(),
                    title: entry.title.clone(),
                    username: entry.username.clone(),
                    places: entry.origins.iter().cloned().collect(),
                    account,
                })
            })
            .collect();
        Ok(MailSignIns { sign_ins })
    }

    /// Keep the cloud-storage account `account` in the files mode `mode` its person chose
    /// (`SET_FILES_MODE`): files kept in sync, or copied into the workspace once.
    pub(super) async fn set_files_mode(
        &self,
        signing: Signing<'_>,
        (account, mode): (&str, &str),
    ) -> Result<MailAccounts, String> {
        let held = signing.vault.accounts().await?;
        let account = held
            .account(account)
            .ok_or_else(|| "That account is not kept here".to_owned())?;
        if !is_file_account(&account) {
            return Err("Only a cloud-storage account's files have a mode".to_owned());
        }
        let mode = match mode.trim() {
            "sync" => accounts::cloud_types::FilesMode::Sync,
            "import" => accounts::cloud_types::FilesMode::Import,
            other => {
                return Err(format!(
                    "`{other}` is no files mode: an account's files stay in sync or are \
                     imported once"
                ));
            }
        };
        change_config(signing.vault, account.id.as_str(), |config| {
            let mut settings = Value::Object(std::mem::take(config));
            accounts::cloud_types::set_files_mode(&mut settings, mode);
            if let Value::Object(back) = settings {
                *config = back;
            }
            Ok(())
        })
        .await?;
        Ok(mail_accounts(
            &signing.vault.accounts().await?,
            signing.root,
        ))
    }

    /// Tell this device's files service `request`, of one cloud-storage account (`SYNC_NOW`,
    /// `PAUSE_SYNC`, `RESUME_SYNC`), answering the accounts as they are.
    pub(super) async fn control_sync(
        &self,
        signing: Signing<'_>,
        request: Request,
    ) -> Result<MailAccounts, String> {
        let (Request::SyncNow { account }
        | Request::Pause { account }
        | Request::Resume { account }) = &request;
        let held = signing.vault.accounts().await?;
        let account = held
            .account(account)
            .ok_or_else(|| "That account is not kept here".to_owned())?;
        if !is_file_account(&account) {
            return Err("Only a cloud-storage account's files sync on this device".to_owned());
        }
        let cloud = self.cloud_control();
        tokio::task::spawn_blocking(move || cloud.control(request))
            .await
            .map_err(|_| "the files service was not told to the end".to_owned())??;
        Ok(mail_accounts(
            &signing.vault.accounts().await?,
            signing.root,
        ))
    }

    pub(super) async fn set_services(
        &self,
        signing: Signing<'_>,
        (account, chosen): (&str, AccountServices),
    ) -> Result<MailAccounts, String> {
        let held = signing.vault.accounts().await?;
        let account = held
            .account(account)
            .ok_or_else(|| "That account is not kept here".to_owned())?;
        let provider = provider_of_account(&account);
        // A file account's services are its mount's own choice, which its
        // provider list does not limit: `Nextcloud` offers files and
        // calendars, a drive its files.
        let chosen = if is_file_account(&account) {
            chosen
        } else {
            chosen.within(provider)
        };
        #[cfg(feature = "agents")]
        let mut services = chosen;
        // A build without the calendar module keeps no calendar account.
        #[cfg(not(feature = "agents"))]
        let services = AccountServices {
            calendar: false,
            ..chosen
        };
        // An iCloud account's calendars go with its sign-in: kept where they
        // are turned on, removed with what was kept of them where they are
        // turned off, each in one change with the mail account's own settings.
        #[cfg(feature = "agents")]
        if provider == Provider::Icloud {
            let linked = linked_calendar(&held, &account);
            if services.calendar && linked.is_none() {
                match self
                    .keep_service_calendar(signing, account.id.as_str())
                    .await
                {
                    Ok(calendar) => {
                        let server = self.icloud_calendar_server();
                        let mail = with_link(&account, services, Some((&calendar, &server)));
                        signing
                            .vault
                            .keep_and_remove_accounts(&[mail, calendar], &[])
                            .await?;
                        return Ok(mail_accounts(
                            &signing.vault.accounts().await?,
                            signing.root,
                        ));
                    }
                    Err(why) => {
                        tracing::warn!(
                            account = %account.id.as_str(),
                            %why,
                            "mail: an iCloud account's calendars were not kept, so it feeds mail alone"
                        );
                        services.calendar = false;
                    }
                }
            } else if !services.calendar
                && let Some(calendar) = &linked
            {
                let mail = with_link(&account, services, None);
                signing
                    .vault
                    .keep_and_remove_accounts(&[mail], &[calendar.as_str()])
                    .await?;
                if let Some(sign_in) = held
                    .account(calendar)
                    .and_then(|kept| sign_in_of(&kept.config))
                {
                    signing.vault.forget_sign_in(calendar, &sign_in).await?;
                }
                return Ok(mail_accounts(
                    &signing.vault.accounts().await?,
                    signing.root,
                ));
            }
        }
        set_services_in_config(signing.vault, &account, services).await?;
        Ok(mail_accounts(
            &signing.vault.accounts().await?,
            signing.root,
        ))
    }

    /// The calendar account kept as a kept iCloud account's service, with the
    /// sign-in the workspace's vault holds for it.
    #[cfg(feature = "agents")]
    async fn keep_service_calendar(
        &self,
        signing: Signing<'_>,
        account: &str,
    ) -> Result<accounts::Account, String> {
        let (kept, secret) = crate::person_settings::account_with_secret(signing, account).await?;
        let address = field(&kept, "email");
        let server = self.icloud_calendar_server();
        let username = if address.is_empty() {
            kept.id.to_string()
        } else {
            address
        };
        keep_calendar_account(
            signing.vault,
            &server,
            &username,
            &secret.password,
            &kept.display_name,
            Some(kept.id.as_str()),
        )
        .await
    }

    /// Where an iCloud account's calendars are kept: iCloud's own server, or
    /// what a test's calendar server stands in with.
    #[cfg(feature = "agents")]
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        allow(
            clippy::unused_self,
            reason = "a test's calendar server is read from the holder; a real build always answers iCloud's own"
        )
    )]
    fn icloud_calendar_server(&self) -> String {
        #[cfg(any(test, feature = "test-support"))]
        {
            let at = self
                .inner
                .icloud_calendars_at
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(at) = at {
                return at;
            }
        }
        ICLOUD_CALENDARS.to_owned()
    }

    /// Answer `asked`, which finds an address's servers, adds an account, signs one in again or
    /// says what it feeds: what a person does with the accounts their vault keeps.
    pub(super) async fn person_mail_action(
        &self,
        signing: Signing<'_>,
        workspace: &str,
        acting: &tilo_action_policy::Acting,
        asked: AccountsAction,
    ) -> Result<Value, ActionCallError> {
        match asked {
            AccountsAction::FindServers { address } => answer(&self.find_servers(&address).await),
            asked @ AccountsAction::AddMail { .. } => {
                let adding = AddMailAccount::asked(asked).ok_or(ActionCallError::UnknownAction)?;
                self.answered_account_change(&self.add_mail(signing, adding).await)
            }
            AccountsAction::AddCalendar {
                server,
                username,
                password,
                name,
                read_only,
            } => {
                let signing_in = (server.as_str(), username.as_str(), password.as_str());
                let shown = (
                    name.as_deref().unwrap_or_default(),
                    read_only.unwrap_or(false),
                );
                self.answered_account_change(
                    &Box::pin(self.add_calendar(signing, signing_in, shown)).await,
                )
            }
            AccountsAction::Reconnect { account } => {
                let account = named(account)?;
                self.answered_account_change(
                    &Box::pin(self.reconnect(signing, workspace, acting, &account)).await,
                )
            }
            AccountsAction::Check { account } => {
                let account = named(account)?;
                answer(&Box::pin(self.check_saved_sign_in(signing, &account)).await)
            }
            AccountsAction::ReplacePassword { account, password } => self.answered_account_change(
                &Box::pin(self.replace_password(signing, (&account, password))).await,
            ),
            AccountsAction::DiscoverCalendars { account } => {
                let account = named(account)?;
                self.answered_account_change(
                    &Box::pin(self.discover_calendars(signing, &account)).await,
                )
            }
            AccountsAction::SetCalendars { account, calendars } => self.answered_account_change(
                &Box::pin(self.set_calendars(signing, (&account, &calendars))).await,
            ),
            AccountsAction::SetServices {
                account,
                mail,
                calendar,
                files,
            } => {
                let services = AccountServices {
                    mail: mail.unwrap_or(false),
                    calendar: calendar.unwrap_or(false),
                    files: files.unwrap_or(false),
                };
                self.answered_account_change(
                    &Box::pin(self.set_services(signing, (&account, services))).await,
                )
            }
            AccountsAction::SignIns => answer(&self.vault_sign_ins(workspace).await),
            AccountsAction::SetFilesMode { account, mode } => self.answered_account_change(
                &Box::pin(self.set_files_mode(signing, (&account, &mode))).await,
            ),
            AccountsAction::SyncNow { account } => self.answered_account_change(
                &Box::pin(self.control_sync(signing, Request::SyncNow { account })).await,
            ),
            AccountsAction::PauseSync { account } => self.answered_account_change(
                &Box::pin(self.control_sync(signing, Request::Pause { account })).await,
            ),
            AccountsAction::ResumeSync { account } => self.answered_account_change(
                &Box::pin(self.control_sync(signing, Request::Resume { account })).await,
            ),
            AccountsAction::Remove { account } => {
                let account = named(account)?;
                self.answered_account_change(
                    &remove_mail((signing.vault, signing.root), &account).await,
                )
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Answer `outcome`, a change to the person's accounts, and tell what reads them that they
    /// moved when it was kept.
    fn answered_account_change<T: serde::Serialize>(
        &self,
        outcome: &Result<T, String>,
    ) -> Result<Value, ActionCallError> {
        if outcome.is_ok() {
            self.person_settings_changed();
        }
        answer(outcome)
    }
}

/// The account `account`, as a sign-in of it shows the app that signs in: where it signs in,
/// never what it signs in with.
fn signed_in_account(account: &accounts::Account) -> SignedInAccount {
    let address = field(account, "email");
    let username = field(account, "username");
    let address = if address.is_empty() {
        if username.is_empty() {
            account.id.to_string()
        } else {
            username
        }
    } else {
        address
    };
    SignedInAccount {
        id: account.id.to_string(),
        kind: account.type_id.to_string(),
        name: account.display_name.clone(),
        address,
        server: server_of(account),
        read_only: account
            .config
            .get("read_only")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        enabled: account.enabled,
        settings: sign_in_settings(&account.config),
    }
}

/// The account settings an app that signs in reads: where its servers are, the name it signs
/// in as, and the vault entry that keeps its secret. Every other setting stays in the
/// workspace, so none a type adds later can leave by this listing.
fn sign_in_settings(config: &Value) -> Value {
    const KEPT: [&str; 13] = [
        "display_name",
        "email",
        "username",
        "imap_host",
        "imap_port",
        "imap_tls",
        "smtp_host",
        "smtp_port",
        "smtp_tls",
        "auth",
        "import_pending",
        "identities",
        accounts::email::PASSWORD_ITEM_KEY,
    ];
    let mut kept = Map::new();
    for key in KEPT {
        if let Some(value) = config.get(key) {
            kept.insert(key.to_owned(), value.clone());
        }
    }
    Value::Object(kept)
}

/// The mail accounts the person keeps, without their secrets: every account
/// that feeds mail, and every other account a provider keeps beside them
/// (a Nextcloud server's, whose files and calendars it is signed in to),
/// which feeds no mail.
pub(super) fn mail_accounts(held: &accounts::person::AccountList, root: &Path) -> MailAccounts {
    let email = accounts::AccountTypeId::from(accounts::email::TYPE_ID);
    let mail = held.of_type(accounts::email::TYPE_ID);
    let listed = |account: &accounts::Account| {
        let provider = provider_of_account(account);
        let address = field(account, "email");
        MailAccount {
            id: account.id.to_string(),
            address: if address.is_empty() {
                let username = field(account, "username");
                if username.is_empty() {
                    account.id.to_string()
                } else {
                    username
                }
            } else {
                address
            },
            display_name: account.display_name.clone(),
            incoming_host: field(account, "imap_host"),
            incoming_port: port(account, "imap_port", 993),
            outgoing_host: field(account, "smtp_host"),
            outgoing_port: port(account, "smtp_port", 465),
            has_secret: has_sign_in(account),
            // Only a mail account sends as other addresses; another
            // provider's settings are not mail settings.
            identities: if account.type_id == email {
                accounts::email::AccountConfig::from_value(account.config.clone()).map_or_else(
                    |error| {
                        tracing::warn!(
                            account = %account.id,
                            %error,
                            "mail: an account's settings do not read, so it is listed with \
                             no other addresses it sends as"
                        );
                        Vec::new()
                    },
                    |config| config.identities,
                )
            } else {
                Vec::new()
            },
            provider,
            services: services_of(account, provider),
            server: server_of(account),
        }
    };
    MailAccounts {
        active: mail
            .first()
            .map(|account| account.id.to_string())
            .unwrap_or_default(),
        accounts: mail
            .iter()
            .chain(
                // A provider's account kept under another type (a Nextcloud
                // server's) feeds the apps it was signed in for, and no mail.
                held.all().iter().filter(|account| {
                    account.type_id != email && account.config.get("provider").is_some()
                }),
            )
            .map(listed)
            .collect(),
        signing_in: crate::person_settings::sign_in::signing_in(root),
        // Which providers this server begins a sign-in with is the sign-in
        // module's to say.
        signs_in_with: crate::person_settings::sign_in::signs_in_with(),
    }
}

/// The provider of `account`: the one its settings name, else the one its
/// servers are known by.
pub(super) fn provider_of_account(account: &accounts::Account) -> Provider {
    account
        .config
        .get("provider")
        .and_then(Value::as_str)
        .and_then(Provider::from_key)
        .unwrap_or_else(|| {
            provider_of_servers(&field(account, "imap_host"), &field(account, "smtp_host"))
        })
}

/// The provider that serves `found`'s servers: Google's or Microsoft's where
/// their own page signs a person in there, iCloud where the mail server is
/// its own, and any other provider otherwise.
fn provider_of(found: &MailServers) -> Provider {
    match found.own_sign_in {
        Some("Google") => Provider::Google,
        Some("Microsoft") => Provider::Microsoft,
        None if is_icloud(&found.incoming.name) => Provider::Icloud,
        Some(_) | None => Provider::Other,
    }
}

/// Whether `name` is one of iCloud's mail servers.
fn is_icloud(name: &str) -> bool {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    name == "imap.mail.me.com" || name.ends_with(".mail.me.com")
}

/// The provider of an account whose mail servers are `incoming` and
/// `outgoing`.
fn provider_of_servers(incoming: &str, outgoing: &str) -> Provider {
    let under = |domains: &[&str]| {
        [incoming, outgoing].iter().any(|name| {
            let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
            domains
                .iter()
                .any(|domain| name == *domain || name.ends_with(&format!(".{domain}")))
        })
    };
    if under(&["gmail.com", "googlemail.com", "google.com"]) {
        Provider::Google
    } else if under(&["outlook.com", "office365.com", "hotmail.com", "live.com"]) {
        Provider::Microsoft
    } else if is_icloud(incoming) {
        Provider::Icloud
    } else {
        Provider::Other
    }
}

/// Where an iCloud account's calendars are kept.
#[cfg(feature = "agents")]
const ICLOUD_CALENDARS: &str = "https://caldav.icloud.com";

/// `account` with its services as `services` says and, where `calendar` names the calendar
/// account kept as one, the link to it: what one change keeps beside the calendar account.
#[cfg(feature = "agents")]
fn with_link(
    account: &accounts::Account,
    services: AccountServices,
    calendar: Option<(&accounts::Account, &str)>,
) -> accounts::Account {
    let mut account = account.clone();
    if let Some(settings) = account.config.as_object_mut() {
        if let Ok(services) = serde_json::to_value(services) {
            settings.insert("services".into(), services);
        }
        settings.remove("calendar_account");
        settings.remove("server");
        if let Some((calendar, server)) = calendar {
            settings.insert("calendar_account".into(), calendar.id.as_str().into());
            settings.insert("server".into(), server.into());
        }
    }
    account
}

/// The calendar account kept as `account`'s service: the one its settings
/// name, else the one kept for it.
#[cfg(feature = "agents")]
fn linked_calendar(
    held: &accounts::person::AccountList,
    account: &accounts::Account,
) -> Option<String> {
    let named = account
        .config
        .get("calendar_account")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    crate::person_settings::linked_calendar(held, named, account.id.as_str())
}

/// The `CalDAV` server `typed` names, as a sign-in may go to it: a bare name (`dav.example.org`)
/// is that server over https. It is held to the one rule every calendar sign-in is under
/// ([`accounts::is_secure_endpoint`]), encrypted or this machine itself, and carries no sign-in
/// of its own.
pub(super) fn calendar_server(typed: &str) -> Result<tilo_dav::Url, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("A calendar account has a server address".to_owned());
    }
    let spelled = if typed.contains("://") {
        typed.to_owned()
    } else {
        format!("https://{typed}")
    };
    let url = tilo_dav::Url::parse(&spelled)
        .map_err(|_| format!("{typed} is not a calendar server's address"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("The server's address must not carry a sign-in".to_owned());
    }
    if !accounts::is_secure_endpoint(url.as_str()) {
        return Err(format!(
            "{typed} is not reached over https, so its sign-in would be sent in the clear"
        ));
    }
    Ok(url)
}

/// The id of the calendar account `username` signs in with at `server`.
fn caldav_id(username: &str, server: &tilo_dav::Url) -> String {
    format!("caldav:{username}@{}", server.authority())
}

/// The id the calendar account `adding` names would be kept under, where its server's address
/// reads.
pub(crate) fn calendar_account_id(server: &str, username: &str) -> Option<String> {
    let username = username.trim();
    let server = calendar_server(server).ok()?;
    (!username.is_empty()).then(|| caldav_id(username, &server))
}

/// Whether `account` is connected for reading only.
fn is_read_only(account: &accounts::Account) -> bool {
    account
        .config
        .get("read_only")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The calendars `found` on `account`'s server, as the account keeps them: the ones it held
/// before keep the choice its person made for them, a calendar the server holds now shows, and
/// every one of an account connected for reading only is one that is not written, whatever its
/// server lets its person do.
fn calendars_found(
    found: Vec<tilo_dav::discovery::CalendarCollection>,
    account: &accounts::Account,
    read_only: bool,
) -> Result<Value, String> {
    let before: Vec<accounts::calendar::RemoteCalendar> = match account.config.get("calendars") {
        Some(Value::Null) | None => Vec::new(),
        Some(calendars) => serde_json::from_value(calendars.clone())
            .map_err(|error| format!("This account's kept calendars do not read: {error}"))?,
    };
    let calendars: Vec<accounts::calendar::RemoteCalendar> = found
        .into_iter()
        .enumerate()
        .map(|(place, calendar)| {
            let had = before.iter().find(|had| had.id == calendar.href);
            accounts::calendar::RemoteCalendar {
                id: calendar.href,
                name: calendar.name,
                color: had.map_or_else(
                    || u8::try_from(place % 7).unwrap_or_default(),
                    |had| had.color,
                ),
                visible: had.is_none_or(|had| had.visible),
                read_only: calendar.read_only || read_only,
            }
        })
        .collect();
    serde_json::to_value(&calendars).map_err(|error| error.to_string())
}

/// The calendar account `account`, as the accounts list answers one.
fn calendar_row(account: &accounts::Account) -> MailAccount {
    MailAccount {
        id: account.id.to_string(),
        address: field(account, "username"),
        display_name: account.display_name.clone(),
        has_secret: has_sign_in(account),
        server: field(account, "server_url"),
        services: AccountServices {
            mail: false,
            calendar: true,
            files: false,
        },
        ..MailAccount::default()
    }
}

/// The calendar account kept beside a provider's mail account, signed in with
/// the password its person gave. Its server is not asked here: the calendar
/// bridge asks it when it keeps the account's calendars, and the person
/// chooses among them in Settings.
async fn keep_calendar_account(
    vault: VaultAt<'_>,
    server_url: &str,
    username: &str,
    password: &str,
    name: &str,
    mail_account: Option<&str>,
) -> Result<accounts::Account, String> {
    let username = username.trim().to_owned();
    if username.is_empty() || password.is_empty() {
        return Err("A calendar account has a user name and a password".into());
    }
    let server = tilo_dav::Url::parse(server_url).map_err(|error| error.to_string())?;
    let id = caldav_id(&username, &server);
    let display_name = if name.trim().is_empty() {
        username.clone()
    } else {
        name.trim().to_owned()
    };
    let mut stored =
        accounts::Account::new(accounts::AccountId::from(id), accounts::calendar::TYPE_ID);
    stored.display_name = display_name.clone();
    stored.config = serde_json::json!({
        "display_name": display_name,
        "server_url": server.as_str(),
        "username": username,
        "calendars": [],
        "collapsed": false,
    });
    // The mail account whose calendars these are: what reads the link back when the account's
    // services, or the account itself, go.
    if let Some(mail) = mail_account
        && let Some(config) = stored.config.as_object_mut()
    {
        config.insert("mail_account".to_owned(), mail.into());
    }
    let places = places_of(&stored)?;
    let owner = stored.id.to_string();
    let before = vault
        .accounts()
        .await?
        .account(stored.id.as_str())
        .and_then(|kept| sign_in_of(&kept.config));
    let reference = vault
        .keep_sign_in(
            &owner,
            (accounts::vault_items::ItemKind::Login, places),
            &username,
            zeroize::Zeroizing::new(password.to_owned()),
            before.as_ref(),
        )
        .await?;
    accounts::email::set_password_item(&mut stored.config, Some(&reference.to_string()));
    Ok(stored)
}

/// The services `account` feeds: the ones its settings name, else mail alone
/// for an account kept before it said, and what its provider offers for one
/// that never had mail.
/// Whether `account` is one a cloud mount serves: its files and, for
/// `Nextcloud`, its calendars are chosen in the words its mount reads.
fn is_file_account(account: &accounts::Account) -> bool {
    matches!(
        account.type_id.as_str(),
        accounts::cloud_types::GOOGLE_DRIVE
            | accounts::cloud_types::ONEDRIVE
            | accounts::cloud_types::DROPBOX
            | accounts::cloud_types::ICLOUD_DRIVE
            | accounts::cloud_types::NEXTCLOUD
            | accounts::cloud_types::WEBDAV
    )
}

pub(super) fn services_of(account: &accounts::Account, provider: Provider) -> AccountServices {
    // A file account's choice is written in the words the cloud service
    // reads (`use_files`, `use_calendar`) as well as its own (`services`): the
    // words the cloud service reads stand where no choice was ever written,
    // so a mount keeps following them.
    if is_file_account(account)
        && let Some(written) = account.config.get("services")
    {
        match serde_json::from_value::<AccountServices>(written.clone()) {
            Ok(services) => return services,
            Err(error) => tracing::warn!(
                %error,
                "the account's services do not read, so its own words stand"
            ),
        }
    }
    if is_file_account(account) {
        let flag = |key: &str, usual: bool| {
            account
                .config
                .get(key)
                .and_then(Value::as_bool)
                .unwrap_or(usual)
        };
        return AccountServices {
            mail: flag("use_mail", false),
            calendar: flag("use_calendar", false),
            files: flag("use_files", true),
        };
    }
    let default = if account.type_id.as_str() == accounts::email::TYPE_ID {
        AccountServices::default()
    } else {
        provider.offers()
    };
    let Some(value) = account.config.get("services") else {
        return default;
    };
    match serde_json::from_value(value.clone()) {
        Ok(services) => services,
        Err(error) => {
            tracing::warn!(
                account = %account.id,
                %error,
                "mail: an account's services do not read, so it is read as its provider's default"
            );
            default
        }
    }
}

/// The server `account` keeps with its provider: a Nextcloud server's
/// address, or the calendar server of an account whose calendars are its
/// service.
fn server_of(account: &accounts::Account) -> String {
    let named = field(account, "server_url");
    if named.is_empty() {
        field(account, "server")
    } else {
        named
    }
}

/// Why no servers answered for `address`, in words its person reads and can
/// act on: the words that are no address stay, and any other failure names
/// the domain whose servers were not found.
fn no_servers(why: &str, address: &str) -> String {
    if why == tilo_account_probe::mail_servers::NOT_AN_ADDRESS {
        return why.to_owned();
    }
    match address.trim().rsplit_once('@') {
        Some((_, domain)) if !domain.is_empty() => {
            format!("No mail servers were found for {domain}.")
        }
        _ => "No mail servers were found for that address.".to_owned(),
    }
}

/// Where the mail of `address` lives, as a person reads it.
fn found_servers(found: MailServers) -> FoundServers {
    let provider = provider_of(&found);
    FoundServers {
        provider,
        incoming: ServerAt {
            name: found.incoming.name,
            port: found.incoming.port,
        },
        outgoing: ServerAt {
            name: found.outgoing.name,
            port: found.outgoing.port,
        },
        username: found.username,
    }
}

/// Whether every address `name` answers with is on the public internet. A
/// name nobody answers for is asked nothing either. Looks the name up, so it
/// is called off the threads that carry out turns.
pub(super) fn answers_in_public(name: &str) -> bool {
    tilo_account_probe::imap::Reach::public().takes(name)
}

/// The account `id` as the listing answers it.
pub(super) fn kept_mail(
    held: &accounts::person::AccountList,
    root: &Path,
    id: &str,
) -> Result<MailAccount, String> {
    mail_accounts(held, root)
        .accounts
        .into_iter()
        .find(|kept| kept.id == id)
        .ok_or_else(|| "the account was kept but did not read back".to_owned())
}

/// Change the settings kept of the account `id` with `change`, and keep the
/// account so in the workspace's accounts.
async fn change_config(
    vault: VaultAt<'_>,
    id: &str,
    change: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>,
) -> Result<accounts::Account, String> {
    let Some(mut account) = vault.accounts().await?.account(id) else {
        return Err("That account is not kept here".into());
    };
    let kept = std::mem::take(&mut account.config);
    let mut config = match kept {
        Value::Object(config) => config,
        Value::Null => Map::new(),
        _ => return Err("That account's settings do not read".into()),
    };
    change(&mut config)?;
    account.config = Value::Object(config);
    vault.keep_account(&account).await?;
    Ok(account)
}

/// Keep `services` as the ones `account` feeds: the account's own words,
/// and for a file account the `use_files` and `use_calendar` its cloud mount
/// reads, so what it feeds and what it mounts never disagree.
async fn set_services_in_config(
    vault: VaultAt<'_>,
    account: &accounts::Account,
    services: AccountServices,
) -> Result<(), String> {
    let file_account = is_file_account(account);
    change_config(vault, account.id.as_str(), |config| {
        config.insert(
            "services".into(),
            serde_json::to_value(services).map_err(|error| error.to_string())?,
        );
        if file_account {
            config.insert("use_files".into(), services.files.into());
            config.insert("use_calendar".into(), services.calendar.into());
            config.insert("use_mail".into(), services.mail.into());
        }
        Ok(())
    })
    .await
    .map(drop)
}

fn field(account: &accounts::Account, name: &str) -> String {
    account
        .config
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn port(account: &accounts::Account, name: &str, default: u16) -> u16 {
    account
        .config
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or(default)
}

/// `account`, the id an action names, where it names one.
fn named(account: String) -> Result<String, ActionCallError> {
    if account.trim().is_empty() {
        return Err(ActionCallError::invalid_request(
            "the request names its account",
        ));
    }
    Ok(account)
}
