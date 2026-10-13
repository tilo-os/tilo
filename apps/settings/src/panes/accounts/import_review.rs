use accounts::email::AuthMethod;
use accounts::thunderbird::{Candidate, DiscoveryError};
use serde::Serialize;

use crate::accounts::AccountRoot;
use crate::i18n::{t, t_args};

#[derive(Clone, Debug, Serialize)]
pub(super) struct Choice {
    pub candidate: Candidate,
    pub selected: bool,
    pub authentication: Option<AuthMethod>,
}

#[derive(Default)]
pub(super) struct ImportReview {
    pub revision: u64,
    pub choices: Vec<Choice>,
    pub status: Option<String>,
    before: Option<accounts::AccountStore>,
}

impl ImportReview {
    pub fn begin(&mut self, root: &AccountRoot) -> Result<u64, String> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| t("accounts-import-stale"))?;
        self.choices.clear();
        self.status = Some(t("accounts-import-reading"));
        self.before = Some(root.store());
        Ok(self.revision)
    }

    pub fn receive(
        &mut self,
        revision: u64,
        outcome: Result<Vec<Candidate>, DiscoveryError>,
    ) -> Result<(), String> {
        if revision != self.revision {
            return Err(t("accounts-import-stale"));
        }
        match outcome {
            Ok(candidates) => {
                self.choices = candidates
                    .into_iter()
                    .map(|candidate| Choice {
                        candidate,
                        selected: false,
                        authentication: None,
                    })
                    .collect();
                self.status = Some(t(if self.choices.is_empty() {
                    "accounts-import-none"
                } else {
                    "accounts-import-review"
                }));
                Ok(())
            }
            Err(error) => {
                let message = error_message(error);
                self.status = Some(message.clone());
                Err(message)
            }
        }
    }

    pub fn select(
        &mut self,
        source: &str,
        authentication: &str,
        selected: bool,
        revision: u64,
    ) -> Result<(), String> {
        if revision != self.revision {
            return Err(t("accounts-import-stale"));
        }
        let auth = match authentication {
            "password" => AuthMethod::Password,
            "oauth" => AuthMethod::OAuth,
            _ => return Err(t("accounts-import-auth-required")),
        };
        let next = self
            .revision
            .checked_add(1)
            .ok_or_else(|| t("accounts-import-stale"))?;
        let choice = self
            .choices
            .iter_mut()
            .find(|choice| identity(&choice.candidate) == source)
            .ok_or_else(|| t("accounts-import-stale"))?;
        if selected {
            choice
                .candidate
                .reviewed_config(auth)
                .map_err(error_message)?;
        }
        choice.selected = selected;
        choice.authentication = Some(auth);
        self.revision = next;
        Ok(())
    }

    /// Save ordinary connection metadata only. Never opens custody, copies a
    /// secret, calls a server, or overwrites an already configured account.
    pub fn commit(&mut self, root: &AccountRoot, revision: u64) -> Result<String, String> {
        if revision != self.revision {
            return Err(t("accounts-import-stale"));
        }
        let mut store = root.store();
        if self.before.as_ref() != Some(&store) {
            return Err(t("accounts-import-accounts-changed"));
        }
        let mut additions = Vec::new();
        for choice in self.choices.iter().filter(|choice| choice.selected) {
            let authentication = choice
                .authentication
                .ok_or_else(|| t("accounts-import-auth-required"))?;
            let config = choice
                .candidate
                .reviewed_config(authentication)
                .map_err(error_message)?;
            let id = accounts::AccountId::from(config.email.to_ascii_lowercase());
            if store.get(&id).is_some() {
                continue;
            }
            if additions
                .iter()
                .any(|prior: &accounts::Account| prior.id == id)
            {
                return Err(t("accounts-import-duplicate"));
            }
            let mut account = accounts::Account::new(id, accounts::thunderbird::PENDING_TYPE);
            account.display_name.clone_from(&config.display_name);
            account.set_config(&config);
            additions.push(account);
        }
        if !self.choices.iter().any(|choice| choice.selected) {
            return Err(t("accounts-import-select"));
        }
        let count = additions.len();
        for account in additions {
            store.upsert(account);
        }
        if count != 0 {
            root.save_store(&store)
                .map_err(|_| t("accounts-import-save-failed"))?;
        }
        self.before = Some(store);
        let status = t_args("accounts-import-saved", &[("count", count.into())]);
        self.status = Some(status.clone());
        Ok(status)
    }

    pub fn cancel(&mut self) -> Result<(), String> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| t("accounts-import-stale"))?;
        self.choices.clear();
        self.before = None;
        self.status = None;
        Ok(())
    }

    pub fn inspect(&self) -> Result<String, String> {
        serde_json::to_string(&serde_json::json!({
            "revision": self.revision,
            "accounts": self.choices,
            "status": self.status,
            "credentials_imported": false,
            "access_granted": false,
        }))
        .map_err(|_| t("accounts-import-read-failed"))
    }
}

pub(super) fn identity(candidate: &Candidate) -> String {
    format!("{}:{}", candidate.source_account, candidate.source_identity)
}

pub(super) fn error_message(error: DiscoveryError) -> String {
    t(match error {
        DiscoveryError::Read => "accounts-import-read-failed",
        DiscoveryError::NotPreferencesFile => "accounts-import-prefs-required",
        DiscoveryError::TooLarge | DiscoveryError::TooMany => "accounts-import-too-large",
        DiscoveryError::Malformed | DiscoveryError::MissingMetadata => "accounts-import-malformed",
        DiscoveryError::Unsupported => "accounts-import-unsupported",
    })
}

#[cfg(test)]
mod tests;
