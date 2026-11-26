//! The workspace's vault, as the device this service runs on holds it.
//!
//! A workspace keeps one vault: Passwords' vault document
//! ([`VAULT_RECORD`]). Its entries are sealed under the document's own keys,
//! which every device the workspace's policy lets read Passwords holds: a member's laptop, a
//! page, and a server, which is a device that stores more and holds them through the grant its
//! administrator membership is. Whatever this service needs a secret for (a mail account it signs
//! in to, a provider's key, a program the workspace's machine starts, an agent's turn) reads it
//! here, from this device's own copy of the document, and asks no other device for it.
//!
//! Reading opens each entry with the document's keys ([`OpenVault::entries`]); every change is
//! sealed with them and written as this device's own write of the document
//! ([`Workspaces::write_vault`]), which syncs to every other holder as any member's write does.
//! A use of an entry is written down in its uses beside it, never into the entry, so a use and a
//! member's edit of the entry at the same time keep both.
//!
//! A write made for a call keeps that call: as its caller signed it, or, for an agent's call
//! nobody signed, as this host signs it as the device that carried it out. An agent's call keeps
//! how it was allowed beside it. Every device reading the write admits it as a step the called
//! action declares, decided when the call was allowed, and never again as the agent's edit of
//! the vault's record ([`Workspaces::write_vault`]).
//!
//! Who may read what is not decided here: the device reads the document as the policy lets it,
//! and each caller is decided by the policy for its own acting before anything it asked for is
//! read ([`super::person_vault`], [`super::machine_secrets`], [`super::hosted_use`]).

use super::vault_entries::{WorkspaceVaultEntries, uses_field};
use accounts::vault_items::{UseRecord, VaultChange, VaultItems};
use accounts::{SecretId, SecretItem};
use tilo_action_policy::Acting;
use tilo_actions::remote::VAULT_RECORD;
use tilo_actions::remote::{CallAllowed, InvokeRequest, SignedCall};
use tilo_resource::{AppNamed, Id, Replica, Resource as _};
use zeroize::Zeroizing;

use super::Workspaces;
use super::account_logins::VAULT_NOT_HERE;
#[cfg(feature = "device-keys")]
use super::person_settings::kept_sign_ins::HostSignIns;

/// The app that keeps the vault: whose grants decide each reader of it.
pub(crate) const APP: &str = tilo_app_id::apps::PASSWORDS;

/// The most bytes of the vault document one read takes.
const MAX_VAULT_BYTES: usize = 64 * 1024 * 1024;

/// How many changes one write carries when a host carries its earlier vault over: each write of
/// a document is bounded in size.
#[cfg(feature = "device-keys")]
const CARRIED_PER_WRITE: usize = 16;

/// The workspace's vault as this device read it: the document, its keys, and its entries
/// opened with them.
pub(crate) struct OpenVault {
    workspace: String,
    keys: key_group::ContentKeys,
    /// The document as it was read: `None` where nobody has made it yet.
    #[cfg(test)]
    document: Option<Vec<u8>>,
    entries: Vec<(SecretItem, Vec<UseRecord>)>,
}

impl OpenVault {
    /// The document as it was read, where somebody made it.
    #[cfg(test)]
    pub(crate) fn document(&self) -> Option<&[u8]> {
        self.document.as_deref()
    }

    /// Every entry of the vault, each with the uses kept beside it.
    pub(crate) fn entries(&self) -> &[(SecretItem, Vec<UseRecord>)] {
        &self.entries
    }

    /// The vault's entries read as items: what signs in where.
    pub(crate) fn items(&self) -> VaultItems {
        VaultItems::of_entries(self.entries.iter().cloned())
    }

    /// The entry `id` names, when the vault keeps it.
    pub(crate) fn entry(&self, id: &str) -> Option<&SecretItem> {
        self.entries
            .iter()
            .map(|(entry, _)| entry)
            .find(|entry| entry.id.0 == id)
    }
}

