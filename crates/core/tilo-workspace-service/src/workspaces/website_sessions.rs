//! The website sessions of a workspace: the sign-in cookies a person selected for a site, each an
//! entry of the workspace's one vault.
//!
//! A website session is one of the workspace's accounts whose settings name the vault entry that
//! holds its cookies ([`accounts::email::password_item`]). The entry is kept for that account and
//! goes with it, and every device of the workspace that may read the vault holds it. It is no
//! item anything signs in or fills a page's field with: only Browser opens its site with it,
//! asked through the session, which decides who may ask. Each time it is handed on, the use is
//! written into the entry's uses first.

use crate::workspace_vault::ItemRef;
use accounts::vault_items::{
    ACCOUNT_FIELD, KIND_FIELD, MAX_USES_KEPT, Place, UseRecord, VaultChange,
};
use accounts::{Account, SecretItem, SecretItemKind};
use zeroize::Zeroizing;

use super::Workspaces;
use super::person_settings::kept_sign_ins::sign_in_of;

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

impl Workspaces {
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
