//! A workspace's vault as the person services reach it: through the accounts that sign in with
//! its entries, and, once, what a host kept of it before the vault was the workspace's document.

use accounts::vault_items::{VaultChange, VaultItems};
use tilo_action_policy::Acting;
use tilo_actions::remote::InvokeRequest;
use tilo_workspace_service::workspaces::vault::OpenVault;

use crate::PersonServices;
use crate::person_settings::kept_sign_ins::HostSignIns;

/// How many changes one write carries when a host carries its earlier vault over: each write of
/// a document is bounded in size.
const CARRIED_PER_WRITE: usize = 16;

/// Where code outside the workspaces' own (a connection's sign-in) reaches a workspace's vault:
/// the vault of one workspace this device holds, or, in a test of that code alone, items in
/// hand.
#[derive(Clone, Copy)]
pub(crate) enum VaultAt<'a> {
    /// `workspace`'s vault on this device, its writes made as the acting that asked for them
    /// when one is named ([`PersonServices::vault_for`]), each keeping the call it serves when one
    /// is named ([`VaultAt::serving`]).
    Workspace(
        &'a PersonServices,
        &'a str,
        Option<&'a Acting>,
        Option<&'a InvokeRequest>,
    ),
    #[cfg(test)]
    Held(&'static TestStores),
}

/// What a test of the code that reaches a workspace's stores keeps in hand: the vault's items
/// and the workspace's accounts. It reads as its vault's items.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestStores {
    items: std::sync::Mutex<VaultItems>,
    accounts: std::sync::Mutex<accounts::person::AccountList>,
}

#[cfg(test)]
impl TestStores {
    /// The account `id`, as the workspace's accounts keep it now.
    pub(crate) fn account(&self, id: &str) -> Option<accounts::Account> {
        self.accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .account(id)
    }

    /// Keep `account` among the workspace's accounts, as a member's Settings would.
    pub(crate) fn keep_account(&self, account: &accounts::Account) {
        self.accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keep(account)
            .unwrap_or_else(|refusal| panic!("the account is not kept: {refusal}"));
    }
}

#[cfg(test)]
impl std::ops::Deref for TestStores {
    type Target = std::sync::Mutex<VaultItems>;

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl std::fmt::Debug for VaultAt<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace(_, workspace, ..) => {
                formatter.debug_tuple("Workspace").field(workspace).finish()
            }
            #[cfg(test)]
            Self::Held(_) => formatter.write_str("Held"),
        }
    }
}

impl<'a> VaultAt<'a> {
    /// The same vault, each write it makes keeping `call`, the call it is made for: the person
    /// whose device signed it is the one the write names, and the step it takes is one the
    /// called action declares ([`tilo_actions::ActionContract::effects`]).
    #[must_use]
    pub(crate) fn serving(self, call: &'a InvokeRequest) -> Self {
        match self {
            Self::Workspace(workspaces, workspace, acting, _) => {
                Self::Workspace(workspaces, workspace, acting, Some(call))
            }
            #[cfg(test)]
            held @ Self::Held(_) => held,
        }
    }

    /// The call each write of this vault keeps, when it serves one ([`Self::serving`]).
    pub(crate) const fn call(&self) -> Option<&'a InvokeRequest> {
        match *self {
            Self::Workspace(_, _, _, call) => call,
            #[cfg(test)]
            Self::Held(_) => None,
        }
    }

    /// Let `change` read and change the vault's items, writing what it changed before
    /// answering ([`PersonServices::changing_vault`]).
    ///
    /// # Errors
    /// As [`PersonServices::changing_vault`].
    pub(crate) async fn changing<T>(
        &self,
        change: impl FnOnce(&mut VaultItems) -> Result<T, accounts::vault_items::ItemError>,
    ) -> Result<T, String> {
        match self {
            Self::Workspace(workspaces, workspace, acting, call) => {
                workspaces
                    .changing_vault(workspace, (*acting, *call), change)
                    .await
            }
            #[cfg(test)]
            Self::Held(items) => {
                let mut items = items
                    .lock()
                    .map_err(|_| "the vault is poisoned".to_owned())?;
                change(&mut items).map_err(|error| error.to_string())
            }
        }
    }

    /// The workspace's accounts, as this device holds them
    /// ([`PersonServices::workspace_accounts`]).
    ///
    /// # Errors
    /// As [`PersonServices::workspace_accounts`].
    pub(crate) async fn accounts(&self) -> Result<accounts::person::AccountList, String> {
        match self {
            Self::Workspace(workspaces, workspace, ..) => {
                workspaces.workspace_accounts(workspace).await
            }
            #[cfg(test)]
            Self::Held(held) => Ok(held
                .accounts
                .lock()
                .map_err(|_| "the accounts are poisoned".to_owned())?
                .clone()),
        }
    }

    /// Keep `account` in the workspace's accounts, added or in place of the one with its id.
    ///
    /// # Errors
    /// As [`PersonServices::keep_workspace_account`].
    pub(crate) async fn keep_account(&self, account: &accounts::Account) -> Result<(), String> {
        match self {
            Self::Workspace(workspaces, workspace, acting, call) => {
                // Who acted, as they acted: the write keeps the call it serves under them, and
                // names whom the device's own writes name only where it serves none.
                let by = acting
                    .cloned()
                    .unwrap_or_else(|| workspaces.acting_or_own(None));
                workspaces
                    .keep_workspace_account((workspace, &by, *call), account)
                    .await
            }
            #[cfg(test)]
            Self::Held(held) => held
                .accounts
                .lock()
                .map_err(|_| "the accounts are poisoned".to_owned())?
                .keep(account)
                .map_err(|refusal| refusal.to_string()),
        }
    }

    /// Keep `keep` and remove `remove` in the workspace's accounts, in one change: what an
    /// action does to accounts that belong together.
    ///
    /// # Errors
    /// As [`PersonServices::keep_workspace_accounts`].
    pub(crate) async fn keep_and_remove_accounts(
        &self,
        keep: &[accounts::Account],
        remove: &[&str],
    ) -> Result<(), String> {
        match self {
            Self::Workspace(workspaces, workspace, acting, call) => {
                // Who acted, as they acted: the write keeps the call it serves under them, and
                // names whom the device's own writes name only where it serves none.
                let by = acting
                    .cloned()
                    .unwrap_or_else(|| workspaces.acting_or_own(None));
                workspaces
                    .keep_workspace_accounts((workspace, &by, *call), keep, remove)
                    .await
            }
            #[cfg(test)]
            Self::Held(held) => {
                let mut accounts = held
                    .accounts
                    .lock()
                    .map_err(|_| "the accounts are poisoned".to_owned())?;
                for id in remove {
                    accounts.remove(&accounts::AccountId::from((*id).to_owned()));
                }
                for account in keep {
                    accounts
                        .keep(account)
                        .map_err(|refusal| refusal.to_string())?;
                }
                drop(accounts);
                Ok(())
            }
        }
    }
}

