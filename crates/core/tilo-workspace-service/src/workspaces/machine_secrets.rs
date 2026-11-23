//! The secrets a workspace's Linux machine hands the programs it starts: the items of the
//! workspace's vault its machine secrets document names
//! ([`tilo_environment::machine_secrets`]), each released for whoever starts the program.
//!
//! [`Workspaces::workspace_secret`] is the one place an item becomes its secret for the
//! machine. Whether the one starting the program may read the workspace's vault is decided by
//! the workspace's policy for them, as for any reader of the vault's document; the item is read
//! from that document as this server holds it ([`super::vault`]), an item the vault does not
//! hold is refused, and nothing is read from anywhere else.

use accounts::vault_items::{ItemError, ItemId, UsedFor};
use tilo_actions::remote::Caller;
use tilo_actions::remote::environment_commands::VmSecret;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_environment::machine_secrets::{DOCUMENT, MachineSecrets};
use tilo_resource::Resource as _;
use zeroize::Zeroizing;

use super::Workspaces;

/// What a use of an item for the machine is written down as.
const USING: &str = "tilo.terminal/machine-secret";

impl Workspaces {
    /// The variables a program `by` starts in `workspace`'s machine gets, each with its secret:
    /// read once, when the program starts. None when the workspace names no secrets.
    ///
    /// # Errors
    ///
    /// The document cannot be read, `by` may not read the workspace's vault, or the vault does
    /// not hold an item the document names: the program is not started without it.
    pub async fn machine_secrets(
        &self,
        workspace: &str,
        by: &Caller,
    ) -> Result<Vec<VmSecret>, String> {
        let Some(saved) = self
            .resource_document(workspace, tilo_app_id::apps::TERMINAL, DOCUMENT)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(Vec::new());
        };
        let doc = automerge::AutoCommit::load(&saved).map_err(|error| error.to_string())?;
        let named = MachineSecrets::read(&doc).map_err(|error| error.to_string())?;
        let mut secrets = Vec::with_capacity(named.secrets.len());
        for secret in named.secrets {
            let item =
                ItemRef::parse(&secret.item).ok_or_else(|| ItemError::NotFound.to_string())?;
            let value = self
                .workspace_secret(workspace, &item, by, &secret.variable)
                .await?;
            secrets.push(VmSecret {
                variable: secret.variable,
                value: value.to_string(),
            });
        }
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
            .ok_or("this server does not hold that workspace")?;
        if !super::vault::may_read_vault(&policy, workspace, by) {
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
        self.changing_vault(workspace, Some(&by.acting()), |items| {
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
