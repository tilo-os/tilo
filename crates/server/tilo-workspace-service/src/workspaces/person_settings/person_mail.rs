//! The person's mail accounts as their holder keeps them: the servers of an
//! address worked out from the address alone, an account added once its
//! server took its sign-in, who provides it, which of its services its
//! person uses, and the calendar account kept as one of them.
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

#[cfg(feature = "agents")]
use super::calendars;
use super::kept_sign_ins::{Signing, has_sign_in};
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
    /// where its person typed none, and keeping an iCloud account's
    /// calendars with its sign-in where they asked for them
    /// (`ADD_MAIL`).
    pub(super) async fn add_mail(
        &self,
        signing: Signing<'_>,
        mut adding: AddMailAccount,
    ) -> Result<MailAccount, String> {
        let root = signing.root;
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
        // A provider whose accounts sign in on its own page takes no password
        // from a mail program: its person is told where they sign in instead.
        if provider.signs_in_on_its_page() {
            return Err(its_own_page(provider));
        }
        let password = adding.password.clone();
        let mut kept =
            add_mail_with(self.account_tester(), (signing.vault, root), adding, None).await?;
        // An iCloud account's calendars are kept with its app password, where
        // its person asked for them. A calendar server that does not take
        // them leaves the mail account as it is, feeding mail alone.
        if provider == Provider::Icloud && kept.services.calendar {
            match self.keep_added_calendars(signing, &kept, &password).await {
                Ok(calendar) => {
                    link_calendar(root, &kept.id, &calendar, &self.icloud_calendar_server())?;
                    kept = kept_mail(root, &kept.id)?;
                }
                Err(why) => {
                    tracing::warn!(
                        account = %kept.id,
                        %why,
                        "mail: an iCloud account's calendars were not kept, so it feeds mail alone"
                    );
                    kept.services.calendar = false;
                    set_services_in_config(root, &kept.id, kept.services)?;
                }
            }
        }
        Ok(kept)
    }

    /// Say which of an account's services its person uses
    /// (`SET_SERVICES`). An iCloud account's calendars go
    /// with its sign-in: kept where they are turned on, removed with what was
    /// kept of them where they are turned off. Its mail account stays
    /// whatever is said, and Mail hides it where its mail is off.
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
        let root = signing.root;
        let store = accounts::AccountStore::load_in(root);
        let id = accounts::AccountId::from(saying.account.clone());
        let account = store
            .get(&id)
            .cloned()
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
        #[cfg(feature = "agents")]
        let linked = linked_calendar(root, &account);
        #[cfg(feature = "agents")]
        if provider == Provider::Icloud {
            if services.calendar && linked.is_none() {
                match self.keep_service_calendar(signing, &account).await {
                    Ok(calendar) => {
                        link_calendar(
                            root,
                            &saying.account,
                            &calendar,
                            &self.icloud_calendar_server(),
                        )?;
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
                remove_calendar((signing.vault, root), calendar).await?;
                unlink_calendar(root, &saying.account)?;
            }
        }
        set_services_in_config(root, &saying.account, services)?;
        mail_accounts(root)
    }

    /// Keep the calendars of an added iCloud account with its sign-in,
    /// answering the calendar account's id, which the mail account keeps.
    #[cfg(feature = "agents")]
    async fn keep_added_calendars(
        &self,
        signing: Signing<'_>,
        account: &MailAccount,
        password: &str,
    ) -> Result<String, String> {
        let service = calendars::Service {
            server_url: self.icloud_calendar_server(),
            username: account.address.clone(),
            password: password.to_owned(),
            name: account.display_name.clone(),
            mail_account: account.id.clone(),
        };
        let reach =
            calendars::Asking::of(signing.vault, signing.by, self.inner.models.mail_reach());
        let root = signing.root.to_owned();
        tokio::task::spawn_blocking(move || calendars::add_service(&root, &reach, service))
            .await
            .map_err(|_| "the calendar server was not asked to the end".to_owned())?
    }

    /// Keep the calendars of a kept iCloud account with the sign-in its entry in the
    /// workspace's vault holds, answering the calendar account's id.
    #[cfg(feature = "agents")]
    async fn keep_service_calendar(
        &self,
        signing: Signing<'_>,
        account: &accounts::Account,
    ) -> Result<String, String> {
        let reference = super::kept_sign_ins::sign_in_of(&account.config)
            .ok_or_else(|| "That account has no sign-in yet for its calendars".to_owned())?;
        let places = super::kept_sign_ins::places_of(account)?;
        let password = signing
            .vault
            .use_sign_in(
                &reference,
                (accounts::vault_items::ItemKind::Login, &places),
                signing.by,
                super::kept_sign_ins::SIGN_IN,
            )
            .await?
            .secret;
        let address = field(account, "email");
        let kept = MailAccount {
            id: account.id.to_string(),
            address: if address.is_empty() {
                account.id.to_string()
            } else {
                address
            },
            display_name: account.display_name.clone(),
            ..MailAccount::default()
        };
        self.keep_added_calendars(signing, &kept, &password).await
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

    /// A build without the calendar module keeps no iCloud calendars: an
    /// added account feeds mail alone.
    #[cfg(not(feature = "agents"))]
    #[allow(
        clippy::unused_async,
        reason = "it keeps the shape of the build that does keep calendars"
    )]
    async fn keep_added_calendars(
        &self,
        _signing: Signing<'_>,
        _account: &MailAccount,
        _password: &str,
    ) -> Result<String, String> {
        Err("this build keeps no calendar accounts".to_owned())
    }

    /// Where an iCloud account's calendars are kept, in a build that keeps
    /// none.
    #[cfg(not(feature = "agents"))]
    #[allow(
        clippy::unused_self,
        reason = "it answers iCloud's own server whatever the account"
    )]
    fn icloud_calendar_server(&self) -> String {
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
pub(super) fn mail_accounts(root: &Path) -> Result<MailAccounts, String> {
    let store = accounts::AccountStore::load_in(root);
    let email = accounts::AccountTypeId::from(accounts::email::TYPE_ID);
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
    Ok(MailAccounts {
        accounts: store
            .list(&email)
            .into_iter()
            .chain(
                // A provider's account kept under another type (a Nextcloud
                // server's) feeds the apps it was signed in for, and no mail.
                store.all().iter().filter(|account| {
                    account.type_id != email && account.config.get("provider").is_some()
                }),
            )
            .map(listed)
            .collect(),
        active: store
            .active_id(&email)
            .map(ToString::to_string)
            .unwrap_or_default(),
        signing_in: super::sign_in::signing_in(root),
        // Which providers this server begins a sign-in with is the sign-in
        // module's to say.
        signs_in_with: super::sign_in::signs_in_with(),
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
pub(super) fn provider_of_servers(incoming: &str, outgoing: &str) -> Provider {
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
    FoundServers {
        provider: provider_of(&found),
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

/// What an address of a provider that signs in on a page of its own is
/// refused with: where its person signs in instead.
fn its_own_page(provider: Provider) -> String {
    let called = match provider {
        Provider::Google => "Google",
        Provider::Microsoft => "Microsoft",
        Provider::Icloud => "iCloud",
        Provider::Nextcloud => "Nextcloud",
        Provider::Other => "This provider",
    };
    format!("{called} accounts sign in on its own page: use \"Sign in with {called}\" instead")
}

/// Whether every address `name` answers with is on the public internet. A
/// name nobody answers for is asked nothing either. Looks the name up, so it
/// is called off the threads that carry out turns.
pub(super) fn answers_in_public(name: &str) -> bool {
    tilo_account_probe::imap::Reach::public().takes(name)
}

/// The account `id` as the listing answers it.
pub(super) fn kept_mail(root: &Path, id: &str) -> Result<MailAccount, String> {
    mail_accounts(root)?
        .accounts
        .into_iter()
        .find(|kept| kept.id == id)
        .ok_or_else(|| "the account was kept but did not read back".to_owned())
}

/// Change the settings kept of the account `id` with `change`, and save the
/// store.
fn change_config(
    root: &Path,
    id: &str,
    change: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>,
) -> Result<accounts::Account, String> {
    let mut store = accounts::AccountStore::load_in(root);
    let key = accounts::AccountId::from(id.to_owned());
    let Some(mut account) = store.get(&key).cloned() else {
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
    store.upsert(account.clone());
    store.save_in(root).map_err(|error| error.to_string())?;
    Ok(account)
}

/// Keep `services` as the ones the account `id` feeds.
fn set_services_in_config(root: &Path, id: &str, services: Services) -> Result<(), String> {
    change_config(root, id, |config| {
        config.insert(
            "services".into(),
            serde_json::to_value(services).map_err(|error| error.to_string())?,
        );
        Ok(())
    })
    .map(drop)
}

/// Keep `calendar` as the calendar account of the mail account `id`, and
/// `server` as where that account's calendars live.
fn link_calendar(root: &Path, id: &str, calendar: &str, server: &str) -> Result<(), String> {
    change_config(root, id, |settings| {
        settings.insert(
            "calendar_account".into(),
            Value::String(calendar.to_owned()),
        );
        settings.insert("server".into(), Value::String(server.to_owned()));
        Ok(())
    })
    .map(drop)
}

/// Forget `account`'s calendar account: what its settings say of one is
/// taken away with the calendar account itself.
#[cfg(feature = "agents")]
fn unlink_calendar(root: &Path, account: &str) -> Result<(), String> {
    change_config(root, account, |settings| {
        settings.remove("calendar_account");
        settings.remove("server");
        Ok(())
    })
    .map(drop)
}

/// The calendar account kept as `account`'s service: the one its settings
/// name, else the one kept beside the calendars that names it.
pub(super) fn linked_calendar(root: &Path, account: &accounts::Account) -> Option<String> {
    if let Some(named) = account
        .config
        .get("calendar_account")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .filter(|id| calendar_is_kept(root, id))
    {
        return Some(named.to_owned());
    }
    kept_calendar_for(root, account.id.as_str())
}

/// Whether the calendar account `id` is kept beside the person's accounts,
/// where this build keeps calendars.
#[cfg(feature = "agents")]
fn calendar_is_kept(root: &Path, id: &str) -> bool {
    calendars::is_kept(root, id)
}

#[cfg(not(feature = "agents"))]
fn calendar_is_kept(_root: &Path, _id: &str) -> bool {
    true
}

/// The calendar account kept for the mail account `id`, where this build
/// keeps calendars.
#[cfg(feature = "agents")]
fn kept_calendar_for(root: &Path, id: &str) -> Option<String> {
    calendars::linked_to(root, id)
}

#[cfg(not(feature = "agents"))]
fn kept_calendar_for(_root: &Path, _id: &str) -> Option<String> {
    None
}

/// Remove the calendar account `calendar` and what was kept of it, where
/// this build keeps calendars.
#[cfg(feature = "agents")]
pub(super) async fn remove_calendar(
    (vault, root): (VaultAt<'_>, &Path),
    calendar: &str,
) -> Result<(), String> {
    calendars::remove_account((vault, root), calendar).await
}

#[cfg(not(feature = "agents"))]
#[allow(
    clippy::unnecessary_wraps,
    clippy::unused_async,
    reason = "it keeps the shape of the build that does remove a calendar account"
)]
pub(super) async fn remove_calendar(
    (_vault, _root): (VaultAt<'_>, &Path),
    _calendar: &str,
) -> Result<(), String> {
    Ok(())
}

/// Where an iCloud account's calendars are kept.
const ICLOUD_CALENDARS: &str = "https://caldav.icloud.com";

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
        .ok_or_else(|| super::invalid("the request names its account"))
}
