//! The person's accounts in a window: each account's settings from their
//! record, its secret from this device's custody (in a session, released by
//! the session's account custody, [`crate::custody`]).

use super::{AccountConfig, AccountStore, EngineRequest, MailApp, MessageId};

impl MailApp {
    pub(super) fn adopt_person_accounts(&mut self) {
        let root = self.state().accounts_persist.root();
        let Some((record, seen)) = &mut self.person_accounts else {
            return;
        };
        let revision = record.revision();
        if *seen == Some(revision) {
            return;
        }
        *seen = Some(revision);
        let person = record.get();
        let mut adopted = if self.custody.is_some() {
            AccountStore::without_secrets(&person.store())
        } else if let Some(root) = root {
            let vault = match accounts::AccountCredentials::open_in(&root) {
                Ok(vault) => vault,
                Err(error) => {
                    tracing::error!(%error, "mail account credentials could not be opened");
                    *seen = None;
                    return;
                }
            };
            AccountStore::from_store(&person.store(), &vault)
        } else {
            // In a session the secrets are the session's to release, and no
            // sandbox here opens the person's vault: an account shows
            // without a secret it was not released, and says so rather than
            // signing in with nothing.
            AccountStore::without_secrets(&person.store())
        };
        let active_changed = {
            let mut st = self.shared.borrow_mut();
            keep_released_secrets(self.custody.as_mut(), &mut adopted, &st.accounts);
            let previous = st.accounts.active_account().cloned();
            if let Some(shown) = st
                .accounts
                .active_account()
                .map(|account| account.id.clone())
                && let Some(index) = adopted.accounts.iter().position(|a| a.id == shown)
            {
                adopted.active = index;
            }
            for account in &mut adopted.accounts {
                if let Some(previous) = st.accounts.accounts.iter().find(|a| a.id == account.id) {
                    account.collapsed = previous.collapsed;
                }
            }
            let active_changed = previous.as_ref().map(|a| (&a.id, &a.config))
                != adopted.active_account().map(|a| (&a.id, &a.config));
            // An account removed in Settings takes its mail with it, here
            // and from what this device keeps.
            let removed: Vec<String> = st
                .accounts
                .accounts
                .iter()
                .filter(|old| !adopted.accounts.iter().any(|new| new.id == old.id))
                .map(|old| old.id.clone())
                .collect();
            for id in &removed {
                let gone: Vec<MessageId> = st.mailbox.of_account(id).map(|m| m.id).collect();
                for message in gone {
                    st.mailbox.purge(message);
                }
                self.state().store.forget(id);
                self.kept_loaded.remove(id);
            }
            if !removed.is_empty() {
                st.selected = None;
            }
            st.accounts = adopted;
            active_changed
        };
        // An empty window has already completed its first sync. Its first
        // Settings-created account must connect here, not wait for the
        // one-time startup path to run again. Inactive additions need no fetch.
        if self.shared.borrow().accounts.is_empty() {
            self.connect_requested = false;
        } else if self.awaits_active_secret() {
            // It connects once custody released its secret.
            self.connect_requested = false;
        } else if self.state().engine.is_some() && (active_changed || !self.connect_requested) {
            let (cfg, folder) = {
                let st = self.shared.borrow();
                (st.accounts.active_config(), st.folder)
            };
            self.state()
                .post_engine(EngineRequest::connect(cfg.clone()));
            self.state().post_engine(EngineRequest::fetch(cfg, folder));
            self.connect_requested = true;
        }
        self.dirty = true;
    }

    pub(super) fn awaits_active_secret(&self) -> bool {
        let Some(custody) = &self.custody else {
            return false;
        };
        self.shared
            .borrow()
            .accounts
            .active_account()
            .is_some_and(|account| {
                needs_released_secret(&account.config) && custody.awaits(&account.id)
            })
    }

    pub(super) fn take_released_secrets(&mut self) {
        let wake = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(custody) = self.custody.as_mut() else {
            return;
        };
        let released = custody.poll(wake.as_ref());
        if released.is_empty() {
            return;
        }
        let mut st = self.shared.borrow_mut();
        let active = st
            .accounts
            .active_account()
            .map(|account| account.id.clone());
        let mut connect = false;
        for answer in released {
            match answer {
                crate::custody::Released::Secret { account, password } => {
                    if let Some(kept) = st.accounts.accounts.iter_mut().find(|a| a.id == account) {
                        password.as_str().clone_into(&mut kept.config.password);
                    }
                    connect |= active.as_deref() == Some(account.as_str());
                }
                crate::custody::Released::Refused { account, error } => {
                    tracing::error!(%account, %error, "the session's account custody did not release this account's secret");
                    if active.as_deref() == Some(account.as_str()) {
                        // The account cannot sign in until its secret is
                        // released, and the person fixes that in Settings.
                        let label = st
                            .accounts
                            .label_for(&account)
                            .unwrap_or_else(|| account.clone());
                        st.note_sync_failure(label, crate::state::SyncCause::SignedOut);
                    }
                }
            }
        }
        drop(st);
        if connect && self.state().engine.is_some() {
            let (cfg, folder) = {
                let st = self.shared.borrow();
                (st.accounts.active_config(), st.folder)
            };
            self.state()
                .post_engine(EngineRequest::connect(cfg.clone()));
            self.state().post_engine(EngineRequest::fetch(cfg, folder));
            self.connect_requested = true;
        }
        self.dirty = true;
    }
}

fn needs_released_secret(config: &AccountConfig) -> bool {
    config.auth == crate::model::AuthMethod::Password
        && !config.import_pending
        && config.password.is_empty()
}

fn keep_released_secrets(
    custody: Option<&mut crate::custody::SecretRelease>,
    adopted: &mut AccountStore,
    shown: &AccountStore,
) {
    for account in &mut adopted.accounts {
        if account.config.password.is_empty()
            && let Some(previous) = shown.accounts.iter().find(|a| a.id == account.id)
        {
            let mut unchanged = previous.config.clone();
            unchanged.password = String::new();
            if unchanged == account.config {
                account
                    .config
                    .password
                    .clone_from(&previous.config.password);
            }
        }
    }
    if let Some(custody) = custody {
        for account in &adopted.accounts {
            if needs_released_secret(&account.config) {
                custody.ask(&account.id);
            }
        }
    }
}