impl std::fmt::Debug for OpenVault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenVault")
            .field("workspace", &self.workspace)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

/// The route the vault's document travels on in `workspace`.
fn route(workspace: &str) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.to_owned(),
        cluster_id: workspace.to_owned(),
        document: VAULT_RECORD.to_owned(),
    }
}

impl Workspaces {
    /// `workspace`'s vault, opened on this device with the document's own keys.
    ///
    /// # Errors
    /// This service does not hold the workspace, the policy does not let its device read
    /// Passwords now, its device holds no keys of the vault, or the document does not read.
    pub(crate) async fn open_vault(&self, workspace: &str) -> Result<OpenVault, String> {
        self.opened_vault(workspace, true)
            .await?
            .ok_or_else(|| VAULT_NOT_HERE.to_owned())
    }

    /// `workspace`'s vault as [`Self::open_vault`] opens it, but without making its keys where
    /// nobody made them yet: `None` when it has none here, so nothing is kept in it. A read that
    /// only looks for what is kept writes nothing.
    ///
    /// # Errors
    /// As [`Self::open_vault`], except that no keys here is `None`.
    pub(crate) async fn read_made_vault(
        &self,
        workspace: &str,
    ) -> Result<Option<OpenVault>, String> {
        self.opened_vault(workspace, false).await
    }

    async fn opened_vault(
        &self,
        workspace: &str,
        making: bool,
    ) -> Result<Option<OpenVault>, String> {
        // The policy decides as the vault is read: keys this device was handed before its access
        // was taken away still open what was sealed with them, and they open nothing here.
        let state = self
            .state(workspace)
            .await
            .ok_or_else(|| VAULT_NOT_HERE.to_owned())?;
        if let Err(reason) = state.check_document_read(
            self.inner.host.cert.device_public_key,
            APP,
            VAULT_RECORD,
            tilo_clock::wall(),
        ) {
            tracing::debug!(%workspace, %reason, "the policy does not let this device read the vault");
            return Err(VAULT_NOT_HERE.to_owned());
        }
        // When `making`, the keys are made where nobody made them yet, as this device's own
        // write of any of the workspace's documents makes them.
        // The vault's steps are boxed here, once: every sign-in, use and connection awaits this,
        // and inlined their state would make each of those futures as large as all of it.
        let keys = match Box::pin(self.own_route_keys(&route(workspace), making)).await {
            Some(Ok(keys)) => keys,
            Some(Err(reason)) if making => {
                tracing::debug!(%workspace, %reason, "the vault's keys are not held here");
                return Err(VAULT_NOT_HERE.to_owned());
            }
            Some(Err(_)) | None => return Ok(None),
        };
        Box::pin(self.read_vault(workspace, keys)).await.map(Some)
    }