impl PersonServices {
    /// `workspace`'s vault, for code that carries it beside what else it reaches.
    pub(crate) fn vault_at<'a>(&'a self, workspace: &'a str) -> VaultAt<'a> {
        VaultAt::Workspace(self, workspace, None, None)
    }

    /// `workspace`'s vault whose writes are made as `acting`: the person that asked, or the
    /// agent acting for one, so every holder decides each write as that invoker.
    pub(crate) fn vault_for<'a>(&'a self, workspace: &'a str, acting: &'a Acting) -> VaultAt<'a> {
        VaultAt::Workspace(self, workspace, Some(acting), None)
    }

    /// [`Self::vault_for`], each write keeping `call` when one is named ([`VaultAt::serving`]).
    pub(crate) fn vault_serving<'a>(
        &'a self,
        workspace: &'a str,
        acting: &'a Acting,
        call: Option<&'a InvokeRequest>,
    ) -> VaultAt<'a> {
        VaultAt::Workspace(self, workspace, Some(acting), call)
    }
}

impl PersonServices {
    /// Carry what this host kept of each of its workspaces' vaults before into their vault
    /// documents, once, as it starts ([`Self::carry_over`]): no read of a vault converts
    /// anything. A workspace whose keys are not here yet keeps its old store until a start
    /// that holds them.
    pub async fn carry_vaults_over(&self) {
        let hosted: Vec<String> = self.ids().await.iter().map(ToString::to_string).collect();
        for workspace in hosted {
            let kept = accounts::vault_dir_in(&self.workspace_root(&workspace)).exists();
            if !kept {
                continue;
            }
            let carried = match self.open_vault(&workspace).await {
                Ok(opened) => Box::pin(self.carry_over(opened)).await,
                Err(error) => Err(error),
            };
            if let Err(error) = carried {
                tracing::info!(%workspace, %error, "what the host kept of the workspace's vault stays until a start that can carry it over");
            }
        }
    }

    /// Carry what this host kept of `opened`'s vault in its own sealed store, before the
    /// workspace's vault was the document, into the document, once after each start: every item
    /// and entry the document does not keep yet, with its uses, and every account's and
    /// provider's sign-in, as that account's or provider's own entry, which its settings then
    /// name ([`HostSignIns`]). Only once they are written are they taken out of the host's
    /// store, and the store goes, so a failed write loses nothing and is carried at the next
    /// start.
    ///
    /// # Errors
    /// The host's store does not open, or what it kept was not written: it stays.
    async fn carry_over(&self, opened: OpenVault) -> Result<(), String> {
        let workspace = opened.workspace.clone();
        let root = self.workspace_root(&workspace);
        let Some(sign_ins) =
            HostSignIns::under(&root, &self.workspace_accounts(&workspace).await?.all())?
        else {
            return Ok(());
        };
        let kept = accounts::vault_items::KeptBefore::under(&root).map_err(|error| {
            format!("what the host kept of the workspace's vault before does not open: {error}")
        })?;
        let mut items = opened.items();
        let named = sign_ins.keep_in(&mut items)?;
        let mut changes = items.take_changes();
        for (entry, uses) in kept.entries() {
            if opened.entry(&entry.id.0).is_some() {
                continue;
            }
            changes.push(VaultChange::Keep(entry.clone()));
            if !uses.is_empty() {
                changes.push(VaultChange::Uses(entry.id.clone(), uses.clone()));
            }
        }
        let carried = changes.len();
        let nothing = kept.is_empty() && sign_ins.is_empty();
        // A few at a time: one write of a document carries a bounded number of bytes.
        let mut opened = opened;
        let mut changes = changes.into_iter().peekable();
        while changes.peek().is_some() {
            let batch: Vec<VaultChange> = changes.by_ref().take(CARRIED_PER_WRITE).collect();
            self.write_vault(&opened, batch, None, None).await?;
            opened = self.read_vault(&workspace, opened.keys).await?;
        }
        kept.forget().map_err(|error| error.to_string())?;
        sign_ins.carry((self, &workspace), &named).await?;
        if !nothing {
            tracing::info!(%workspace, carried, "carried what the host kept of the workspace's vault into the vault document");
        }
        Ok(())
    }
}
