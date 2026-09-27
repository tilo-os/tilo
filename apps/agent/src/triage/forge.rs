//! Where an approved issue is filed.
//!
//! The triage agent never holds a forge token. It names the forge account
//! the team connected in Settings and hands the approved draft to account
//! custody, which files it with the token sealed for that account
//! ([`account_custody::Custodian::file_forge_issue`]).

use account_custody::forge::{FiledIssue, ForgeIssue};
use accounts::AccountId;
use chat::feedback::IssueDraft;

/// Something that files an approved issue.
pub trait Forge: Send {
    /// File `draft`, returning where it was filed.
    ///
    /// # Errors
    ///
    /// Why it was not filed, in words for the thread.
    fn file(&self, draft: &IssueDraft) -> Result<FiledIssue, String>;
}

/// The team's forge account, through account custody.
pub struct Custody {
    custodian: account_custody::Custodian,
    account: AccountId,
}

impl Custody {
    #[must_use]
    pub fn new(custodian: account_custody::Custodian, account: AccountId) -> Self {
        Self { custodian, account }
    }
}

impl Forge for Custody {
    fn file(&self, draft: &IssueDraft) -> Result<FiledIssue, String> {
        self.custodian
            .file_forge_issue(
                &self.account,
                &ForgeIssue {
                    title: draft.title.clone(),
                    body: draft.body.clone(),
                },
            )
            .map_err(|error| error.to_string())
    }
}

/// No forge account connected: drafts can be approved, and filing says why
/// it cannot happen yet.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unconnected;

impl Forge for Unconnected {
    fn file(&self, _draft: &IssueDraft) -> Result<FiledIssue, String> {
        Err("no forge account is connected for triage".into())
    }
}
