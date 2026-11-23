//! The workspace's mail accounts as their holder keeps them: the servers of an
//! address worked out from the address alone, an account added once its
//! server took its sign-in, and which of its services its person uses.
//!
//! Nothing here carries a secret back: a reading says an account's address
//! and its servers, never its password.

use std::path::Path;

use serde_json::{Map, Value};
use tilo_account_probe::mail_servers::MailServers;
use tilo_actions::remote::person_accounts::{
    self, AddMailAccount, FindServers, FoundServers, MailAccount, MailAccounts, Provider, ServerAt,
    Services, SetServices,
};
use tilo_actions::remote::{ActionCallError, InvokeRequest};

use super::kept_sign_ins::{Signing, has_sign_in, sign_in_of};
use super::{add_mail_with, answer, argument, remove_mail};
use crate::workspaces::Workspaces;
use crate::workspaces::vault::VaultAt;

impl Workspaces {
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
            let service = super::calendars::Service {
                server_url: server.clone(),
                username: adding.address.clone(),
                password: adding.password.clone(),
                name: adding.display_name.clone(),
                mail_account: adding.address.clone(),
            };
            match super::calendars::prepare_service_in(
                (signing.vault, signing.root),
                self.inner.models.mail_reach(),
                signing.by,
                service,
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
                .and_then(tilo_actions::remote::person_vault::ItemRef::from_id)
        {
            let _ = signing
                .vault
                .forget_sign_in(calendar.id.as_str(), &reference)
                .await;
        }
        kept
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
    pub(super) async fn set_services(
        &self,
        signing: Signing<'_>,
        saying: SetServices,
    ) -> Result<MailAccounts, String> {
        let held = signing.vault.accounts().await?;
        let account = held
            .account(&saying.account)
            .ok_or_else(|| "That account is not kept here".to_owned())?;
        let provider = provider_of_account(&account);
        #[cfg(feature = "agents")]
        let mut services = saying.services.within(provider);
        // A build without the calendar module keeps no calendar account.
        #[cfg(not(feature = "agents"))]
        let services = Services {
            calendar: false,
            ..saying.services.within(provider)
        };
        // An iCloud account's calendars go with its sign-in: kept where they
        // are turned on, removed with what was kept of them where they are
        // turned off, each in one change with the mail account's own settings.
        #[cfg(feature = "agents")]
        if provider == Provider::Icloud {
            let linked = linked_calendar(&held, &account);
            if services.calendar && linked.is_none() {
                match self.keep_service_calendar(signing, &saying.account).await {
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
                            account = %saying.account,
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
                super::calendars::forget_kept(signing.root, calendar);
                return Ok(mail_accounts(
                    &signing.vault.accounts().await?,
                    signing.root,
                ));
            }
        }
        set_services_in_config(signing.vault, &saying.account, services).await?;
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
        let (kept, secret) = super::account_with_secret(signing, account).await?;
        let address = field(&kept, "email");
        let service = super::calendars::Service {
            server_url: self.icloud_calendar_server(),
            username: if address.is_empty() {
                kept.id.to_string()
            } else {
                address
            },
            password: secret.password,
            name: kept.display_name.clone(),
            mail_account: kept.id.to_string(),
        };
        super::calendars::prepare_service_in(
            (signing.vault, signing.root),
            self.inner.models.mail_reach(),
            signing.by,
            service,
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

    /// Answer a request that finds an address's servers, adds an account or
    /// says what it feeds, or none when `request` asks anything else: what a
    /// person does with the accounts their vault keeps.
    pub(super) async fn person_mail_action(
        &self,
        signing: Signing<'_>,
        request: &InvokeRequest,
    ) -> Result<Option<Value>, ActionCallError> {
        let answered = match request.target.address.action.as_str() {
            person_accounts::FIND_SERVERS => {
                let asking: FindServers = argument(request, "an address whose servers to find")?;
                answer(&self.find_servers(&asking.address).await)
            }
            person_accounts::ADD_MAIL => {
                let adding: AddMailAccount = argument(request, "an account to add")?;
                let kept = self.add_mail(signing, adding).await;
                if kept.is_ok() {
                    self.person_settings_changed();
                }
                answer(&kept)
            }
            person_accounts::SET_SERVICES => {
                let saying: SetServices = argument(request, "an account and its services")?;
                let kept = self.set_services(signing, saying).await;
                if kept.is_ok() {
                    self.person_settings_changed();
                }
                answer(&kept)
            }
            person_accounts::REMOVE => {
                let account = one_account(request)?;
                let kept = remove_mail((signing.vault, signing.root), &account).await;
                if kept.is_ok() {
                    self.person_settings_changed();
                }
                answer(&kept)
            }
            _ => return Ok(None),
        };
        Ok(Some(answered?))
    }
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
        signing_in: super::sign_in::signing_in(root),
        // Which providers this server begins a sign-in with is the sign-in
        // module's to say.
        signs_in_with: super::sign_in::signs_in_with(),
    }
}

/// The provider of `account`: the one its settings name, else the one its
/// servers are known by.
fn provider_of_account(account: &accounts::Account) -> Provider {
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
    services: Services,
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
    super::calendars::linked_to(held, named, account.id.as_str())
}

/// The services `account` feeds: the ones its settings name, else mail alone
/// for an account kept before it said, and what its provider offers for one
/// that never had mail.
fn services_of(account: &accounts::Account, provider: Provider) -> Services {
    let default = if account.type_id.as_str() == accounts::email::TYPE_ID {
        Services::default()
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

/// Keep `services` as the ones the account `id` feeds.
async fn set_services_in_config(
    vault: VaultAt<'_>,
    id: &str,
    services: Services,
) -> Result<(), String> {
    change_config(vault, id, |config| {
        config.insert(
            "services".into(),
            serde_json::to_value(services).map_err(|error| error.to_string())?,
        );
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

/// The account a request names, as `{ "account": … }`.
fn one_account(request: &InvokeRequest) -> Result<String, ActionCallError> {
    let value: Map<String, Value> = argument(request, "the account")?;
    value
        .get("account")
        .and_then(Value::as_str)
        .filter(|account| !account.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ActionCallError::invalid_request("the request names its account"))
}
