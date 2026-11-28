//! The website sessions of a workspace: the sign-in cookies a person selected for a site, each an
//! entry of the workspace's one vault.
//!
//! A website session is one of the workspace's accounts whose settings name the vault entry that
//! holds its cookies ([`accounts::email::password_item`]). The entry is kept for that account and
//! goes with it, and every device of the workspace that may read the vault holds it. It is no
//! item anything signs in or fills a page's field with: only Browser opens its site with it,
//! asked through the session, which decides who may ask. Each time it is handed on, the use is
//! written into the entry's uses first.

use accounts::vault_items::{
    ACCOUNT_FIELD, KIND_FIELD, MAX_USES_KEPT, Place, UseRecord, VaultChange,
};
use accounts::{Account, SecretItem, SecretItemKind};
use tilo_workspace_records::vault::ItemRef;
use zeroize::Zeroizing;

use crate::person_settings::kept_sign_ins::sign_in_of;

/// What an entry holding a website session says it is: no kind of item, so nothing signs in or
/// fills a field with it.
const KIND: &str = "website-session";

/// What a website session's use is written down as.
const OPENED: &str = "tilo.browser/website-session";

/// One of the workspace's accounts with the cookies its vault entry keeps for it.
pub struct WebsiteSession {
    pub account: Account,
    /// The cookies as they were kept: `None` where the entry is gone from the vault.
    pub cookies: Option<Zeroizing<String>>,
}

/// The entry `account` names, where the vault keeps it as `account`'s website session.
fn entry_of<'a>(
    entries: &'a [(SecretItem, Vec<UseRecord>)],
    account: &Account,
) -> Option<&'a (SecretItem, Vec<UseRecord>)> {
    let item = sign_in_of(&account.config)?;
    entries.iter().find(|(entry, _)| {
        entry.id.0 == item.id()
            && entry.fields.get(KIND_FIELD).map(String::as_str) == Some(KIND)
            && entry.fields.get(ACCOUNT_FIELD) == Some(&account.id.0)
    })
}

/// What a website sign-in kept by its person themselves (`add-website-session`) is made under:
/// what [`crate::website_session_asks`] reads it back for them with.
pub const KEPT_BINDING: &str = "workspace";