    /// Carry what this host kept of each of its workspaces' vaults before into their vault
    /// documents, once, as it starts ([`Self::carry_over`]): no read of a vault converts
    /// anything. A workspace whose keys are not here yet keeps its old store until a start
    /// that holds them.
    #[cfg(feature = "device-keys")]
    pub(crate) async fn carry_vaults_over(&self) {
        let hosted: Vec<String> = self.inner.workspaces.lock().await.keys().cloned().collect();
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

    /// `workspace`'s vault document as this device holds it now, opened with `keys`.
    async fn read_vault(
        &self,
        workspace: &str,
        keys: key_group::ContentKeys,
    ) -> Result<OpenVault, String> {
        let document = self.vault_document(workspace).await?;
        let entries = match &document {
            Some(bytes) => open_entries(&keys, bytes)?,
            None => Vec::new(),
        };
        Ok(OpenVault {
            workspace: workspace.to_owned(),
            keys,
            #[cfg(test)]
            document,
            entries,
        })
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
    #[cfg(feature = "device-keys")]
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

    /// The vault document of `workspace` as this device reads it, `None` where nobody has made
    /// it yet.
    async fn vault_document(&self, workspace: &str) -> Result<Option<Vec<u8>>, String> {
        if self.holds_own_copy(workspace).await {
            return self
                .resource_document(workspace, APP, VAULT_RECORD)
                .await
                .map_err(|error| error.to_string());
        }
        if !self.holds_resource(workspace, APP, VAULT_RECORD) {
            return Ok(None);
        }
        let read = self
            .own_read(workspace, (APP, VAULT_RECORD), &[], MAX_VAULT_BYTES)
            .await?;
        match read {
            tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. }
            | tilo_peer_state::workspace_documents::DocumentRead::Changes { bytes, .. } => {
                Ok(Some(bytes))
            }
            tilo_peer_state::workspace_documents::DocumentRead::Absent => Ok(None),
            tilo_peer_state::workspace_documents::DocumentRead::Pending => Err(
                "The workspace's vault is still arriving on this device; try again in a moment"
                    .to_owned(),
            ),
            tilo_peer_state::workspace_documents::DocumentRead::Unchanged(_) => {
                Err("the vault's document did not read".to_owned())
            }
        }
    }

    /// Write `changes` to `opened`'s vault as this device's own write of its document, sealed
    /// under its keys: the entries kept, forgotten and used. A vault nobody has made yet is
    /// made with them, as Passwords makes it.
    ///
    /// `call` is the request the write answers, when a caller asked for it: the host that keeps
    /// the workspace's own copy writes it for them, keeping the call as they signed it, so every
    /// device reading it sees that they asked and decides the step their action declares
    /// ([`crate::workspace_vault::effects`]) as theirs. An agent's call is kept with
    /// how it was allowed, and one nobody signed (a turn this host carries out, a call it
    /// admitted again on its person's yes) is signed by this host as the device that carried it
    /// out ([`Self::carried_call`]).
    ///
    /// # Errors
    /// An entry does not seal, the document cannot be made here, or the workspace refused the
    /// write.
    pub(crate) async fn write_vault(
        &self,
        opened: &OpenVault,
        changes: Vec<VaultChange>,
        acting: Option<&Acting>,
        call: Option<&InvokeRequest>,
    ) -> Result<(), String> {
        if changes.is_empty() {
            return Ok(());
        }
        // The workspace's own copy is this service's: it writes the record for a caller who
        // asked, keeping their call. A member's device writes its own person's change: it keeps
        // no record for anyone else, so no call it was asked by goes with it.
        let served = match call {
            Some(request) if self.holds_own_copy(&opened.workspace).await => {
                let used: Vec<&str> = changes
                    .iter()
                    .filter_map(|change| match change {
                        VaultChange::Uses(SecretId(id), _) => Some(id.as_str()),
                        _ => None,
                    })
                    .collect();
                self.carried_call(&opened.workspace, request, acting, &used)
                    .await?
            }
            _ => None,
        };
        // A caller whose call the write keeps is named as they asked; without it, the write
        // names whom this device may write for.
        let acting = match (&served, acting) {
            (Some(_), Some(acting)) => acting.clone(),
            _ => self.acting_or_own(acting),
        };
        let device = self.signing_device().device();
        let keys = &opened.keys;
        Box::pin(self.author_record(
            &opened.workspace,
            (APP, WorkspaceVaultEntries::KIND, VAULT_RECORD),
            |doc| {
                let mut replica = in_passwords(Replica::from_doc(std::mem::take(doc)))
                    .as_invoker_acting(acting.clone());
                if let Some((invocation, signed)) = served.clone() {
                    replica = replica.serving(invocation, signed);
                }
                // This device's own actor: two devices continuing the record under one actor
                // would make one sequence with two different changes.
                replica
                    .doc_mut()
                    .set_actor(automerge::ActorId::from(device.to_vec()));
                let applied = apply_changes(&mut replica, keys, changes);
                std::mem::swap(doc, replica.doc_mut());
                applied
            },
        ))
        .await
    }

    /// What a vault write keeps of `request`, the call it answers for `acting`: the call as its
    /// caller signed it. An agent's call that nobody signed (a turn this host carries out, a
    /// call it admitted again on its person's yes) is signed here, by this host as the device
    /// that carries it out for the agent's person: an honest record of who did it, which names
    /// the agent and never the person as its maker. With an agent's call goes how it was allowed:
    /// the asked record its person approved, or the lasting grant that let the agent make it
    /// unasked, which names no resource or an item in `used`, the items whose use the write
    /// writes down, when it writes any. `None` for a call nobody signed that no agent made.
    ///
    /// # Errors
    /// The call does not sign here.
    pub(crate) async fn carried_call(
        &self,
        workspace: &str,
        request: &InvokeRequest,
        acting: Option<&Acting>,
        used: &[&str],
    ) -> Result<Option<(tilo_actions::InvocationId, SignedCall)>, String> {
        let agent = acting
            .filter(|acting| acting.acted_for_person().is_some())
            .and_then(Acting::member_id)
            .filter(|member| member.is_agent());
        // A call signed as its acting would sign it is kept as it was signed.
        let signed_as_acting = request.signed.as_deref().is_some_and(|signed| {
            acting.is_none_or(|acting| tilo_server_protocol::invocation::signs_for(acting, signed))
        });
        // A call nobody signed that this host carries out for its own person — a cloud mount's
        // ask on the person's own connection, a sign-in the host finishes after the provider's
        // page approved it — is signed here, as the device that carried it out, so every device
        // reading the write sees who asked.
        let own_person = hex::encode(self.inner.host.cert.user_public_key);
        let ours = request.signed.is_none()
            && acting.is_none_or(|acting| acting.people().contains(&own_person.as_str()));
        let served = served_call(request).filter(|_| signed_as_acting || agent.is_none());
        let mut kept = match served {
            Some(kept) => kept,
            None if agent.is_some() || ours => {
                let signed = tilo_server_protocol::invocation::sign_invocation_now(
                    workspace,
                    request,
                    &self.inner.host.cert,
                    &self.inner.host.key,
                    tilo_clock::unix_now_ns(),
                )
                .map_err(|error| error.to_string())?;
                let mut call = SignedCall::with(request, signed);
                call.argument = None;
                (request.invocation, call)
            }
            None => return Ok(None),
        };
        if let Some(agent) = agent {
            kept.1.allowed = match CallAllowed::asked(request) {
                Some(asked) => Some(asked),
                None => self
                    .lasting_grant(workspace, agent, &request.target, used)
                    .await
                    .map(CallAllowed::Granted),
            };
        }
        Ok(Some(kept))
    }

    /// The lasting grant of `workspace`'s that lets `agent` make the call `target` names unasked,
    /// by its id: one naming the agent, live, allowing that action of that app, and naming no
    /// resource or, where `used` names the items the write uses, one of them.
    async fn lasting_grant(
        &self,
        workspace: &str,
        agent: &tilo_action_policy::MemberId,
        target: &tilo_actions::remote::RemoteActionTarget,
        used: &[&str],
    ) -> Option<String> {
        use tilo_action_policy::Level;
        let policy = self.policy_of(workspace).await?;
        let cx = tilo_action_policy::Context::at(tilo_clock::unix_now_ms());
        let grantee = tilo_action_policy::Grantee::Member(agent.clone());
        policy
            .grants
            .iter()
            .find(|grant| {
                let node = &grant.node;
                grant.grantee == grantee
                    && grant.answer == tilo_action_policy::Answer::Allow
                    && grant.is_live(&cx)
                    && node.id_at(Level::App) == Some(target.provider.as_str())
                    && node.id_at(Level::Action) == Some(target.address.action.as_str())
                    && node.id_at(Level::Resource).is_none_or(|resource| {
                        used.is_empty()
                            || used.iter().any(|item| {
                                resource == *item
                                    || resource
                                        .strip_prefix(*item)
                                        .is_some_and(|rest| rest.starts_with('/'))
                            })
                    })
            })
            .map(|grant| grant.id.0.clone())
    }

    /// `document`, `source`'s vault forked into `branch`, with each entry and its uses sealed
    /// again under the branch's vault keys. They were sealed under the source's, which none of
    /// the branch's devices are handed unless they read the source too: forked as they were, the
    /// branch's vault opened to nothing. Who reads the branch's vault is the branch's policy, as
    /// for any of its documents. A record that is not the vault is left as it is.
    ///
    /// # Errors
    /// Either vault's keys are not held here, or an entry does not open with the source's.
    #[cfg(feature = "agents")]
    pub(crate) async fn reseal_branched_vault(
        &self,
        (source, branch): (&str, &str),
        (app, record): (&str, &str),
        document: &mut Vec<u8>,
    ) -> Result<(), String> {
        if (app, record) != (APP, VAULT_RECORD) {
            return Ok(());
        }
        let keys = |workspace: &str, create: bool| {
            let route = route(workspace);
            async move {
                Box::pin(self.own_route_keys(&route, create))
                    .await
                    .unwrap_or_else(|| Err("they are not held here".to_owned()))
            }
        };
        let from = keys(source, false)
            .await
            .map_err(|why| format!("the source's vault keys: {why}"))?;
        let to = keys(branch, true)
            .await
            .map_err(|why| format!("the branch's vault keys: {why}"))?;
        // The host carries the vault over of itself: its changes name the person it acts for, as
        // every change names who made it.
        let mut replica = in_passwords(
            Replica::<WorkspaceVaultEntries>::load(document).map_err(|error| error.to_string())?,
        )
        .as_invoker_acting(self.acting_or_own(None));
        replica.doc_mut().set_actor(automerge::ActorId::from(
            self.inner.host.cert.device_public_key.to_vec(),
        ));
        let state = replica.state().map_err(|error| error.to_string())?;
        let mut changes = Vec::new();
        for (id, sealed) in state.entries() {
            let entry = open_entry(&from, id, &sealed.0)?;
            changes.push(VaultChange::Keep(entry));
        }
        for (id, sealed) in state.used.iter() {
            let payload = from
                .open_private_field(&uses_field(id.as_str()), &sealed.0)
                .map_err(|error| error.to_string())?;
            let uses: Vec<UseRecord> =
                serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
            changes.push(VaultChange::Uses(SecretId(id.as_str().to_owned()), uses));
        }
        apply_changes(&mut replica, &to, changes)?;
        *document = replica.doc_mut().save();
        Ok(())
    }

    /// Whether this service keeps `workspace`'s own copy, hosting it, rather than a member's.
    pub(crate) async fn holds_own_copy(&self, workspace: &str) -> bool {
        match collab_types::WorkspaceId::parse(workspace) {
            Some(id) => self.hosts_here(id).await,
            None => false,
        }
    }

    /// Who a write this device makes names as its invoker: the caller that asked, or, for a
    /// write the host makes of itself (a carried-over entry, a watch's use), the person this
    /// device acts for. Every change names who made it.
    ///
    /// An agent that asked under a key of its own is shown asking only by an invocation it
    /// signed, which a change written here does not keep, so every receiver refuses a change
    /// naming it. Its change names the person it works for, on whose word it asked.
    pub(crate) fn acting_or_own(&self, acting: Option<&Acting>) -> Acting {
        match acting {
            Some(by) if by.is_agent() && !by.handed => by
                .acted_for_person()
                .map_or_else(|| by.clone(), |person| Acting::person(person.to_owned())),
            Some(by) => by.clone(),
            None => Acting::person(hex::encode(self.signing_device().cert.user_public_key)),
        }
    }

    /// Open `workspace`'s vault, let `change` read and change its items, and write what it
    /// changed before answering what it answered: the one way a secret leaves the vault for code
    /// on this device, with its use written down first.
    ///
    /// # Errors
    /// The vault does not open here, `change` refused, or what it changed was not written: then
    /// nothing it released is answered.
    pub async fn changing_vault<T>(
        &self,
        workspace: &str,
        (acting, call): (Option<&Acting>, Option<&InvokeRequest>),
        change: impl FnOnce(&mut VaultItems) -> Result<T, accounts::vault_items::ItemError>,
    ) -> Result<T, String> {
        let opened = self.open_vault(workspace).await?;
        let mut items = opened.items();
        let answer = change(&mut items).map_err(|error| error.to_string())?;
        self.write_vault(&opened, items.take_changes(), acting, call)
            .await?;
        Ok(answer)
    }

    /// The code of this moment from the code generator the item `reference` names
    /// (`vault.item:<id>`): computed where the seed is, writing nothing down, so a view shows it
    /// and no record holds it.
    ///
    /// # Errors
    /// The vault does not open here, no such item, or it is no code generator.
    pub async fn vault_code(
        &self,
        workspace: &str,
        reference: &str,
    ) -> Result<accounts::vault_items::Code, String> {
        let id = reference
            .strip_prefix("vault.item:")
            .and_then(accounts::vault_items::ItemId::parse)
            .ok_or_else(|| "an item is named by its reference, `vault.item:<id>`".to_owned())?;
        let opened = self.open_vault(workspace).await?;
        opened
            .items()
            .code_for(&id)
            .map_err(|error| error.to_string())
    }
}

/// Where code outside the workspaces' own (a connection's sign-in) reaches a workspace's vault:
/// the vault of one workspace this device holds, or, in a test of that code alone, items in
/// hand.
#[derive(Clone, Copy)]
pub(crate) enum VaultAt<'a> {
    /// `workspace`'s vault on this device, its writes made as the acting that asked for them
    /// when one is named ([`Workspaces::vault_for`]), each keeping the call it serves when one
    /// is named ([`VaultAt::serving`]).
    Workspace(
        &'a Workspaces,
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
    /// answering ([`Workspaces::changing_vault`]).
    ///
    /// # Errors
    /// As [`Workspaces::changing_vault`].
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
    /// ([`Workspaces::workspace_accounts`]).
    ///
    /// # Errors
    /// As [`Workspaces::workspace_accounts`].
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
    /// As [`Workspaces::keep_workspace_account`].
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
    /// As [`Workspaces::keep_workspace_accounts`].
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

impl Workspaces {
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

/// What a vault write keeps of `request`, the call it answers: the invocation and the call as
/// its caller signed it; `None` for a call nobody signed. What the call was asked with is left
/// out: an item's secret travels in with it, and the vault keeps a secret sealed or not at all.
/// The signature covers the argument's hash, so the call still says what was asked.
pub(super) fn served_call(
    request: &InvokeRequest,
) -> Option<(tilo_actions::InvocationId, tilo_actions::remote::SignedCall)> {
    let mut signed = tilo_actions::remote::SignedCall::of(request)?;
    signed.argument = None;
    Some((request.invocation, signed))
}

/// `replica` writing as Passwords does: each change claims Passwords' verb, which every member
/// decides against Passwords' surface.
fn in_passwords(
    replica: Replica<WorkspaceVaultEntries>,
) -> Replica<WorkspaceVaultEntries, AppNamed> {
    replica.in_app(APP, "Passwords")
}

/// Apply `changes` to `replica`, each entry and its uses sealed under `keys`.
fn apply_changes(
    replica: &mut Replica<WorkspaceVaultEntries, AppNamed>,
    keys: &key_group::ContentKeys,
    changes: Vec<VaultChange>,
) -> Result<(), String> {
    for change in changes {
        let action = match change {
            VaultChange::Keep(entry) => {
                let sealed = seal_entry(keys, &entry)?;
                WorkspaceVaultEntries::KEEP_SEALED.with((entry.id.0.clone(), sealed))
            }
            VaultChange::Forget(SecretId(id)) => WorkspaceVaultEntries::REMOVE_SEALED.with(id),
            VaultChange::Uses(SecretId(id), uses) => {
                let payload =
                    Zeroizing::new(serde_json::to_vec(&uses).map_err(|error| error.to_string())?);
                let sealed = keys
                    .seal_private_field(&uses_field(&id), &payload)
                    .map_err(|error| error.to_string())?;
                WorkspaceVaultEntries::KEEP_USES.with((id, sealed))
            }
        };
        replica
            .apply_confirmed(action)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn open_entries(
    keys: &key_group::ContentKeys,
    bytes: &[u8],
) -> Result<Vec<(SecretItem, Vec<UseRecord>)>, String> {
    let state = Replica::<WorkspaceVaultEntries>::load(bytes)
        .and_then(|replica| replica.state())
        .map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for (id, sealed) in state.entries() {
        let entry = match open_entry(keys, id, &sealed.0) {
            Ok(entry) => entry,
            Err(reason) => {
                // An entry sealed under an epoch this device never held is passed over, not the
                // whole vault: the others still read.
                tracing::debug!(entry = %id.as_str(), %reason, "a vault entry did not open");
                continue;
            }
        };
        let uses = state
            .used
            .get(id)
            .map(|sealed| {
                let payload = keys
                    .open_private_field(&uses_field(id.as_str()), &sealed.0)
                    .map_err(|error| error.to_string())?;
                serde_json::from_slice::<Vec<UseRecord>>(&payload)
                    .map_err(|error| error.to_string())
            })
            .transpose()
            .unwrap_or_else(|reason| {
                // Uses sealed under an epoch this device never held, or written in a shape it
                // does not read, are passed over as the entry they belong to would be: the
                // entry itself still reads.
                tracing::debug!(entry = %id.as_str(), %reason, "a vault entry's uses did not open");
                None
            })
            .unwrap_or_default();
        entries.push((entry, uses));
    }
    Ok(entries)
}

/// The entry `sealed` holds at `id`, opened with `keys`: what a Passwords window opens it to.
fn open_entry(key: &key_group::ContentKeys, id: &Id, sealed: &str) -> Result<SecretItem, String> {
    let payload = key
        .open_private_field(id.as_str(), sealed)
        .map_err(|error| error.to_string())?;
    let entry: SecretItem =
        serde_json::from_slice(&payload).map_err(|_| "invalid encrypted vault item".to_owned())?;
    if entry.id.0 != id.as_str() {
        return Err("vault item identifier mismatch".to_owned());
    }
    Ok(entry)
}

/// `entry` sealed at its id with `keys`, as a Passwords window seals it.
fn seal_entry(keys: &key_group::ContentKeys, entry: &SecretItem) -> Result<String, String> {
    let payload =
        Zeroizing::new(serde_json::to_vec(entry).map_err(|_| "invalid vault item".to_owned())?);
    keys.seal_private_field(&entry.id.0, &payload)
        .map_err(|error| error.to_string())
}

/// Whether the workspace's policy lets `by` read the workspace's vault document, the node every
/// reader of it is decided on: for `by`'s whole acting. Only the machine secrets a workspace's
/// programs are started with ask this.
#[cfg(feature = "agents")]
pub(crate) fn may_read_vault(
    policy: &tilo_action_policy::WorkspacePolicy,
    workspace: &str,
    by: &tilo_actions::remote::Caller,
) -> bool {
    let acting = by.acting();
    let mut granted = acting.handed_preset(workspace);
    granted.extend(by.granted());
    let node = collab_types::document_node(workspace, APP, VAULT_RECORD);
    let cx = tilo_action_policy::Context::at_time(tilo_clock::wall());
    tilo_action_policy::Authority::of_acting(acting, policy, &granted)
        .decide(&node, tilo_action_policy::ActionKind::Read, &cx)
        .answer
        == tilo_action_policy::Answer::Allow
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::workspaces::fixtures::{hosting_device, test_host};

    /// What this device keeps in the vault it reads back, opened under the document's keys,
    /// with a use written beside it; what it forgets is gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn what_a_device_keeps_in_the_vault_it_reads_back() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let server = test_host(scratch.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Vault").await.unwrap().workspace;

        let opened = workspaces.open_vault(&workspace).await.unwrap();
        assert!(opened.entries().is_empty());
        let mut entry = SecretItem::login("Example", "ada", "correct horse battery staple");
        entry.add_origin("https://example.test").unwrap();
        let use_record = UseRecord {
            at_unix: 7,
            places: Vec::new(),
            by: "Assistant".to_owned(),
            using: String::new(),
            what: String::new(),
            allowed: None,
        };
        workspaces
            .write_vault(
                &opened,
                vec![
                    VaultChange::Keep(entry.clone()),
                    VaultChange::Uses(entry.id.clone(), vec![use_record.clone()]),
                ],
                None,
                None,
            )
            .await
            .unwrap();
        let again = workspaces.open_vault(&workspace).await.unwrap();
        assert_eq!(again.entries().len(), 1, "{again:?}");
        assert_eq!(again.entries()[0].0, entry);
        assert_eq!(again.entries()[0].1, vec![use_record]);

        workspaces
            .write_vault(
                &again,
                vec![VaultChange::Forget(entry.id.clone())],
                None,
                None,
            )
            .await
            .unwrap();
        assert!(
            workspaces
                .open_vault(&workspace)
                .await
                .unwrap()
                .entries()
                .is_empty()
        );
        server.shutdown().await;
    }

    /// The vault is the system's record on every device: installing Passwords, whose package
    /// declares its own records, keeps the vault's writes admitted, and a package that names the
    /// vault's type itself does not declare it, so no package judges the vault's writes by its
    /// copy.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn installing_passwords_keeps_the_vault_the_systems() {
        const PASSWORDS: &str = r#"{"format":2,"resources":[{"fields":[{"path":"title","type":"text"}],"kind":"passwords.item","label":"Item","verbs":[{"id":"type","kind":"undoable"}],"version":1}]}"#;
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let server = test_host(scratch.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let surfaces = &workspaces.inner.host.surfaces;
        surfaces.register_package(APP, PASSWORDS.as_bytes());
        assert!(surfaces.hash_of(APP).is_some(), "Passwords is installed");
        assert!(
            surfaces
                .kinds_of(APP)
                .iter()
                .any(|kind| kind == WorkspaceVaultEntries::KIND),
            "the vault is the system's record of Passwords"
        );
        let squatter = PASSWORDS.replace("passwords.item", WorkspaceVaultEntries::KIND);
        surfaces.register_package("test.vault-copy", squatter.as_bytes());
        assert!(
            surfaces.hash_of("test.vault-copy").is_some(),
            "a package naming the vault's type keeps the surface its other kinds are declared by"
        );
        assert!(
            surfaces.kinds_of("test.vault-copy").is_empty(),
            "the vault's type stays the system's: a package's copy of it is no kind its store declares"
        );

        let workspace = workspaces.create("Vault").await.unwrap().workspace;
        let opened = workspaces.open_vault(&workspace).await.unwrap();
        let entry = SecretItem::login("Example", "ada", "correct horse battery staple");
        workspaces
            .write_vault(&opened, vec![VaultChange::Keep(entry.clone())], None, None)
            .await
            .unwrap();
        let again = workspaces.open_vault(&workspace).await.unwrap();
        assert_eq!(again.entries().len(), 1, "{again:?}");
        assert_eq!(again.entries()[0].0, entry);
        server.shutdown().await;
    }
}
