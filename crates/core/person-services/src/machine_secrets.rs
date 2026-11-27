//! The secrets a workspace's Linux machine hands the programs it starts: the items of the
//! workspace's vault Terminal's machine secrets name (its `MachineSecret` records, which its
//! `add_secret` and `remove_secret` change), each released for whoever starts the program.
//!
//! [`Workspaces::workspace_secret`] is the one place an item becomes its secret for the
//! machine. Whether the one starting the program may read the workspace's vault is decided by
//! the workspace's policy for them, as for any reader of the vault's document; the item is read
//! from that document as this server holds it ([`tilo_workspace_service::workspaces::vault`]), an item the vault does not
//! hold is refused, and nothing is read from anywhere else.

use accounts::vault_items::{ItemError, ItemId, UsedFor};
use collab_types::store::Query;
use tilo_action_services::environment_commands::VmSecret;
use tilo_actions::remote::Caller;
use tilo_workspace_records::vault::ItemRef;
use zeroize::Zeroizing;

/// What a use of an item for the machine is written down as.
const USING: &str = "tilo.terminal/machine-secret";

/// Terminal's record naming a vault item the machine's programs get, by its kind.
const MACHINE_SECRET: &str = "tl.so.tilo.terminal.MachineSecret";

/// How many records one page of the store's query holds: every page is read.
const PAGE: usize = 256;

impl crate::PersonServices {
    /// The variables a program `by` starts in `workspace`'s machine gets, each with its secret:
    /// read once, when the program starts. None when the workspace names no secrets.
    ///
    /// # Errors
    ///
    /// The records cannot be read, `by` may not read the workspace's vault, or the vault does
    /// not hold an item a record names: the program is not started without it.
    pub async fn machine_secrets(
        &self,
        workspace: &str,
        by: &Caller,
    ) -> Result<Vec<VmSecret>, String> {
        let named = self
            .store_found(workspace, Query::of(MACHINE_SECRET, PAGE))
            .await?;
        let mut secrets = Vec::with_capacity(named.len());
        for (_, held) in named {
            let text = |field: &str| held.scalar(field).as_text().map(str::to_owned);
            let (Some(variable), Some(item)) = (text("variable"), text("item")) else {
                continue;
            };
            let item = ItemRef::parse(&item).ok_or_else(|| ItemError::NotFound.to_string())?;
            let value = self
                .workspace_secret(workspace, &item, by, &variable)
                .await?;
            secrets.push(VmSecret {
                variable,
                value: value.to_string(),
            });
        }
        secrets.sort_by(|a, b| a.variable.cmp(&b.variable));
        Ok(secrets)
    }

    /// The secret the item `item` of `workspace`'s vault keeps, released for `by` and written
    /// down as handed to the machine in `variable`.
    ///
    /// # Errors
    ///
    /// The workspace's policy does not let `by` read its vault, or the vault does not hold the
    /// item.
    pub async fn workspace_secret(
        &self,
        workspace: &str,
        item: &ItemRef,
        by: &Caller,
        variable: &str,
    ) -> Result<Zeroizing<String>, String> {
        let policy = self
            .policy_of(workspace)
            .await
            .ok_or("this device does not have that workspace")?;
        if !tilo_workspace_service::workspaces::vault::may_read_vault(&policy, workspace, by) {
            return Err(
                "Whoever started this may not read the workspace's vault, so its secrets stay in it"
                    .to_owned(),
            );
        }
        let id = ItemId::parse(item.id()).ok_or_else(|| ItemError::NotFound.to_string())?;
        let by_name = by
            .agent_called()
            .map_or_else(String::new, ToOwned::to_owned);
        let what = format!("handed to the workspace's machine in {variable}");
        // The workspace's one vault, as this server holds it: the item is used wherever it
        // belongs, and its use is written down before its secret is handed to the machine.
        self.changing_vault(workspace, (Some(by.acting()), None), |items| {
            let facts = items
                .facts()
                .into_iter()
                .find(|facts| facts.id == id)
                .ok_or(ItemError::NotFound)?;
            items.use_for(
                &id,
                facts.kind,
                &facts.places,
                &by_name,
                UsedFor {
                    using: USING,
                    what: &what,
                    allowed: None,
                },
            )
        })
        .await
        .map(|released| released.secret)
    }
}