impl crate::PersonServices {
    /// Answer `add-website-session` for `person`, `None` where an agent asks: only the person
    /// brings the sign-ins of their own browser, kept for them alone.
    pub(crate) async fn bring_website_sign_in(
        &self,
        workspace: &str,
        person: Option<[u8; 32]>,
        brought: tilo_workspace_records::accounts::AccountsAction,
    ) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
        use tilo_actions::remote::ActionCallError;
        let tilo_workspace_records::accounts::AccountsAction::AddWebsiteSession {
            id,
            title,
            service,
            site,
            cookies,
        } = brought
        else {
            return Err(ActionCallError::UnknownAction);
        };
        let person = person.ok_or_else(|| {
            ActionCallError::refused("Only the person brings their own website sign-ins")
        })?;
        let cookies = Zeroizing::new(cookies);
        let named = (id.as_str(), title.as_str(), service.as_str(), site.as_str());
        Box::pin(self.keep_website_sign_in(workspace, person, named, &cookies))
            .await
            .map_err(ActionCallError::refused)?;
        self.person_settings_changed();
        Ok(serde_json::json!({"id": id, "title": title}))
    }

    /// Keep the cookies `cookies` (a JSON list) the person `person` was signed in to sites with
    /// as the workspace's website sign-in `id`, titled `title`, for `service` (`web`, `slack`,
    /// `discord`) and opening `site`: what `add-website-session` keeps. Only cookies of the
    /// sites `service` signs in to are kept; anything else is refused before the vault is
    /// opened. Keeping it again under its id keeps the newer cookies in its place.
    ///
    /// # Errors
    /// The id, the service or the cookies do not read, or the vault or the accounts record does
    /// not take the write.
    pub async fn keep_website_sign_in(
        &self,
        workspace: &str,
        person: [u8; 32],
        (id, title, service, site): (&str, &str, &str, &str),
        cookies: &Zeroizing<String>,
    ) -> Result<(), String> {
        use browser_continuity_core::sessions::{Cookie, Service, SessionCookies};
        let service = Service::from_word(service)
            .ok_or_else(|| format!("{service} is no service a website sign-in is for"))?;
        let unreadable = || "Those cookies are no website sign-in this workspace keeps".to_owned();
        let read: Vec<Cookie> = serde_json::from_str(cookies).map_err(|_| unreadable())?;
        let session = SessionCookies {
            schema: 1,
            session_id: id.to_owned(),
            service,
            identity_public_key: person,
            setup_binding: KEPT_BINDING.to_owned(),
            source_snapshot_digest: *blake3::hash(cookies.as_bytes()).as_bytes(),
            cookies: read,
            origin_storage: Vec::new(),
        };
        session.validate().map_err(|_| unreadable())?;
        let kept = Zeroizing::new(serde_json::to_string(&session).map_err(|_| unreadable())?);
        let mut account = Account::new(
            id,
            account_credentials::service_sessions::SERVICE_SESSION_TYPE,
        );
        title.clone_into(&mut account.display_name);
        account.config =
            serde_json::json!({"service": service.as_str(), "setup_binding": KEPT_BINDING});
        self.keep_website_session(workspace, &account, (title, site), kept)
            .await
    }

    /// Keep `cookies` as the website session `account` of `workspace`, for `site`, titled
    /// `title`: in the entry the account names where the vault keeps it there, in a new one
    /// otherwise. The account is kept among the workspace's accounts, naming the entry.
    ///
    /// # Errors
    /// `site` is no web address, the vault does not open on this device, or the vault or the
    /// accounts record does not take the write.
    pub async fn keep_website_session(
        &self,
        workspace: &str,
        account: &Account,
        (title, site): (&str, &str),
        cookies: Zeroizing<String>,
    ) -> Result<(), String> {
        let site = Place::parse(site)
            .ok()
            .filter(Place::is_web)
            .ok_or("A website session belongs to a site's web address")?;
        let opened = self.open_vault(workspace).await?;
        let mut entry = SecretItem::login(title, "", cookies.as_str());
        let held = self.workspace_accounts(workspace).await?;
        if let Some((kept, _)) = held
            .account(account.id.as_str())
            .and_then(|held| entry_of(opened.entries(), &held))
        {
            entry.id = kept.id.clone();
            entry.created_unix = kept.created_unix;
        }
        entry.kind = SecretItemKind::Other;
        entry.origins.insert(site.as_str().to_owned());
        entry.fields.insert(KIND_FIELD.to_owned(), KIND.to_owned());
        entry
            .fields
            .insert(ACCOUNT_FIELD.to_owned(), account.id.0.clone());
        let item = ItemRef::from_id(&entry.id.0).ok_or("the vault made an entry with no id")?;
        self.write_vault(&opened, vec![VaultChange::Keep(entry)], None, None)
            .await?;
        let mut account = account.clone();
        accounts::email::set_password_item(&mut account.config, Some(&item.to_string()));
        self.keep_workspace_account((workspace, &self.acting_or_own(None), None), &account)
            .await
    }

    /// Every account of `workspace` of the type `type_id`, with the cookies the vault keeps as
    /// its website session. Nothing is handed on, so no use is written.
    ///
    /// # Errors
    /// The accounts record does not read, or the vault does not open on this device.
    pub async fn website_sessions(
        &self,
        workspace: &str,
        type_id: &str,
    ) -> Result<Vec<WebsiteSession>, String> {
        let accounts = self.workspace_accounts(workspace).await?.all();
        let opened = self.open_vault(workspace).await?;
        Ok(accounts
            .into_iter()
            .filter(|account| account.type_id.as_str() == type_id)
            .map(|account| WebsiteSession {
                cookies: entry_of(opened.entries(), &account)
                    .map(|(entry, _)| Zeroizing::new(entry.password.clone())),
                account,
            })
            .collect())
    }

    /// Write down that `app` opened its site with `account`'s website session, before it is
    /// handed to it.
    ///
    /// # Errors
    /// The vault does not open on this device, the entry is gone, or the use was not written.
    pub async fn website_session_opened(
        &self,
        workspace: &str,
        account: &Account,
        app: &str,
    ) -> Result<(), String> {
        let opened = self.open_vault(workspace).await?;
        let (entry, uses) = entry_of(opened.entries(), account)
            .ok_or("That website session is no longer in the workspace's vault")?;
        let mut uses = uses.clone();
        uses.push(UseRecord {
            at_unix: tilo_clock::unix_now(),
            places: entry
                .origins
                .iter()
                .filter_map(|origin| {
                    tilo_failure::ignored!(
                        "a use names the places of the session's origins that are web places",
                        Place::parse(origin).ok()
                    )
                })
                .collect(),
            by: String::new(),
            using: OPENED.to_owned(),
            what: format!("{app} opened {}", account.display_name),
            allowed: None,
        });
        let excess = uses.len().saturating_sub(MAX_USES_KEPT);
        uses.drain(..excess);
        self.write_vault(
            &opened,
            vec![VaultChange::Uses(entry.id.clone(), uses)],
            None,
            None,
        )
        .await
    }

    /// Forget `account`'s website session: its cookies leave the vault first, then the account
    /// leaves the workspace's accounts, so a removal that fails half way leaves no cookies
    /// behind and is finished by the next.
    ///
    /// # Errors
    /// The vault does not open on this device, or the vault or the accounts record does not
    /// take the write.
    pub async fn forget_website_session(
        &self,
        workspace: &str,
        account: &Account,
    ) -> Result<(), String> {
        let opened = self.open_vault(workspace).await?;
        if let Some((entry, _)) = entry_of(opened.entries(), account) {
            self.write_vault(
                &opened,
                vec![VaultChange::Forget(entry.id.clone())],
                None,
                None,
            )
            .await?;
        }
        self.remove_workspace_account(
            (workspace, &self.acting_or_own(None), None),
            account.id.as_str(),
        )
        .await
        .map(drop)
    }
}
