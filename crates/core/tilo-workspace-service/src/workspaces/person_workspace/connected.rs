//! The accounts a person connected at issuers their apps describe, as a record of their own
//! workspace: who they are at each issuer, for the app that described it.
//!
//! One entry per app and issuer, named `<app> <issuer>`: the account's id and name there, the
//! issuer as the app described it and the client it was signed in as, and which item of the
//! person's vault keeps the grant. Nothing secret is in the record. It is a resource of the
//! person's own workspace, so it reaches every device of theirs and no one else's, and every
//! change is a typed action with an author and a history.

use tilo_resource::{Collection, Id, Model, Refusal, resource};

/// The record of the person's connected accounts in their own workspace.
pub const CONNECTED: (&str, &str) = ("so.tilo.connected-accounts", "connected-accounts");

/// The most accounts one person connects.
pub const MAX_CONNECTED: usize = 256;

/// The person's connected accounts, by app and issuer.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ConnectedAccounts {
    pub accounts: Collection<Connected>,
}

/// One account the person connected.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    /// The app that described the issuer.
    pub app: String,
    /// The issuer's id, as that app describes it.
    pub issuer: String,
    /// The account's id at the issuer.
    pub account: String,
    /// The name the account goes by there.
    pub name: String,
    /// The issuer as the app described it and the client it was signed in as, as JSON: what
    /// gives the grant back when the account is forgotten.
    pub described: String,
    /// The item of the person's vault that keeps the grant.
    pub grant: String,
}

/// The id of `app`'s issuer `issuer`'s entry.
#[must_use]
pub fn entry_id(app: &str, issuer: &str) -> Id {
    Id::from_raw(format!("{app} {issuer}"))
}

fn named(what: &str, text: &str) -> Result<(), Refusal> {
    if text.trim().is_empty() || text.contains(' ') {
        return Err(Refusal::new(format!(
            "a connected account names its {what}"
        )));
    }
    Ok(())
}

#[resource(kind = "tilo.accounts.connected", read_content = tilo_resource::read::fields)]
impl ConnectedAccounts {
    /// Keep the account `account` (named `name`) that `app`'s issuer `issuer` confirmed, in
    /// place of the one connected there before, with the issuer as `described` and the vault
    /// item `grant` keeping what signs it in.
    #[action(external)]
    pub fn keep(
        &mut self,
        app: String,
        issuer: String,
        account: String,
        name: String,
        described: String,
        grant: String,
    ) -> Result<(), Refusal> {
        named("app", &app)?;
        named("issuer", &issuer)?;
        if account.is_empty() {
            return Err(Refusal::new(
                "a connected account has the id its issuer gave it",
            ));
        }
        let id = entry_id(&app, &issuer);
        let entry = Connected {
            app,
            issuer,
            account,
            name,
            described,
            grant,
        };
        if let Some(kept) = self.accounts.get_mut(&id) {
            *kept = entry;
            return Ok(());
        }
        if self.accounts.len() >= MAX_CONNECTED {
            return Err(Refusal::new("too many accounts are connected"));
        }
        self.accounts.push(id, entry);
        Ok(())
    }

    /// Forget the account connected at `app`'s issuer `issuer`.
    #[action(external)]
    pub fn forget(&mut self, app: String, issuer: String) -> Result<(), Refusal> {
        self.accounts.remove(&entry_id(&app, &issuer));
        Ok(())
    }
}
